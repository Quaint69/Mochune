//! Best-effort positioning on the current monitor. Wayland intentionally forbids it.
use crate::SetupWindow;
use slint::winit_030::winit::{
    dpi::PhysicalPosition,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};
use slint::{ComponentHandle, winit_030::WinitWindowAccessor};

pub(crate) fn anchor(w: &SetupWindow) {
    if w.window().is_maximized() || w.window().is_fullscreen() {
        return;
    }
    w.window().with_winit_window(|native| {
        let Ok(handle) = native.window_handle() else {
            return;
        };
        if matches!(handle.as_raw(), RawWindowHandle::Wayland(_)) {
            return;
        }
        let Some(monitor) = native.current_monitor() else {
            return;
        };
        let pos = monitor.position();
        let size = monitor.size();
        #[allow(unused_mut)]
        let mut area = (pos.x, pos.y, size.width as i32, size.height as i32);
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::{
                Foundation::HWND,
                Graphics::Gdi::{
                    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
                },
            };
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                let monitor = unsafe {
                    MonitorFromWindow(HWND(handle.hwnd.get() as *mut _), MONITOR_DEFAULTTONEAREST)
                };
                let mut info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
                    let r = info.rcWork;
                    area = (r.left, r.top, r.right - r.left, r.bottom - r.top);
                }
            }
        }
        let extent = native.outer_size();
        let right = (area.0 + area.2 - extent.width as i32).max(area.0);
        let bottom = (area.1 + area.3 - extent.height as i32).max(area.1);
        let target = if w.get_mini_music() {
            PhysicalPosition::new(right, bottom)
        } else if w.get_compact_music() {
            PhysicalPosition::new(right, area.1)
        } else {
            let x = native
                .outer_position()
                .map_or(area.0, |p| p.x)
                .clamp(area.0, right);
            PhysicalPosition::new(x, area.1)
        };
        if native.outer_position().ok() != Some(target) {
            native.set_outer_position(target);
        }
    });
}
