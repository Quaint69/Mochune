//! Native D3D12 swapchain in an owned child HWND. The desktop UI is software
//! rendered. An owned alpha window composites controls above the video.
//! There is no decoder-texture import, WGPU copy or video pixel mapping here.
use super::native_overlay::Artwork;
use super::*;
use gst_video::prelude::*;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;
use std::{
    collections::VecDeque,
    ffi::c_void,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{
            BLACK_BRUSH, CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject,
            GetStockObject, HBRUSH, HGDIOBJ, RGN_AND, SetWindowRgn,
        },
        System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
        UI::{
            Input::KeyboardAndMouse::{
                ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{s, w},
};
static PROCESS_HEALTH: Mutex<serde_json::Value> = Mutex::new(serde_json::Value::Null);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static RENDERED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UI_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UI_UPLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SESSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DISPOSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// The bundled SDK is 1.26+. Drain completed video work on the playback worker,
// never the UI thread. This does not evict the SDK's process-wide device cache.
fn flush_loaded_devices() {
    // No static D3D12 import in the desktop/music process. Only use an SDK DLL
    // already loaded by the native video plugin in the video worker.
    if let Ok(module) = unsafe { GetModuleHandleW(w!("gstd3d12-1.0-0.dll")) } {
        if let Some(function) = unsafe { GetProcAddress(module, s!("gst_d3d12_flush_all_devices")) }
        {
            // SDK 1.26 C ABI: void function(void); module remains owned by GStreamer.
            let flush: unsafe extern "C" fn() = unsafe { std::mem::transmute(function) };
            unsafe { flush() };
        }
    }
}

static CURSOR_HIDDEN: AtomicBool = AtomicBool::new(false);
fn sync_cursor(w: &SetupWindow, parent: HWND, menu: bool) {
    let hidden = w.get_page_scroll() >= 0.
        && !w.get_browsing_page()
        && !w.get_music_layout()
        && !w.get_settings_open()
        && !w.get_download_open()
        && !w.get_playlist_editor()
        && w.get_video_visible()
        && w.get_playback_active()
        && !w.get_paused()
        && !w.get_resolving()
        && w.get_controls_over_video()
        && !w.get_controls_shown()
        && !menu;
    let was_hidden = CURSOR_HIDDEN.swap(hidden, Ordering::Relaxed);
    if unsafe { GetForegroundWindow() } != parent {
        return;
    }
    let mut point = windows::Win32::Foundation::POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return;
    }
    let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(parent, &mut point) };
    let scale = w.window().scale_factor();
    let x = point.x as f32 / scale;
    let y = point.y as f32 / scale;
    let inside = x >= w.get_native_player_x()
        && x < w.get_native_player_x() + w.get_native_player_width()
        && y >= w.get_native_player_y().max(w.get_native_clip_top())
        && y < (w.get_native_player_y() + w.get_player_render_height())
            .min(w.get_native_clip_bottom());
    if hidden && inside {
        unsafe {
            SetCursor(None);
        }
    } else if was_hidden {
        unsafe {
            SetCursor(LoadCursorW(None, IDC_ARROW).ok());
        }
    }
}
static CLOSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(super) fn stats() -> serde_json::Value {
    serde_json::json!({"backend":"native D3D12 swapchain","wgpu":false,"cpu_video_readback":false,
        "active":ACTIVE.load(Ordering::Relaxed),"rendered":RENDERED.load(Ordering::Relaxed),
        "ui_artwork_bytes":UI_BYTES.load(Ordering::Relaxed),"ui_uploads":UI_UPLOADS.load(Ordering::Relaxed),
        "sessions_created":SESSIONS.load(Ordering::Relaxed),"sessions_closed":CLOSED.load(Ordering::Relaxed),
        "sinks_disposed":DISPOSED.load(Ordering::Relaxed),"sdk_device_cache":"playback process lifetime","video_process":*PROCESS_HEALTH.lock().unwrap()})
}
use super::process_protocol::Input;
#[derive(Default)]
pub(super) struct Inputs {
    pub(super) events: Mutex<VecDeque<Input>>,
    inside: AtomicBool,
}
impl Inputs {
    fn push(&self, event: Input) {
        let mut q = self.events.lock().unwrap();
        // Coalesce pointer motion; preserve click/key edges, with a hard queue cap.
        if matches!(event, Input::Move(..)) && matches!(q.back(), Some(Input::Move(..))) {
            q.pop_back();
        }
        if q.len() >= 64 {
            if let Some(i) = q.iter().position(|e| matches!(e, Input::Move(..))) {
                q.remove(i);
            } else if matches!(event, Input::Up(..) | Input::Blur | Input::Key(_, false)) {
                q.pop_front();
            } else {
                return;
            }
        }
        q.push_back(event);
    }
    unsafe fn message(&self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) {
        let x = lp.0 as u16 as i16 as i32;
        let y = (lp.0 >> 16) as u16 as i16 as i32;
        match msg {
            WM_MOUSEMOVE => {
                if !self.inside.swap(true, Ordering::Relaxed) {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = unsafe { TrackMouseEvent(&mut track) };
                }
                self.push(Input::Move(x, y));
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                unsafe { SetCapture(hwnd) };
                // Video is a mouse surface; the main Slint window owns keyboard focus.
                self.push(Input::Down(x, y));
            }
            WM_LBUTTONUP => {
                let _ = unsafe { ReleaseCapture() };
                self.push(Input::Up(x, y));
            }
            0x02A3 => {
                self.inside.store(false, Ordering::Relaxed);
                self.push(Input::Leave);
            }
            WM_KILLFOCUS => {
                // Returning keyboard focus to the main window is internal to the
                // same app. Do not close a popup between mouse-down and mouse-up.
                let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
                let next = HWND(wp.0 as *mut c_void);
                if next != root && unsafe { GetAncestor(next, GA_ROOT) } != root {
                    self.push(Input::Blur);
                }
            }
            WM_MOUSEWHEEL => self.push(Input::Wheel((wp.0 >> 16) as u16 as i16)),
            WM_KEYDOWN | WM_SYSKEYDOWN => self.push(Input::Key(wp.0 as u32, true)),
            WM_KEYUP | WM_SYSKEYUP => self.push(Input::Key(wp.0 as u32, false)),
            _ => {}
        }
    }
}
pub(super) unsafe extern "system" fn child_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    if msg == WM_SETCURSOR
        && (lp.0 as u16 as u32) == HTCLIENT
        && CURSOR_HIDDEN.load(Ordering::Relaxed)
    {
        unsafe {
            SetCursor(None);
        }
        return LRESULT(1);
    }
    if msg == WM_NCCREATE {
        let create = unsafe { &*(lp.0 as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
    }
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Inputs;
    if !p.is_null() {
        let inputs = unsafe { &*p };
        // d3d12videosink's external-window procedure forwards to this original
        // procedure, including mouse/key events. Do not add another subclass:
        // layering one over the SDK hook can recurse when the HWND is reused.
        unsafe { inputs.message(hwnd, msg, wp, lp) };
    }
    if msg == WM_MOUSEWHEEL {
        return LRESULT(0);
    }
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    if msg == WM_NCDESTROY {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
// Child HWNDs occlude softbuffer's damage-only presents. When they move or
// disappear, invalidate Slint's pixels too, not just the Windows paint region.
// This runs only on geometry/visibility transitions, never on every video frame.
fn repaint_desktop(w: &SetupWindow) {
    crate::repaint_window(w.window());
}

struct Child {
    hwnd: HWND,
    // Owned by the UI thread; this stable allocation outlives every window callback.
    inputs: Box<Inputs>,
    geometry: Option<[i32; 8]>,
    corner_radius: i32,
    border_inset: i32,
    shown: bool,
}
impl Child {
    fn new(parent: HWND) -> Result<Self> {
        static CLASS: std::sync::OnceLock<std::result::Result<(), String>> =
            std::sync::OnceLock::new();
        let instance = unsafe { GetModuleHandleW(None) }?;
        CLASS
            .get_or_init(|| {
                let class = WNDCLASSW {
                    style: CS_DBLCLKS,
                    lpfnWndProc: Some(child_proc),
                    hInstance: HINSTANCE(instance.0),
                    lpszClassName: w!("MochuneNativeVideo"),
                    hbrBackground: HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0),
                    hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
                    ..Default::default()
                };
                if unsafe { RegisterClassW(&class) } == 0 {
                    Err(windows::core::Error::from_thread().to_string())
                } else {
                    Ok(())
                }
            })
            .as_ref()
            .map_err(|e| anyhow::anyhow!("Register video window: {e}"))?;
        let inputs = Box::new(Inputs::default());
        unsafe {
            let style = GetWindowLongPtrW(parent, GWL_STYLE) as u32;
            SetWindowLongPtrW(parent, GWL_STYLE, (style | WS_CLIPCHILDREN.0) as isize);
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("MochuneNativeVideo"),
                w!("Mochune video"),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                Some(parent),
                None,
                Some(HINSTANCE(instance.0)),
                Some((&*inputs as *const Inputs).cast()),
            )
        }?;
        Ok(Self {
            hwnd,
            inputs,
            geometry: None,
            corner_radius: -1,
            border_inset: -1,
            shown: false,
        })
    }
    fn show(&mut self, shown: bool) {
        if self.shown != shown {
            let _ = unsafe { ShowWindow(self.hwnd, if shown { SW_SHOWNA } else { SW_HIDE }) };
            self.shown = shown;
        }
    }
    fn position(&mut self, w: &SetupWindow, scale: f32) {
        let x = w.get_native_player_x();
        let y = w.get_native_player_y();
        let width = w.get_native_player_width().max(1.);
        let height = w.get_player_render_height().max(1.);
        let left = if w.get_left_pane_hovered() && !w.get_fullscreen_mode() {
            (220. - x).max(0.).min(width)
        } else {
            0.
        };
        let clip_top = w.get_native_clip_top();
        let top = (clip_top - y).max(0.).min(height);
        let bottom = (w.get_native_clip_bottom() - y).max(top).min(height);
        let geo = [
            (x * scale).round() as i32,
            (y * scale).round() as i32,
            (width * scale).round().max(1.) as i32,
            (height * scale).round().max(1.) as i32,
            (left * scale).round() as i32,
            (top * scale).round() as i32,
            (width * scale).round() as i32,
            (bottom * scale).round() as i32,
        ];
        // DXGI needs a mapped host to establish/present its first frame. Do not
        // make native visibility depend on status.has_frame/video_visible.
        // The independent alpha window paints loading controls above the host.
        let hide = w.get_music_only()
            || w.get_browsing_page()
            || w.get_music_layout()
            || w.get_settings_open()
            || w.get_download_open()
            || w.get_playlist_editor()
            || bottom <= top;
        let inset = (w.get_video_border_width() * scale).round().max(0.) as i32;
        let radius = ((w.get_video_corner_radius() * scale).round() as i32 - inset).max(0);
        let changed = self.geometry != Some(geo)
            || self.corner_radius != radius
            || self.border_inset != inset
            || self.shown == hide;
        if self.geometry != Some(geo) || self.corner_radius != radius || self.border_inset != inset
        {
            let _ = unsafe { MoveWindow(self.hwnd, geo[0], geo[1], geo[2], geo[3], true) };
            let region = unsafe {
                CreateRectRgn(
                    geo[4].max(inset),
                    geo[5].max(inset),
                    geo[6].min(geo[2] - inset),
                    geo[7].min(geo[3] - inset),
                )
            };
            if radius > 0 {
                let rounded = unsafe {
                    CreateRoundRectRgn(
                        inset,
                        inset,
                        geo[2] - inset + 1,
                        geo[3] - inset + 1,
                        radius * 2,
                        radius * 2,
                    )
                };
                if !rounded.0.is_null() {
                    unsafe {
                        CombineRgn(Some(region), Some(region), Some(rounded), RGN_AND);
                    }
                    let _ = unsafe { DeleteObject(HGDIOBJ(rounded.0)) };
                }
            }
            if unsafe { SetWindowRgn(self.hwnd, Some(region), true) } == 0 {
                let _ = unsafe { DeleteObject(HGDIOBJ(region.0)) };
            }
            self.geometry = Some(geo);
            self.corner_radius = radius;
            self.border_inset = inset;
        }
        self.show(!hide);
        if changed {
            repaint_desktop(w);
            crate::diagnostics::record(
                "native_video_geometry",
                serde_json::json!({
                    "physical_rect": &geo[..4], "clip": &geo[4..], "scale": scale,
                    "shown": !hide, "controls_outside": w.get_controls_outside(),
                    "logical_canvas": [width, w.get_native_player_height()],
                    "logical_video_height": height, "ui_surface":"owned alpha window"
                }),
            );
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        // Same UI thread that created the HWND. The playback session was stopped first.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}
fn controls_failure(shared: &Shared, w: &SetupWindow, error: impl std::fmt::Display) {
    let message = format!("Video controls: {error}");
    let mut previous = shared.failure.lock().unwrap();
    if previous.as_ref() != Some(&message) {
        crate::diagnostics::record(
            "native_controls_error",
            serde_json::json!({"error":message}),
        );
        w.set_player_error(message.clone().into());
    }
    *previous = Some(message);
}
struct Shared {
    process_transport: Mutex<std::sync::Weak<super::windows_process::Transport>>,
    requested: AtomicBool,
    binding: AtomicBool,
    lifecycle: Mutex<()>,
    hwnd: std::sync::atomic::AtomicIsize,
    sink: Mutex<Option<gst::glib::WeakRef<gst::Element>>>,
    failure: Mutex<Option<String>>,
    epoch: std::sync::atomic::AtomicU64,
    evidence: Arc<acceleration::Evidence>,
}
#[derive(Clone)]
pub(super) struct Controller(Arc<Shared>);
impl Controller {
    pub fn new(evidence: Arc<acceleration::Evidence>) -> Self {
        Self(Arc::new(Shared {
            process_transport: Mutex::new(std::sync::Weak::new()),
            requested: AtomicBool::new(false),
            binding: AtomicBool::new(false),
            lifecycle: Mutex::new(()),
            hwnd: std::sync::atomic::AtomicIsize::new(0),
            sink: Mutex::new(None),
            failure: Mutex::new(None),
            epoch: std::sync::atomic::AtomicU64::new(0),
            evidence,
        }))
    }
    pub fn prepare_process_host(&self) -> Result<isize> {
        self.0.requested.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(error) = self.0.failure.lock().unwrap().take() {
                bail!("Video host: {error}");
            }
            let hwnd = self.0.hwnd.load(Ordering::Acquire);
            if hwnd != 0 {
                return Ok(hwnd);
            }
            anyhow::ensure!(Instant::now() < deadline, "Video host window is not ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn set_process_transport(
        &self,
        transport: std::sync::Weak<super::windows_process::Transport>,
    ) {
        *self.0.process_transport.lock().unwrap() = transport;
    }
    pub fn bind_process_window(&self, hwnd: isize) {
        self.0.hwnd.store(hwnd, Ordering::Release);
    }
    pub fn process_artwork(&self, _art: Option<Arc<Artwork>>) {
        // Protocol compatibility only: the parent owns its local alpha UI layer.
    }
    pub fn process_sample(&self) -> (u64, f64, bool, u64) {
        let sink = self
            .0
            .sink
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.upgrade());
        let mut has_frame = false;
        let mut dropped = 0;
        if let Some(sink) = sink {
            if let Some(sample) = sink.property::<Option<gst::Sample>>("last-sample") {
                has_frame = true;
                if let Some(pts) = sample.buffer().and_then(|b| b.pts()) {
                    LAST_VIDEO_PTS.store(pts.nseconds(), Ordering::Release);
                }
                let stats = sink.property::<gst::Structure>("stats");
                dropped = stats.get::<u64>("dropped").unwrap_or(0);
                if let Ok(n) = stats.get::<u64>("rendered") {
                    RENDERED.store(n, Ordering::Relaxed);
                    self.0.evidence.presented.store(n, Ordering::Relaxed);
                }
            }
        }
        (
            RENDERED.load(Ordering::Relaxed),
            super::video_position(),
            has_frame,
            dropped,
        )
    }
    pub fn stop(&self) {
        self.0.requested.store(false, Ordering::Release);
        *self.0.process_transport.lock().unwrap() = std::sync::Weak::new();
        *PROCESS_HEALTH.lock().unwrap() = serde_json::Value::Null;
        let old_sink = self.0.sink.lock().unwrap().take();
        let had_video = old_sink.is_some();
        if let Some(sink) = old_sink.and_then(|s| s.upgrade()) {
            // Do not retain the last decoded frame through BaseSink's sample cache.
            sink.set_property("enable-last-sample", false);
        }
        if had_video {
            CLOSED.fetch_add(1, Ordering::Relaxed);
            // Pipeline is NULL before stop(): finish deferred resource releases.
            flush_loaded_devices();
            crate::diagnostics::record(
                "native_video_gpu_drained",
                serde_json::json!({
                    "overlay_cache_released": true, "sdk_device_cache": "process lifetime",
                    "process_memory": crate::diagnostics::process_memory()
                }),
            );
        }
        self.0.epoch.fetch_add(1, Ordering::Relaxed);
        ACTIVE.store(false, Ordering::Relaxed);
        UI_BYTES.store(0, Ordering::Relaxed);
    }
    pub fn make_sink(&self) -> Result<gst::Element> {
        struct Binding<'a>(&'a AtomicBool);
        impl Drop for Binding<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        {
            let _gate = self.0.lifecycle.lock().unwrap();
            self.0.binding.store(true, Ordering::Release);
        }
        let _binding = Binding(&self.0.binding);
        anyhow::ensure!(
            gst::version() >= (1, 26, 0, 0),
            "Embedded Windows video needs GStreamer 1.26+"
        );
        self.0.failure.lock().unwrap().take();
        self.0.requested.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(3);
        let hwnd = loop {
            if let Some(error) = self.0.failure.lock().unwrap().take() {
                bail!("Native video window: {error}");
            }
            let hwnd = self.0.hwnd.load(Ordering::Acquire);
            if hwnd != 0 {
                break hwnd;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "Native video window is not ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        // Only video playback loads/probes the D3D12 plugin and creates GPU devices.
        for factory in gst::ElementFactory::factories_with_type(
            gst::ElementFactoryType::DECODER,
            gst::Rank::NONE,
        ) {
            let name = factory.name();
            if name.starts_with("d3d11") {
                factory.set_rank(gst::Rank::NONE);
            } else if name.starts_with("d3d12") {
                factory.set_rank(gst::Rank::from(356));
            }
        }
        let sink = gst::ElementFactory::make("d3d12videosink")
            .property("direct-swapchain", true)
            .property("force-aspect-ratio", true)
            .property("enable-navigation-events", false)
            .property("error-on-closed", false)
            .build()
            .context("The bundled native D3D12 video sink is unavailable")?;
        sink.set_property_from_str("overlay-mode", "none");
        let overlay = sink
            .clone()
            .dynamic_cast::<gst_video::VideoOverlay>()
            .map_err(|_| anyhow::anyhow!("Native video overlay interface missing"))?;
        unsafe { overlay.set_window_handle(hwnd as usize) };
        sink.add_weak_ref_notify(|| {
            DISPOSED.fetch_add(1, Ordering::Relaxed);
            crate::diagnostics::record(
                "native_video_sink_disposed",
                serde_json::json!({
                    "process_memory": crate::diagnostics::process_memory()
                }),
            );
        });
        // Never upload software-decoded video as a fallback.
        let caps: gst::Caps = super::windows_video_caps(false).parse()?;
        let filter = gst::ElementFactory::make("capsfilter")
            .property("caps", caps)
            .build()?;
        let bin = gst::Bin::new();
        bin.add_many([&filter, &sink])?;
        filter.link(&sink)?;
        bin.add_pad(&gst::GhostPad::with_target(
            &filter.static_pad("sink").unwrap(),
        )?)?;
        *self.0.sink.lock().unwrap() = Some(sink.downgrade());
        SESSIONS.fetch_add(1, Ordering::Relaxed);
        RENDERED.store(0, Ordering::Relaxed);
        crate::diagnostics::record(
            "native_video_session",
            serde_json::json!({"backend":"D3D12 direct swapchain","wgpu":false,"decode_policy":"hardware video only","software_fallback":false,"ui_overlay":"owned alpha UI window","input":"D3D12Memory"}),
        );
        Ok(bin.upcast())
    }
    pub fn timer(&self, app: &SetupWindow, ready: Arc<AtomicBool>) -> slint::Timer {
        let timer = slint::Timer::default();
        let shared = self.0.clone();
        let weak = app.as_weak();
        let mut child = None::<Child>;
        let mut layer = None::<super::windows_overlay::Layer>;
        let menu_view =
            std::rc::Rc::new(std::cell::RefCell::new(None::<crate::NativePlayerOverlay>));
        let keys = menu_view.clone();
        app.on_native_player_key(move |text| {
            keys.borrow()
                .as_ref()
                .is_some_and(|v| super::native_overlay::route_menu_key(v, text))
        });
        let mut epoch = 0;
        let mut last_sample = Instant::now();
        app.set_native_video_enabled(true);
        let focus = self.0.clone();
        let focus_main = app.as_weak();
        app.on_native_focus_player(move || {
            if let Some(w) = focus_main.upgrade() {
                w.invoke_focus_video_shortcuts();
            }
            let hwnd = focus.hwnd.load(Ordering::Acquire);
            if hwnd != 0 {
                let _ = unsafe { SetFocus(Some(GetAncestor(HWND(hwnd as *mut c_void), GA_ROOT))) };
            }
        });
        app.set_graphics_info("Software UI · native D3D12 video on demand".into());
        app.set_hardware_support(
            "Video hardware is checked when playback starts; Music creates no video device.".into(),
        );
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(33),
            move || {
                let Some(w) = weak.upgrade() else { return };
                let (parent, scale) =
                    w.window()
                        .with_winit_window(|native| {
                            let hwnd = native.window_handle().ok().and_then(|handle| match handle
                                .as_raw()
                            {
                                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                                _ => None,
                            });
                            (hwnd, native.scale_factor() as f32)
                        })
                        .unwrap_or((None, 1.));
                let Some(parent) = parent else { return };
                ready.store(true, Ordering::Release);
                if !shared.requested.load(Ordering::Acquire) {
                    layer.take();
                    menu_view.borrow_mut().take();
                    UI_BYTES.store(0, Ordering::Relaxed);
                    let _gate = shared.lifecycle.lock().unwrap();
                    // A worker may have requested playback after the first check.
                    if shared.requested.load(Ordering::Acquire) {
                        return;
                    }
                    if shared.binding.load(Ordering::Acquire) {
                        if let Some(child) = child.as_mut() {
                            if child.shown {
                                child.show(false);
                                repaint_desktop(&w);
                            }
                        }
                        return;
                    }
                    shared.hwnd.store(0, Ordering::Release);
                    if child.take().is_some() {
                        w.set_video_visible(false);
                        repaint_desktop(&w);
                        UI_BYTES.store(0, Ordering::Relaxed);
                        crate::diagnostics::record(
                            "native_video_released",
                            serde_json::json!({
                                "ui_buffers_released": true,
                                "process_memory": crate::diagnostics::process_memory()
                            }),
                        );
                    }
                    return;
                }
                sync_cursor(&w, HWND(parent as *mut c_void), layer.as_ref().is_some_and(|l| l.ui.menu_active()));
                // Hide immediately on a mode switch. Destruction waits for the
                // worker to finish NULL/teardown and clear requested.
                if w.get_music_layout() {
                    layer.take();
                    menu_view.borrow_mut().take();
                    UI_BYTES.store(0, Ordering::Relaxed);
                    if let Some(t)=shared.process_transport.lock().unwrap().upgrade(){t.terminate();}
                    if let Some(child) = child.as_mut() {
                        if child.shown {
                            child.show(false);
                            repaint_desktop(&w);
                        }
                    }
                    return;
                }
                let current_epoch = shared.epoch.load(Ordering::Relaxed);
                if current_epoch != epoch {
                    epoch = current_epoch;
                    w.set_video_visible(false);
                }
                if child.is_none() {
                    match Child::new(HWND(parent as *mut c_void)) {
                        Ok(new_child) => {
                            shared.hwnd.store(new_child.hwnd.0 as isize, Ordering::Release);
                            child=Some(new_child);
                        }
                        Err(error) => {
                            *shared.failure.lock().unwrap() = Some(error.to_string());
                            return;
                        }
                    }
                }
                let child = child.as_mut().unwrap();
                child.position(&w, scale);
                if layer.is_none() {
                    match super::windows_overlay::Layer::new(HWND(parent as *mut c_void), &w) {
                        Ok(new_layer) => {
                            *menu_view.borrow_mut() = Some(new_layer.ui.view.clone_strong());
                            layer = Some(new_layer);
                        }
                        Err(error) => {
                            controls_failure(&shared, &w, error);
                            return;
                        }
                    }
                }
                let layer = layer.as_mut().unwrap();
                layer.ui.sync(&w);
                for input in layer.inputs() { child.inputs.push(input); }
                // Dispatch outside the queue lock: callbacks can reenter WndProc.
                let remote = shared.process_transport.lock().unwrap().upgrade();
                if let Some(t) = &remote {
                    for input in t.inputs() {
                        child.inputs.push(input);
                    }
                }
                let inputs = child
                    .inputs
                    .events
                    .lock()
                    .unwrap()
                    .drain(..)
                    .collect::<Vec<_>>();
                for input in inputs {
                    use slint::platform::{PointerEventButton as B, WindowEvent as E};
                    let pos = |x: i32, y: i32| {
                        slint::LogicalPosition::new(x as f32 / scale, y as f32 / scale)
                    };
                    match input {
                        Input::Move(x, y) => {
                            let pointer = pos(x, y);
                            let root_x = w.get_native_player_x() + pointer.x;
                            let left_threshold = if w.get_left_pane_hovered() { 220. } else { 10. };
                            w.set_left_pane_hovered(
                                !w.get_fullscreen_mode()
                                    && !w.get_settings_open()
                                    && root_x < left_threshold,
                            );
                            layer.ui.pointer(E::PointerMoved { position: pointer });
                        }
                        Input::Down(x, y) => {
                            let _ = unsafe { SetFocus(Some(HWND(parent as *mut c_void))) };
                            w.invoke_focus_video_shortcuts();
                            let pointer = pos(x, y);
                            layer.ui.pointer(E::PointerPressed {
                                position: pointer,
                                button: B::Left,
                            });
                        }
                        Input::Up(x, y) => {
                            let pointer = pos(x, y);
                            layer.ui.pointer(E::PointerReleased {
                                position: pointer,
                                button: B::Left,
                            });
                        }
                        Input::Leave => {
                            // Crossing from the picture into its owned UI window is
                            // not leaving the player. Avoid flickering hover state.
                            let mut point = windows::Win32::Foundation::POINT::default();
                            if unsafe { GetCursorPos(&mut point) }.is_ok() {
                                let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(HWND(parent as *mut c_void), &mut point) }.ok();
                                let x = point.x as f32 / scale - w.get_native_player_x();
                                let y = point.y as f32 / scale - w.get_native_player_y();
                                if x < 0. || y < 0. || x >= w.get_native_player_width() || y >= w.get_native_player_height() {
                                    layer.ui.pointer(E::PointerExited);
                                }
                            }
                        },
                        Input::Blur => {
                            if unsafe {GetForegroundWindow()} != HWND(parent as *mut c_void) {
                                layer.ui.pointer(E::PointerExited);
                            }
                        }
                        Input::Wheel(delta) => {
                            if layer.ui.menu_active() {
                                layer.ui.pointer(E::PointerScrolled { position: slint::LogicalPosition::default(), delta_x: 0., delta_y: delta as f32 / 3. });
                            } else if !w.get_fullscreen_mode() {
                                // The native picture consumes OS wheel messages. Scroll
                                // its owning content pane, not the offscreen controls.
                                let bottom = (w.get_page_visible_height() - w.get_page_content_height()).min(0.);
                                w.set_page_scroll((w.get_page_scroll() + delta as f32 / 3.).clamp(bottom, 0.));
                            }
                        }
                        Input::Key(key, pressed) => {
                            if let Some(text) = key_text(key) {
                                w.window().dispatch_event(if pressed {
                                    E::KeyPressed { text }
                                } else {
                                    E::KeyReleased { text }
                                });
                            }
                        }
                    }
                }
                // Input callbacks may change controls placement/visibility this tick.
                layer.ui.sync(&w);
                layer.ui.render(w.get_native_player_width(), w.get_native_player_height(), w.get_player_render_height());
                if let Some(geo) = child.geometry {
                    if let Err(error) = layer.present(geo, child.shown) {
                        controls_failure(&shared, &w, error);
                        layer.hide();
                        return;
                    }
                }
                UI_BYTES.store(layer.bytes() as u64, Ordering::Relaxed);
                UI_UPLOADS.store(layer.uploads, Ordering::Relaxed);
                let sink = shared
                    .sink
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|sink| sink.upgrade());
                if let Some(t) = remote {
                    let status = t.status.lock().unwrap();
                    *PROCESS_HEALTH.lock().unwrap()=serde_json::json!({"memory":status.process_memory,"hardware_verified":status.verified,"rendered":status.rendered});
                    RENDERED.store(status.rendered, Ordering::Relaxed);
                    LAST_VIDEO_PTS.store((status.pts.max(0.) * 1e9) as u64, Ordering::Release);
                    w.set_video_frames(status.rendered.min(i32::MAX as u64) as i32);
                    // Paused preroll has a valid picture even before the rendered counter advances.
                    if status.has_frame {
                        ACTIVE.store(true, Ordering::Relaxed);
                        w.set_video_visible(true);
                    }
                }
                if let Some(sink) = sink {
                    if last_sample.elapsed() >= Duration::from_millis(100) {
                        last_sample = Instant::now();
                        if let Some(sample) = sink.property::<Option<gst::Sample>>("last-sample") {
                            if let Some(pts) = sample.buffer().and_then(|buffer| buffer.pts()) {
                                LAST_VIDEO_PTS.store(pts.nseconds(), Ordering::Release);
                            }
                            let stats = sink.property::<gst::Structure>("stats");
                            if let Ok(rendered) = stats.get::<u64>("rendered") {
                                RENDERED.store(rendered, Ordering::Relaxed);
                                shared.evidence.presented.store(rendered, Ordering::Relaxed);
                                w.set_video_frames(rendered.min(i32::MAX as u64) as i32);
                            }
                            ACTIVE.store(true, Ordering::Relaxed);
                            w.set_video_visible(true);
                        }
                    }
                }
            },
        );
        timer
    }
}
fn key_text(key: u32) -> Option<slint::SharedString> {
    use slint::platform::Key;
    Some(match key {
        0x25 => Key::LeftArrow.into(),
        0x27 => Key::RightArrow.into(),
        0x26 => Key::UpArrow.into(),
        0x28 => Key::DownArrow.into(),
        0x1B => Key::Escape.into(),
        0x0D => Key::Return.into(),
        0x79 => Key::F10.into(),
        0x20 => " ".into(),
        0x46 => "f".into(),
        _ => return None,
    })
}
// Created by the video helper, never subclassed across process boundaries.
pub(super) struct ProcessWindow(Child);
impl ProcessWindow {
    pub fn new(parent: HWND) -> Result<Self> {
        Child::new(parent).map(Self)
    }
    pub fn hwnd(&self) -> isize {
        self.0.hwnd.0 as isize
    }
    pub fn fit_parent(&mut self, parent: HWND) {
        let mut rect = windows::Win32::Foundation::RECT::default();
        if unsafe { GetClientRect(parent, &mut rect) }.is_ok() {
            let geo = [0, 0, rect.right.max(1), rect.bottom.max(1), 0, 0, 0, 0];
            if self.0.geometry != Some(geo) {
                let _ = unsafe { MoveWindow(self.0.hwnd, 0, 0, geo[2], geo[3], false) };
                self.0.geometry = Some(geo);
            }
            self.0.show(true);
        }
    }
    pub fn inputs(&self) -> Vec<Input> {
        self.0.inputs.events.lock().unwrap().drain(..).collect()
    }
}
