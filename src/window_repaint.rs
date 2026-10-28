//! Bounded full repaints after native geometry/visibility changes settle.
use crate::{SetupWindow, repaint_window};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{cell::Cell, rc::Rc, time::Duration};

pub(crate) struct RestoreRedraw {
    ui: slint::Weak<SetupWindow>,
    timer: Timer,
    epoch: Cell<u64>,
}
impl RestoreRedraw {
    pub(crate) fn new(ui: &SetupWindow) -> Rc<Self> {
        Rc::new(Self {
            ui: ui.as_weak(),
            timer: Timer::default(),
            epoch: Cell::new(0),
        })
    }
    pub(crate) fn schedule(self: &Rc<Self>) {
        let epoch = self.epoch.get().wrapping_add(1);
        self.epoch.set(epoch);
        let weak = Rc::downgrade(self);
        // Coalesce resize bursts; the event hook runs before Slint applies size.
        self.timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(80),
            move || {
                let Some(this) = weak.upgrade() else { return };
                this.paint(epoch);
                let weak = Rc::downgrade(&this);
                Timer::single_shot(Duration::from_millis(220), move || {
                    let Some(this) = weak.upgrade() else { return };
                    this.paint(epoch);
                    let weak = Rc::downgrade(&this);
                    // Refresh the other retained presentation buffer after the OS
                    // restore animation and conditional UI tree have both settled.
                    Timer::single_shot(Duration::from_millis(16), move || {
                        if let Some(this) = weak.upgrade() {
                            this.paint(epoch);
                        }
                    });
                });
            },
        );
    }
    fn paint(&self, epoch: u64) {
        if self.epoch.get() != epoch {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        let size = ui.window().size();
        if !ui.get_skin_window_active() || size.width == 0 || size.height == 0 {
            return;
        }
        repaint_window(ui.window());
    }
}
