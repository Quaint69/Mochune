//! Suppress native non-client painting only while the app draws its own frame.
use crate::SetupWindow;
use anyhow::{Context, Result};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{ComponentHandle, winit_030::WinitWindowAccessor};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Dwm::{
        DWMNCRP_DISABLED, DWMNCRP_USEWINDOWSTYLE, DWMWA_NCRENDERING_POLICY,
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute,
    },
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow},
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, MINMAXINFO, STYLESTRUCT, SWP_FRAMECHANGED,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
            WM_DWMCOMPOSITIONCHANGED, WM_GETMINMAXINFO, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCDESTROY,
            WM_NCPAINT, WM_STYLECHANGING, WS_BORDER, WS_CAPTION, WS_EX_CLIENTEDGE,
            WS_EX_DLGMODALFRAME, WS_EX_STATICEDGE, WS_EX_WINDOWEDGE, WS_POPUP,
        },
    },
};
const ID: usize = 0x4d4f4348;
struct State {
    ui: slint::Weak<SetupWindow>,
    alive: Cell<bool>,
    custom: Cell<Option<bool>>,
}
pub(crate) struct Frame {
    hwnd: HWND,
    state: Option<Box<State>>,
}
unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    // State is stable in its Box. This subclass and its guard run on the same UI thread.
    let state = unsafe { &*(data as *const State) };
    if msg == WM_NCDESTROY {
        state.alive.set(false);
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(procedure), id) };
        return unsafe { DefSubclassProc(hwnd, msg, wp, lp) };
    }
    let custom = state
        .ui
        .upgrade()
        .is_some_and(|ui| ui.get_immersive() || ui.get_mini_music());
    if state.custom.get() != Some(custom) || msg == WM_DWMCOMPOSITIONCHANGED {
        state.custom.set(Some(custom));
        configure_compositor(hwnd, custom);
    }
    if custom {
        match msg {
            WM_GETMINMAXINFO
                if lp.0 != 0
                    && !state
                        .ui
                        .upgrade()
                        .is_some_and(|ui| ui.get_fullscreen_mode()) =>
            {
                // Keep Winit's minimum/maximum tracking constraints, then bound
                // a maximized borderless window to the monitor's work area.
                // WS_POPUP otherwise lets Windows treat maximized as fullscreen
                // and cover/intercept the taskbar on Windows 11.
                let result = unsafe { DefSubclassProc(hwnd, msg, wp, lp) };
                maximize_work_area(hwnd, lp);
                return result;
            }
            WM_STYLECHANGING if lp.0 != 0 => {
                let change = unsafe { &mut *(lp.0 as *mut STYLESTRUCT) };
                change.styleNew = borderless_style(wp.0 as i32, change.styleNew);
            }
            // Forward activation to Winit so focus/keyboard/animation state stays
            // correct. -1 tells DefWindowProc not to repaint the native frame.
            WM_NCACTIVATE => return unsafe { DefSubclassProc(hwnd, msg, wp, LPARAM(-1)) },
            WM_NCPAINT => return LRESULT(0),
            // Winit already handles the structured/maximized calculation. Its
            // simple RECT path delegates to Windows and can inset our client.
            WM_NCCALCSIZE if wp.0 == 0 => return LRESULT(0),
            _ => {}
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
}
fn maximize_work_area(hwnd: HWND, lp: LPARAM) {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if monitor.0.is_null() || !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return;
    }
    let width = info.rcWork.right - info.rcWork.left;
    let height = info.rcWork.bottom - info.rcWork.top;
    if width <= 0 || height <= 0 {
        return;
    }
    // WM_GETMINMAXINFO supplies a writable MINMAXINFO for this synchronous call.
    let limits = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
    limits.ptMaxPosition.x = info.rcWork.left - info.rcMonitor.left;
    limits.ptMaxPosition.y = info.rcWork.top - info.rcMonitor.top;
    limits.ptMaxSize.x = width;
    limits.ptMaxSize.y = height;
}
fn borderless_style(index: i32, style: u32) -> u32 {
    if index == GWL_STYLE.0 {
        (style & !(WS_CAPTION.0 | WS_BORDER.0)) | WS_POPUP.0
    } else if index == GWL_EXSTYLE.0 {
        style
            & !(WS_EX_WINDOWEDGE.0
                | WS_EX_CLIENTEDGE.0
                | WS_EX_DLGMODALFRAME.0
                | WS_EX_STATICEDGE.0)
    } else {
        style
    }
}
fn configure_compositor(hwnd: HWND, custom: bool) {
    let policy = if custom {
        DWMNCRP_DISABLED
    } else {
        DWMNCRP_USEWINDOWSTYLE
    };
    let corners = DWMWCP_DONOTROUND;
    // Older Windows versions may not support the corner preference. Both calls
    // are optional: caption styles and WM_NCPAINT suppression work without DWM.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            (&policy as *const _) as *const _,
            std::mem::size_of_val(&policy) as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&corners as *const _) as *const _,
            std::mem::size_of_val(&corners) as u32,
        );
    }
}
fn install(ui: &SetupWindow) -> Result<Frame> {
    let hwnd = ui
        .window()
        .with_winit_window(|native| {
            let RawWindowHandle::Win32(handle) = native.window_handle().ok()?.as_raw() else {
                return None;
            };
            Some(HWND(handle.hwnd.get() as *mut _))
        })
        .flatten()
        .context("Windows frame handle unavailable")?;
    let mut state = Box::new(State {
        ui: ui.as_weak(),
        alive: Cell::new(true),
        custom: Cell::new(None),
    });
    let data = (&mut *state as *mut State) as usize;
    anyhow::ensure!(
        unsafe { SetWindowSubclass(hwnd, Some(procedure), ID, data) }.as_bool(),
        "Could not install custom window frame handling"
    );
    let frame = Frame {
        hwnd,
        state: Some(state),
    };
    let custom = ui.get_immersive() || ui.get_mini_music();
    if custom {
        // Winit keeps caption bits for undecorated windows. Remove those paint
        // styles now and on future style changes, while retaining native sizing,
        // system-menu, minimize/maximize and taskbar functionality.
        for index in [GWL_STYLE, GWL_EXSTYLE] {
            let old = unsafe { GetWindowLongPtrW(hwnd, index) } as u32;
            let new = borderless_style(index.0, old);
            if old != new {
                unsafe {
                    SetWindowLongPtrW(hwnd, index, new as isize);
                }
            }
        }
    }
    configure_compositor(hwnd, custom);
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        )
    }?;
    Ok(frame)
}
impl Drop for Frame {
    fn drop(&mut self) {
        if self.state.as_ref().is_some_and(|s| s.alive.get())
            && !unsafe { RemoveWindowSubclass(self.hwnd, Some(procedure), ID) }.as_bool()
        {
            // Never free callback data while an unexpectedly retained subclass
            // can still reference it. This contains only a Weak UI handle.
            if let Some(state) = self.state.take() {
                let _ = Box::leak(state);
            }
        }
    }
}

// Winit can defer HWND creation until its event loop starts, even after show().
// Keep the subclass alive for the entire UI run, and never fail startup solely
// because the native handle has not been created yet.
pub(crate) fn install_when_ready(ui: &SetupWindow) -> Rc<RefCell<Option<Frame>>> {
    let slot = Rc::new(RefCell::new(None));
    attempt(ui.as_weak(), slot.clone(), 0);
    slot
}
fn attempt(ui: slint::Weak<SetupWindow>, slot: Rc<RefCell<Option<Frame>>>, count: u8) {
    slint::Timer::single_shot(
        Duration::from_millis(if count == 0 { 0 } else { 32 }),
        move || {
            let Some(window) = ui.upgrade() else { return };
            match install(&window) {
                Ok(frame) => {
                    *slot.borrow_mut() = Some(frame);
                    crate::repaint_window(window.window());
                }
                Err(_) if count < 8 => attempt(ui, slot, count + 1),
                Err(error) => crate::diagnostics::record(
                    "custom_frame_install_failed",
                    serde_json::json!({"error":error.to_string()}),
                ),
            }
        },
    );
}
