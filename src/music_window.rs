//! Independent window-mode geometry. Never sample dimensions during a transition.
use crate::{SetupWindow, repaint_window, window_position};
use slint::{ComponentHandle, LogicalSize, winit_030::WinitWindowAccessor};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Windowed,
    Maximized,
    Compact,
    Mini,
}
struct Geometry {
    windowed: LogicalSize,
    maximized: Option<LogicalSize>,
    compact: LogicalSize,
    mini: LogicalSize,
    current: Mode,
    main_return: Mode,
    mini_return: Mode,
    maximize_return: Mode,
}
impl Default for Geometry {
    fn default() -> Self {
        Self {
            windowed: LogicalSize::new(1180., 780.),
            maximized: None,
            compact: LogicalSize::new(380., 740.),
            mini: LogicalSize::new(420., 120.),
            current: Mode::Windowed,
            main_return: Mode::Windowed,
            mini_return: Mode::Windowed,
            maximize_return: Mode::Windowed,
        }
    }
}
struct Modes {
    ui: slint::Weak<SetupWindow>,
    geometry: RefCell<Geometry>,
    transitioning: Cell<bool>,
    epoch: Cell<u64>,
    observe_timer: slint::Timer,
    repaint_pending: Cell<bool>,
    reapply_on_focus: Cell<bool>,
}
impl Modes {
    fn observed(w: &SetupWindow) -> Mode {
        if w.window().is_maximized() {
            Mode::Maximized
        } else if w.get_mini_music() {
            Mode::Mini
        } else if w.get_compact_music() {
            Mode::Compact
        } else {
            Mode::Windowed
        }
    }
    fn save(g: &mut Geometry, mode: Mode, size: LogicalSize) {
        if !size.width.is_finite() || !size.height.is_finite() {
            return;
        }
        match mode {
            Mode::Windowed if size.width >= 720. && size.height >= 600. => g.windowed = size,
            Mode::Maximized => g.maximized = Some(size),
            Mode::Compact if size.width < 720. && size.height >= 600. => g.compact = size,
            Mode::Mini if size.width >= 400. && size.height >= 112. => g.mini = size,
            _ => {}
        }
    }
    fn capture(&self, w: &SetupWindow) {
        if self.transitioning.get() || w.window().is_fullscreen() || !w.get_skin_window_active() {
            return;
        }
        let observed = Self::observed(w);
        let mut g = self.geometry.borrow_mut();
        if observed == Mode::Maximized && g.current != Mode::Maximized {
            g.maximize_return = g.current;
        }
        g.current = observed;
        Self::save(
            &mut g,
            observed,
            w.window().size().to_logical(w.window().scale_factor()),
        );
    }
    fn schedule(self: &Rc<Self>) {
        // Called by Slint changed handlers: don't read layout properties on their stack.
        let weak = Rc::downgrade(self);
        self.observe_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(140),
            move || {
                if let Some(state) = weak.upgrade() {
                    state.observe();
                }
            },
        );
    }
    fn observe(self: &Rc<Self>) {
        if self.transitioning.get() {
            return;
        }
        let Some(w) = self.ui.upgrade() else {
            return;
        };
        if !w.get_music_layout() || w.window().is_fullscreen() || !w.get_skin_window_active() {
            return;
        }
        if self.reapply_on_focus.replace(false) {
            let target = self.geometry.borrow().current;
            self.transition(target);
            return;
        }
        let observed = Self::observed(&w);
        let current = self.geometry.borrow().current;
        if current == Mode::Maximized && observed != Mode::Maximized {
            let target = self.geometry.borrow().maximize_return;
            self.transition(target);
            return;
        }
        if observed == Mode::Maximized && current != Mode::Maximized {
            self.geometry.borrow_mut().maximize_return = current;
            w.set_mini_music(false);
        } else if observed != current
            && matches!(
                (current, observed),
                (Mode::Windowed, Mode::Compact) | (Mode::Compact, Mode::Windowed)
            )
        {
            if observed == Mode::Compact {
                self.geometry.borrow_mut().main_return = Mode::Windowed;
            }
            self.transition(observed);
            return;
        }
        if observed == Mode::Compact
            && w.window()
                .size()
                .to_logical(w.window().scale_factor())
                .height
                < 600.
        {
            self.transition(Mode::Compact);
            return;
        }
        self.capture(&w);
        if observed == Mode::Compact {
            let size = w.window().size().to_logical(w.window().scale_factor());
            if (400. ..600.).contains(&size.width) {
                self.geometry.borrow_mut().compact = LogicalSize::new(380., size.height);
                self.transition(Mode::Compact);
            }
        }
        if self.repaint_pending.replace(false) {
            // Resize acknowledgements and the conditional compact tree must both
            // settle before invalidating the complete restored surface.
            repaint_window(w.window());
            let weak = self.ui.clone();
            let state = Rc::downgrade(self);
            let epoch = self.epoch.get();
            slint::Timer::single_shot(Duration::from_millis(16), move || {
                if let (Some(w), Some(state)) = (weak.upgrade(), state.upgrade()) {
                    if state.epoch.get() == epoch && w.get_skin_window_active() {
                        repaint_window(w.window());
                    }
                }
            });
        }
    }
    fn apply(w: &SetupWindow, mode: Mode, size: LogicalSize) {
        if mode == Mode::Maximized {
            w.window().set_maximized(true);
        } else {
            w.window().set_maximized(false);
            w.window().set_size(size);
        }
    }
    fn transition(self: &Rc<Self>, target: Mode) {
        let Some(w) = self.ui.upgrade() else {
            return;
        };
        self.observe_timer.stop();
        self.transitioning.set(true);
        self.repaint_pending.set(true);
        let epoch = self.epoch.get().wrapping_add(1);
        self.epoch.set(epoch);
        let size = {
            let mut g = self.geometry.borrow_mut();
            g.current = target;
            match target {
                Mode::Windowed => g.windowed,
                Mode::Compact => g.compact,
                Mode::Mini => g.mini,
                Mode::Maximized => g.maximized.unwrap_or(g.windowed),
            }
        };
        if w.window().is_fullscreen() {
            w.invoke_toggle_fullscreen();
        }
        w.set_mini_music(target == Mode::Mini);
        w.window().with_winit_window(|native| {
            use slint::winit_030::winit::dpi::LogicalSize as NativeSize;
            // Apply constraints before requesting the new size, not one render
            // later when Slint publishes Window properties.
            native.set_max_inner_size(None::<NativeSize<f64>>);
            native.set_min_inner_size(Some(if target == Mode::Mini {
                NativeSize::new(420., 120.)
            } else {
                NativeSize::new(360., 600.)
            }));
            if target == Mode::Mini {
                native.set_max_inner_size(Some(NativeSize::new(420., 120.)));
            }
            native.set_resizable(target != Mode::Mini);
        });
        Self::apply(&w, target, size);
        let weak = Rc::downgrade(self);
        slint::Timer::single_shot(Duration::from_millis(80), move || {
            let Some(state) = weak.upgrade().filter(|s| s.epoch.get() == epoch) else {
                return;
            };
            if let Some(w) = state.ui.upgrade() {
                if !w.get_skin_window_active() {
                    state.transitioning.set(false);
                    state.reapply_on_focus.set(true);
                    return;
                }
                if target == Mode::Maximized {
                    if !w.window().is_maximized() {
                        Self::apply(&w, target, size);
                    }
                } else {
                    let actual = w.window().size().to_logical(w.window().scale_factor());
                    if w.window().is_maximized()
                        || (actual.width - size.width).abs() > 1.
                        || (actual.height - size.height).abs() > 1.
                    {
                        Self::apply(&w, target, size);
                    }
                }
                window_position::anchor(&w);
            }
            let weak = Rc::downgrade(&state);
            slint::Timer::single_shot(Duration::from_millis(220), move || {
                let Some(state) = weak.upgrade().filter(|s| s.epoch.get() == epoch) else {
                    return;
                };
                if let Some(w) = state.ui.upgrade() {
                    if !w.get_skin_window_active() {
                        state.transitioning.set(false);
                        state.reapply_on_focus.set(true);
                        return;
                    }
                    if Self::observed(&w) == target {
                        Self::save(
                            &mut state.geometry.borrow_mut(),
                            target,
                            w.window().size().to_logical(w.window().scale_factor()),
                        );
                    }
                    state.transitioning.set(false);
                    window_position::anchor(&w);
                    state.schedule();
                }
            });
        });
    }
}

pub(crate) fn install(window: &SetupWindow) {
    let state = Rc::new(Modes {
        ui: window.as_weak(),
        geometry: RefCell::new(Geometry::default()),
        transitioning: Cell::new(false),
        epoch: Cell::new(0),
        observe_timer: slint::Timer::default(),
        repaint_pending: Cell::new(false),
        reapply_on_focus: Cell::new(false),
    });
    let s = state.clone();
    window.on_music_resize(move || s.schedule());
    let s = state.clone();
    window.on_toggle_compact_music(move || {
        let Some(w) = s.ui.upgrade() else {
            return;
        };
        if !w.get_music_layout() {
            return;
        }
        s.capture(&w);
        let target = {
            let mut g = s.geometry.borrow_mut();
            if g.current == Mode::Compact {
                g.main_return
            } else {
                if matches!(g.current, Mode::Windowed | Mode::Maximized) {
                    g.main_return = g.current;
                }
                Mode::Compact
            }
        };
        s.transition(target);
    });
    let s = state.clone();
    window.on_toggle_mini_music(move || {
        let Some(w) = s.ui.upgrade() else {
            return;
        };
        if !w.get_music_layout() {
            return;
        }
        s.capture(&w);
        let target = {
            let mut g = s.geometry.borrow_mut();
            if g.current == Mode::Mini {
                g.mini_return
            } else {
                g.mini_return = g.current;
                Mode::Mini
            }
        };
        s.transition(target);
    });
    let minimize = state.clone();
    window.on_minimize_window(move || {
        let Some(w) = minimize.ui.upgrade() else {
            return;
        };
        minimize.capture(&w);
        if minimize.transitioning.get() {
            minimize.reapply_on_focus.set(true);
        }
        minimize.observe_timer.stop();
        // Cancel delayed geometry corrections, which would otherwise revive a
        // window minimized during a compact/mini transition.
        minimize.epoch.set(minimize.epoch.get().wrapping_add(1));
        minimize.transitioning.set(false);
        minimize.repaint_pending.set(true);
        // Wayland cannot reliably report unminimize. Always issue the native
        // request instead of setting an already-true Slint request property.
        if w.window()
            .with_winit_window(|native| native.set_minimized(true))
            .is_none()
        {
            w.window().set_minimized(true);
        }
    });
    window.on_toggle_window_maximized(move || {
        let Some(w) = state.ui.upgrade() else {
            return;
        };
        if !w.get_music_layout() {
            w.window().set_maximized(!w.window().is_maximized());
            return;
        }
        state.capture(&w);
        let target = {
            let mut g = state.geometry.borrow_mut();
            if g.current == Mode::Maximized {
                g.maximize_return
            } else {
                g.maximize_return = g.current;
                Mode::Maximized
            }
        };
        state.transition(target);
    });
}
