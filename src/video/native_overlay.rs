#![cfg_attr(not(feature = "native-overlay-test"), allow(dead_code))]
//! Software-only artwork for native video controls. Never receives video pixels.
//! Slint 1.18.1 is pinned: its separate-context constructor is experimental.
use crate::native_ui::BufferedRange;
use crate::{NativePlayerOverlay, SetupWindow};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::private_unstable_api::re_exports::SlintContext;
use slint::{ComponentHandle, Model};
use std::{rc::Rc, sync::Arc};
struct OffscreenPlatform(std::rc::Weak<MinimalSoftwareWindow>);
impl Platform for OffscreenPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        self.0
            .upgrade()
            .map(|w| w as Rc<dyn WindowAdapter>)
            .ok_or_else(|| PlatformError::Other("Offscreen window released".into()))
    }
}
#[derive(Debug)]
pub(super) struct Artwork {
    pub version: u64,
    pub canvas: [u32; 2],
    pub rect: [u32; 4],
    pub bgra: Vec<u8>,
}
// Only an open video menu consumes these keys; text entry and application
// shortcuts keep normal OS/Slint focus in the main window.
pub(super) fn route_menu_key(view: &NativePlayerOverlay, text: slint::SharedString) -> bool {
    use slint::platform::{Key, WindowEvent};
    if !view.get_menu_active() {
        return false;
    }
    if text == slint::SharedString::from(Key::Tab) {
        view.invoke_dismiss_menus();
        return false;
    }
    if ![
        slint::SharedString::from(Key::UpArrow),
        Key::DownArrow.into(),
        Key::Return.into(),
        Key::Escape.into(),
        " ".into(),
    ]
    .contains(&text)
    {
        return false;
    }
    view.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    view.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    true
}
pub(super) struct Overlay {
    pub view: NativePlayerOverlay,
    context: SlintContext,
    requests: std::cell::Cell<Option<(i32, i32)>>,
    theme_revision: std::cell::Cell<i32>,
    font_count: std::cell::Cell<usize>,
    window: Rc<MinimalSoftwareWindow>,
    ranges: Rc<slint::VecModel<BufferedRange>>,
    pixels: Vec<PremultipliedRgbaColor>,
    rows: Vec<Option<(u32, u32)>>,
    size: [u32; 2],
    version: u64,
    geometry: Option<(u32, u32, u32)>,
    redraws: u8,
    popup_capture: std::cell::Cell<bool>,
    pointer_position: std::cell::Cell<Option<(f32, f32)>>,
    pub artwork: Option<Arc<Artwork>>,
}
impl Overlay {
    pub fn new(main: Option<slint::Weak<SetupWindow>>) -> Result<Self, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        let context = SlintContext::new(Box::new(OffscreenPlatform(Rc::downgrade(&window))));
        let view = NativePlayerOverlay::new_with_context(context.clone())?;
        // Register after attaching the context; embedded-font registration in a
        // generated constructor otherwise creates a window on the ambient backend.
        for data in [
            include_bytes!("../../ui/fonts/DejaVuSans.ttf").as_slice(),
            include_bytes!("../../ui/fonts/DejaVuSans-Bold.ttf").as_slice(),
            include_bytes!("../../ui/fonts/DejaVuSansMono-Bold.ttf").as_slice(),
        ] {
            window
                .renderer()
                .register_font_from_memory(data.into())
                .map_err(|e| PlatformError::Other(e.to_string()))?;
        }

        let source = main.clone();
        view.global::<crate::native_ui::MusicTheme>().on_resolve(
            move |section, key, kind, state, revision| {
                let Some(main) = source.as_ref().and_then(|w| w.upgrade()) else {
                    return Default::default();
                };
                let p = main
                    .global::<crate::MusicTheme>()
                    .invoke_resolve(section, key, kind, state, revision);
                crate::native_ui::ThemePaint {
                    background: p.background,
                    has_background: p.has_background,
                    gradient_end: p.gradient_end,
                    has_gradient_end: p.has_gradient_end,
                    border: p.border,
                    has_border: p.has_border,
                    text: p.text,
                    has_text: p.has_text,
                    tint: p.tint,
                    has_tint: p.has_tint,
                    radius: p.radius,
                    has_radius: p.has_radius,
                    border_width: p.border_width,
                    has_border_width: p.has_border_width,
                    font_family: p.font_family,
                    has_font_family: p.has_font_family,
                    font_size: p.font_size,
                    has_font_size: p.has_font_size,
                    font_weight: p.font_weight,
                    has_font_weight: p.has_font_weight,
                    opacity: p.opacity,
                    has_opacity: p.has_opacity,
                    scale: p.scale,
                    has_scale: p.has_scale,
                    offset_x: p.offset_x,
                    offset_y: p.offset_y,
                    inset: p.inset,
                    frame_slice: p.frame_slice,
                    pixelated: p.pixelated,
                    replace_chrome: p.replace_chrome,
                    image: p.image,
                    frame: p.frame,
                }
            },
        );
        let weak = main.clone();
        view.on_player_activity(move || {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_player_activity();
            }
        });
        let weak = main.clone();
        view.on_player_click(move || {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_player_click();
            }
        });
        let weak = main.clone();
        view.on_player_double_click(move |value| {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_player_double_click(value);
            }
        });
        let weak = main.clone();
        view.on_toggle_pause(move || {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_toggle_pause();
            }
        });
        let weak = main.clone();
        view.on_play(move || {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_play();
            }
        });
        let weak = main.clone();
        view.on_seek(move |value| {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_seek(value);
            }
        });
        let weak = main.clone();
        view.on_volume_change(move |value| {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                if value <= 0. && w.get_volume() > 0. {
                    w.set_previous_volume(w.get_volume());
                }
                w.invoke_volume_change(value);
            }
        });
        let weak = main.clone();
        view.on_toggle_controls_position(move || {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_toggle_controls_position();
            }
        });
        let weak = main.clone();
        view.on_choose_caption(move |value| {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_choose_caption(value);
            }
        });
        let weak = main.clone();
        view.on_toggle_video_repeat(move || {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_toggle_video_repeat();
            }
        });
        let weak = main.clone();
        view.on_choose_quality(move |value| {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_choose_quality(value);
            }
        });
        let weak = main.clone();
        view.on_toggle_fullscreen(move || {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_toggle_fullscreen();
            }
        });
        let weak = main;
        view.on_global_key(move |text| {
            if let Some(w) = weak.as_ref().and_then(|w| w.upgrade()) {
                w.invoke_focus_video_shortcuts();
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                        text: text.clone(),
                    });
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
            }
        });
        let ranges = Rc::new(slint::VecModel::from(vec![
            BufferedRange {
                start: 0.,
                estimated: false,
                end: 0.
            };
            crate::timeline::SLOTS
        ]));
        view.set_buffered_ranges(ranges.clone().into());
        view.set_previous_volume(70.);
        view.show()?;
        Ok(Self {
            view,
            context,
            requests: std::cell::Cell::new(None),
            theme_revision: std::cell::Cell::new(-1),
            font_count: std::cell::Cell::new(0),
            window,
            ranges,
            pixels: Vec::new(),
            rows: Vec::new(),
            size: [0, 0],
            version: 0,
            geometry: None,
            redraws: 0,
            popup_capture: std::cell::Cell::new(false),
            pointer_position: std::cell::Cell::new(None),
            artwork: None,
        })
    }
    pub fn sync(&self, main: &SetupWindow) {
        let theme = main.global::<crate::MusicTheme>();
        let native = self.view.global::<crate::native_ui::MusicTheme>();
        native.set_music_mode(false);
        native.set_defined(theme.get_defined());
        native.set_selected(theme.get_selected());
        let revision = theme.get_revision();
        if self.theme_revision.replace(revision) != revision {
            let fonts = crate::music_skin::registered_font_bytes();
            for bytes in fonts.iter().skip(self.font_count.get()) {
                let _ = self
                    .window
                    .renderer()
                    .register_font_from_memory((*bytes).into());
            }
            self.font_count.set(fonts.len());
            native.set_revision(revision);
        }
        self.view.set_picture_radius(main.get_video_corner_radius());
        self.view
            .set_picture_border_width(main.get_video_border_width());
        self.view.set_light_mode(main.get_light_mode());
        self.view.set_previous_volume(main.get_previous_volume());
        self.view.set_playback_active(main.get_playback_active());
        let requests = (
            main.get_quality_open_request(),
            main.get_caption_open_request(),
        );
        if let Some(previous) = self.requests.replace(Some(requests)) {
            if requests.0 != previous.0 {
                self.view.set_quality_open_request(requests.0);
            }
            if requests.1 != previous.1 {
                self.view.set_caption_open_request(requests.1);
            }
        }

        if self.view.get_controls_over_video() != main.get_controls_over_video() {
            self.view
                .set_controls_over_video(main.get_controls_over_video());
        }
        if self.view.get_controls_outside() != main.get_controls_outside() {
            self.view.set_controls_outside(main.get_controls_outside());
        }
        if self.view.get_controls_shown() != main.get_controls_shown() {
            self.view.set_controls_shown(main.get_controls_shown());
        }
        if self.view.get_fullscreen_mode() != main.get_fullscreen_mode() {
            self.view.set_fullscreen_mode(main.get_fullscreen_mode());
        }
        if self.view.get_has_selection() != main.get_has_selection() {
            self.view.set_has_selection(main.get_has_selection());
        }
        if self.view.get_selection_playable() != main.get_selection_playable() {
            self.view
                .set_selection_playable(main.get_selection_playable());
        }
        if self.view.get_paused() != main.get_paused() {
            self.view.set_paused(main.get_paused());
        }
        if self.view.get_resolving() != main.get_resolving() {
            self.view.set_resolving(main.get_resolving());
        }
        if self.view.get_video_visible() != main.get_video_visible() {
            self.view.set_video_visible(main.get_video_visible());
        }
        if self.view.get_music_only() != main.get_music_only() {
            self.view.set_music_only(main.get_music_only());
        }
        if self.view.get_legacy_quality() != main.get_legacy_quality() {
            self.view.set_legacy_quality(main.get_legacy_quality());
        }
        if self.view.get_repeat_video() != main.get_repeat_video() {
            self.view.set_repeat_video(main.get_repeat_video());
        }
        let main_ranges = main.get_buffered_ranges();
        for i in 0..crate::timeline::SLOTS {
            let row = main_ranges.row_data(i).map_or(
                BufferedRange {
                    start: 0.,
                    end: 0.,
                    estimated: false,
                },
                |r| BufferedRange {
                    start: r.start,
                    end: r.end,
                    estimated: r.estimated,
                },
            );
            if self.ranges.row_data(i).as_ref() != Some(&row) {
                self.ranges.set_row_data(i, row);
            }
        }
        self.view
            .set_buffered_estimated(main.get_buffered_estimated());
        if self.view.get_position() != main.get_position() {
            self.view.set_position(main.get_position());
        }
        if self.view.get_duration() != main.get_duration() {
            self.view.set_duration(main.get_duration());
        }
        if self.view.get_volume() != main.get_volume() {
            self.view.set_volume(main.get_volume());
        }
        if self.view.get_clock() != main.get_clock() {
            self.view.set_clock(main.get_clock());
        }
        if self.view.get_caption_text() != main.get_caption_text() {
            self.view.set_caption_text(main.get_caption_text());
        }
        if self.view.get_caption_next() != main.get_caption_next() {
            self.view.set_caption_next(main.get_caption_next());
        }
        if self.view.get_seek_feedback() != main.get_seek_feedback() {
            self.view.set_seek_feedback(main.get_seek_feedback());
        }
        if self.view.get_seek_feedback_right() != main.get_seek_feedback_right() {
            self.view
                .set_seek_feedback_right(main.get_seek_feedback_right());
        }
        self.view.set_caption_labels(main.get_caption_labels());
        if self.view.get_caption_choice() != main.get_caption_choice() {
            self.view.set_caption_choice(main.get_caption_choice());
        }

        self.view.set_quality_labels(main.get_quality_labels());
        self.view
            .set_quality_compact_labels(main.get_quality_compact_labels());
        if self.view.get_quality_choice() != main.get_quality_choice() {
            self.view.set_quality_choice(main.get_quality_choice());
        }
    }
    pub fn render(&mut self, width: f32, height: f32, video_height: f32) -> bool {
        self.view.set_popup_boundary(video_height);
        if width < 1. || height < 1. {
            return false;
        }
        let scale = (4_194_304. / (width * height))
            .sqrt()
            .min(1.)
            .min(4096. / width)
            .min(4096. / height);
        let size = [
            (width * scale).ceil().max(1.) as u32,
            (height * scale).ceil().max(1.) as u32,
        ];
        if self.size != size || (self.view.window().scale_factor() - scale).abs() > 0.001 {
            self.size = size;
            self.window
                .window()
                .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
                    scale_factor: scale,
                });
            self.window
                .set_size(slint::PhysicalSize::new(size[0], size[1]));
            self.pixels = vec![PremultipliedRgbaColor::default(); (size[0] * size[1]) as usize];
            self.rows = vec![None; size[1] as usize];
        }
        let visible_height = (video_height * scale).round().max(0.) as u32;
        let geometry = (size[0], size[1], visible_height);
        if self.geometry != Some(geometry) {
            self.geometry = Some(geometry);
            self.redraws = 3;
        }
        if self.redraws > 0 {
            self.redraws -= 1;
            self.window.window().request_redraw();
        }
        self.context.update_timers_and_animations();
        let mut dirty = None;
        let pixels = &mut self.pixels;
        self.window.draw_if_needed(|r| {
            dirty = Some(r.render(pixels, size[0] as usize));
        });
        let Some(dirty) = dirty else {
            return false;
        };
        let top = dirty.bounding_box_origin().y.max(0) as u32;
        let bottom = (top + dirty.bounding_box_size().height).min(size[1]);
        for y in top..bottom {
            let row = &self.pixels[(y * size[0]) as usize..((y + 1) * size[0]) as usize];
            let first = row.iter().position(|p| p.alpha > 0);
            let last = row.iter().rposition(|p| p.alpha > 0);
            self.rows[y as usize] = first.zip(last).map(|(a, b)| (a as u32, b as u32 + 1));
        }
        let mut bounds = None::<[u32; 4]>;
        for (y, row) in self
            .rows
            .iter()
            .enumerate()
            .take(visible_height.min(size[1]) as usize)
        {
            if let Some((left, right)) = row {
                match bounds.as_mut() {
                    Some(b) => {
                        b[0] = b[0].min(*left);
                        b[1] = b[1].min(y as u32);
                        b[2] = b[2].max(*right);
                        b[3] = y as u32 + 1;
                    }
                    None => bounds = Some([*left, y as u32, *right, y as u32 + 1]),
                }
            }
        }
        self.version = self.version.wrapping_add(1);
        self.artwork = bounds.map(|[x, y, right, bottom]| {
            let w = right - x;
            let h = bottom - y;
            let mut bgra = Vec::with_capacity((w * h * 4) as usize);
            for row in y..bottom {
                for pixel in
                    &self.pixels[(row * size[0] + x) as usize..(row * size[0] + right) as usize]
                {
                    bgra.extend_from_slice(&[pixel.blue, pixel.green, pixel.red, pixel.alpha]);
                }
            }
            Arc::new(Artwork {
                version: self.version,
                canvas: [size[0], visible_height.min(size[1])],
                rect: [x, y, w, h],
                bgra,
            })
        });
        true
    }
    pub fn pointer(&self, event: slint::platform::WindowEvent) {
        use slint::platform::{PointerEventButton as B, WindowEvent as E};
        // Win32 can synthesize WM_MOUSEMOVE on redraw/layer changes even with
        // a stationary cursor. Those notifications must not restart auto-hide.
        if let E::PointerMoved { position } = &event {
            if self
                .pointer_position
                .replace(Some((position.x, position.y)))
                == Some((position.x, position.y))
            {
                return;
            }
        } else if matches!(&event, E::PointerExited) {
            self.pointer_position.set(None);
        }
        // Native input arrives in batches. Reveal the local hit-test tree before
        // dispatching the press, rather than waiting for the next main-window
        // sync: a move and press in one tick must reach the seek bar, not picture.
        if matches!(&event, E::PointerMoved { .. } | E::PointerPressed { .. }) {
            self.view.invoke_player_activity();
            self.view.set_controls_shown(true);
        }
        match &event {
            E::PointerPressed {
                position,
                button: B::Left,
            } if self.menu_active() => {
                self.popup_capture.set(true);
                self.view.invoke_popup_pointer(position.x, position.y, true);
                return;
            }
            E::PointerReleased {
                position,
                button: B::Left,
            } if self.popup_capture.replace(false) => {
                self.view
                    .invoke_popup_pointer(position.x, position.y, false);
                return;
            }
            E::PointerScrolled { delta_y, .. } if self.menu_active() => {
                self.view.invoke_scroll_popup(*delta_y);
                return;
            }
            _ => {}
        }
        self.view.window().dispatch_event(event);
    }
    pub fn menu_active(&self) -> bool {
        self.view.get_menu_active()
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // Showing a Slint window registers it with its context. Hide explicitly
        // before dropping the component so the context releases that registration.
        self.view.invoke_dismiss_menus();
        let _ = self.view.hide();
    }
}

#[cfg(feature = "native-overlay-test")]
pub(crate) fn probe() -> anyhow::Result<serde_json::Value> {
    use anyhow::Context;
    use slint::platform::{Key, PointerEventButton as B, WindowEvent as E};
    use std::cell::Cell;
    let mut ui = Overlay::new(None)?;
    ui.view.set_has_selection(true);
    ui.view.set_selection_playable(true);
    ui.view.set_video_visible(true);
    ui.view.set_controls_over_video(true);
    ui.view.set_controls_shown(true);
    ui.view.set_duration(240.);
    ui.view.set_position(60.);
    ui.view.set_volume(70.);
    ui.view.set_clock("1:00 / 4:00".into());
    ui.view
        .set_caption_text("Current caption stays above the next line".into());
    ui.view
        .set_caption_next("The video behind these controls remains on the GPU".into());
    ui.view
        .set_quality_labels(slint::ModelRc::new(slint::VecModel::from(vec![
            "360p".into(),
            "720p · 60 fps".into(),
            "1080p · 60 fps".into(),
        ])));
    ui.view
        .set_quality_compact_labels(slint::ModelRc::new(slint::VecModel::from(vec![
            "360@30".into(),
            "720@60".into(),
            "1080@60".into(),
        ])));
    ui.view.set_quality_choice(1);
    ui.view.set_playback_active(true);
    ui.view
        .set_caption_labels(slint::ModelRc::new(slint::VecModel::from(vec![
            "Off".into(),
            "English".into(),
        ])));
    ui.render(900., 506., 506.);
    let clicks = Rc::new(Cell::new(0));
    let count = clicks.clone();
    ui.view.on_player_click(move || count.set(count.get() + 1));
    let click = |ui: &Overlay, x: f32, y: f32| {
        for event in [
            E::PointerPressed {
                position: slint::LogicalPosition::new(x, y),
                button: B::Left,
            },
            E::PointerReleased {
                position: slint::LogicalPosition::new(x, y),
                button: B::Left,
            },
        ] {
            ui.pointer(event);
        }
    };
    click(&ui, 400., 480.);
    anyhow::ensure!(clicks.get() == 0, "Control padding toggled playback");
    click(&ui, 400., 200.);
    anyhow::ensure!(clicks.get() == 1, "Picture click stopped working");
    let normal = ui.artwork.as_ref().expect("visible artwork");
    let normal_bytes = normal.bgra.len();
    let path = std::path::PathBuf::from(
        std::env::var_os("RUST_EXTERNAL_ROOT")
            .ok_or_else(|| anyhow::anyhow!("Use the external runtime guard"))?,
    )
    .join("data/native-overlay-controls.png");
    let mut pixels = Vec::with_capacity(ui.pixels.len() * 4);
    for p in &ui.pixels {
        pixels.extend_from_slice(&[p.red, p.green, p.blue, p.alpha]);
    }
    image::RgbaImage::from_raw(ui.size[0], ui.size[1], pixels)
        .unwrap()
        .save(&path)?;
    let sought = Rc::new(Cell::new(-1.));
    let target = sought.clone();
    ui.view.on_seek(move |t| target.set(t));
    ui.view.set_controls_shown(false);
    ui.render(900., 506., 506.);
    let picture_clicks = clicks.get();
    // No render or main-window sync between hover, press and release.
    ui.pointer(E::PointerMoved {
        position: slint::LogicalPosition::new(450., 447.),
    });
    for event in [
        E::PointerPressed {
            position: slint::LogicalPosition::new(450., 447.),
            button: B::Left,
        },
        E::PointerReleased {
            position: slint::LogicalPosition::new(450., 447.),
            button: B::Left,
        },
    ] {
        ui.pointer(event);
    }
    anyhow::ensure!(
        (sought.get() - 120.).abs() < 5.,
        "Seek control failed: {}",
        sought.get()
    );
    anyhow::ensure!(
        clicks.get() == picture_clicks,
        "Hidden seek bar click toggled picture playback"
    );
    // Exercise real mouse coordinates, not only programmatic open/keyboard selection.
    let choices = Rc::new(Cell::new(-1));
    let chosen = choices.clone();
    ui.view.on_choose_quality(move |v| chosen.set(v));
    for (width, height) in [(900., 506.), (1493., 840.), (3840., 2160.)] {
        ui.view
            .set_buffered_ranges(slint::ModelRc::new(slint::VecModel::from(vec![
                BufferedRange {
                    start: 0.1,
                    end: 0.75,
                    estimated: false,
                },
            ])));
        ui.render(width, height, height);
        let scale = ui.window.window().scale_factor();
        let track = |fraction: f32| {
            let x = ((ui.view.get_seek_x() + ui.view.get_seek_width() * fraction) * scale) as usize;
            let y = ((ui.view.get_seek_y() + 9.) * scale) as usize;
            ui.pixels[y * ui.size[0] as usize + x]
        };
        let retained = track(0.5);
        let unretained = track(0.9);
        anyhow::ensure!(
            retained.red > unretained.red + 30,
            "Buffer track did not scale with seek bar at {width}"
        );
        let x = ui.view.get_quality_x() + 20.;
        let y = ui.view.get_quality_y() + 16.;
        ui.pointer(E::PointerMoved {
            position: slint::LogicalPosition::new(x, y),
        });
        click(&ui, x, y);
        ui.render(width, height, height);
        anyhow::ensure!(ui.menu_active(), "Mouse did not open quality at {width}");
        let x = ui.view.get_quality_popup_x() + 30.;
        let y = ui.view.get_quality_popup_y() + 4. + 36. * 2. + 18.;
        ui.pointer(E::PointerMoved {
            position: slint::LogicalPosition::new(x, y),
        });
        click(&ui, x, y);
        ui.render(width, height, height);
        anyhow::ensure!(
            !ui.menu_active() && choices.get() == 2,
            "Mouse quality selection failed at {width}: {}, open={}, target=({x},{y})",
            choices.get(),
            ui.menu_active()
        );
        ui.view.set_quality_choice(1);
        choices.set(-1);
    }
    let original_labels = ui.view.get_quality_labels();
    let original_compact = ui.view.get_quality_compact_labels();
    ui.view
        .set_quality_labels(slint::ModelRc::new(slint::VecModel::from(
            (0..16)
                .map(|n| format!("Quality {n}").into())
                .collect::<Vec<_>>(),
        )));
    ui.view
        .set_quality_compact_labels(ui.view.get_quality_labels());
    ui.view.set_quality_choice(0);
    ui.render(1493., 840., 840.);
    click(
        &ui,
        ui.view.get_quality_x() + 20.,
        ui.view.get_quality_y() + 16.,
    );
    ui.render(1493., 840., 840.);
    ui.pointer(E::PointerScrolled {
        position: slint::LogicalPosition::new(
            ui.view.get_quality_popup_x() + 30.,
            ui.view.get_quality_popup_y() + 30.,
        ),
        delta_x: 0.,
        delta_y: -252.,
    });
    ui.render(1493., 840., 840.);
    click(
        &ui,
        ui.view.get_quality_popup_x() + 30.,
        ui.view.get_quality_popup_y() + 4. + 36. * 2. + 18.,
    );
    anyhow::ensure!(
        choices.get() == 9 && !ui.menu_active(),
        "Scrolled quality selection failed: {}",
        choices.get()
    );
    ui.view.set_quality_labels(original_labels);
    ui.view.set_quality_compact_labels(original_compact);
    ui.view.set_quality_choice(1);
    // A lingering buffering notification cannot disable controls on a live picture.
    ui.view.set_resolving(true);
    ui.render(900., 506., 506.);
    click(
        &ui,
        ui.view.get_quality_x() + 20.,
        ui.view.get_quality_y() + 16.,
    );
    ui.render(900., 506., 506.);
    anyhow::ensure!(
        ui.menu_active(),
        "Playing quality picker stayed disabled by stale loading state"
    );
    ui.view.invoke_dismiss_menus();
    ui.view.set_resolving(false);
    ui.render(900., 506., 506.);
    ui.view
        .set_quality_open_request(ui.view.get_quality_open_request() + 1);
    ui.render(900., 506., 506.);
    let popup_top = ui.artwork.as_ref().unwrap().rect[1];
    anyhow::ensure!(
        popup_top < 400,
        "Quality popup did not draw above the control bar"
    );
    anyhow::ensure!(
        !route_menu_key(&ui.view, "x".into()),
        "Video menu swallowed text input"
    );
    anyhow::ensure!(
        route_menu_key(&ui.view, Key::DownArrow.into()),
        "Main-window menu navigation was not routed"
    );
    anyhow::ensure!(
        route_menu_key(&ui.view, Key::Return.into()),
        "Main-window menu selection was not routed"
    );
    anyhow::ensure!(
        !route_menu_key(&ui.view, Key::DownArrow.into()),
        "Closed menu swallowed a player shortcut"
    );
    anyhow::ensure!(
        ui.view.get_quality_choice() == 2,
        "Keyboard quality choice failed"
    );
    ui.view
        .set_quality_open_request(ui.view.get_quality_open_request() + 1);
    ui.render(900., 506., 506.);
    anyhow::ensure!(ui.view.get_menu_active(), "Quality picker did not open");
    ui.view
        .set_quality_open_request(ui.view.get_quality_open_request() + 1);
    ui.render(900., 506., 506.);
    anyhow::ensure!(
        !ui.view.get_menu_active(),
        "Reclicking quality picker did not close it"
    );
    ui.view.invoke_focus_request();
    ui.view.set_controls_over_video(false);
    ui.render(900., 506., 434.);
    anyhow::ensure!(
        ui.artwork
            .as_ref()
            .is_some_and(|f| f.rect[1] + f.rect[3] <= 434),
        "External controls leaked into video artwork"
    );
    // The original probe used only the design-time 900×506 size and missed
    // a fixed Window constraint: controls stayed there after the HWND resized.
    ui.view.invoke_dismiss_menus();
    ui.view.set_caption_text("".into());
    ui.view.set_caption_next("".into());
    ui.view.set_controls_over_video(true);
    for (width, height) in [(600., 337.), (1479., 832.), (3840., 2160.)] {
        ui.render(width, height, height);
        anyhow::ensure!(
            (ui.view.get_canvas_width() - width).abs() < 2.,
            "Overlay width stayed at its design size"
        );
        anyhow::ensure!(
            (ui.view.get_canvas_height() - height).abs() < 2.,
            "Overlay height stayed at its design size"
        );
        let frame = ui
            .artwork
            .as_ref()
            .context("Controls missing after resize")?;
        anyhow::ensure!(
            frame.rect[1] as f32 >= frame.canvas[1] as f32 * 0.7,
            "Controls floated above bottom after resize"
        );
        anyhow::ensure!(
            frame.rect[2] as f32 >= frame.canvas[0] as f32 * 0.95,
            "Controls did not span resized player"
        );
    }
    ui.view.set_controls_over_video(false);
    ui.view.set_controls_outside(true);
    ui.render(1479., 904., 832.);
    anyhow::ensure!(
        ui.artwork.is_none(),
        "Outside controls duplicated onto native video"
    );
    ui.view
        .set_quality_open_request(ui.view.get_quality_open_request() + 1);
    ui.render(1479., 904., 832.);
    let menu = ui
        .artwork
        .as_ref()
        .context("Outside quality menu missing")?;
    anyhow::ensure!(
        menu.rect[1] + menu.rect[3] <= 832 && menu.rect[1] + menu.rect[3] >= 800,
        "Outside quality menu clipped by the native video edge"
    );
    ui.view.invoke_dismiss_menus();
    ui.view.set_controls_over_video(true);
    ui.view.set_controls_outside(false);
    ui.view.set_controls_shown(false);
    ui.view.set_caption_text("".into());
    ui.view.set_caption_next("".into());
    ui.render(900., 506., 506.);
    anyhow::ensure!(ui.artwork.is_none(), "Hidden UI retained stale pixels");
    // Real movement reveals controls, but stationary Win32 redraw notifications
    // must leave them hidden even when the pointer is inside the player.
    let activity = Rc::new(Cell::new(0));
    let count = activity.clone();
    ui.view
        .on_player_activity(move || count.set(count.get() + 1));
    let position = slint::LogicalPosition::new(410., 200.);
    ui.pointer(E::PointerMoved { position });
    ui.view.set_controls_shown(false);
    ui.render(900., 506., 506.);
    let before = activity.get();
    for _ in 0..8 {
        ui.pointer(E::PointerMoved { position });
        ui.render(900., 506., 506.);
    }
    anyhow::ensure!(
        activity.get() == before && !ui.view.get_controls_shown() && ui.artwork.is_none(),
        "Stationary cursor kept controls alive"
    );
    ui.pointer(E::PointerMoved {
        position: slint::LogicalPosition::new(411., 200.),
    });
    anyhow::ensure!(
        ui.view.get_controls_shown() && activity.get() > before,
        "Real movement did not reveal controls"
    );
    ui.view.set_controls_shown(false);
    ui.render(900., 506., 506.);
    // Loading must remain visible above native video even while its controls are hidden.
    ui.view.set_resolving(true);
    ui.render(900., 506., 506.);
    let loading = ui
        .artwork
        .as_ref()
        .context("Missing native video loading artwork")?;
    anyhow::ensure!(
        loading.rect[1] < 253 && loading.rect[1] + loading.rect[3] > 253,
        "Loading indicator was clipped to the control bar"
    );
    // A loader must preserve transparent rounded corners. The old native
    // region implementation replaced these pixels with a black square.
    let alpha = |x: u32, y: u32| -> u8 {
        if x < loading.rect[0]
            || y < loading.rect[1]
            || x >= loading.rect[0] + loading.rect[2]
            || y >= loading.rect[1] + loading.rect[3]
        {
            return 0;
        }
        loading.bgra
            [(((y - loading.rect[1]) * loading.rect[2] + x - loading.rect[0]) * 4 + 3) as usize]
    };
    anyhow::ensure!(
        alpha(418, 221) == 0 && alpha(450, 250) > 0,
        "Loader lost its rounded alpha background"
    );
    ui.view.set_resolving(false);
    ui.render(900., 506., 506.);
    anyhow::ensure!(
        ui.artwork.is_none(),
        "Loading artwork remained after playback resumed"
    );
    // Teardown must also release a still-open inline popup and its focus tree.
    ui.view
        .set_quality_open_request(ui.view.get_quality_open_request() + 1);
    ui.view.set_resolving(true);
    ui.render(900., 506., 506.);
    let weak = Rc::downgrade(&ui.window);
    drop(ui);
    anyhow::ensure!(
        weak.upgrade().is_none(),
        "Offscreen video UI was retained after teardown"
    );
    Ok(
        serde_json::json!({"renderer":"offscreen software controls only","seek":sought.get(),"quality_keyboard":true,"quality_popup_top":popup_top,"outside_controls_clipped":true,"resize_layout":true,"outside_controls_not_duplicated":true,"outside_popup_unclipped":true,"hidden_artwork_cleared":true,"loading_visible_and_cleared":true,"released":true,"artwork_bytes":normal_bytes,"snapshot":path}),
    )
}

// Exercise the real desktop component at sizes larger than the host monitor.
// No player/backend, network requests, or decoded video buffers are created.
#[cfg(feature = "native-overlay-test")]
pub(crate) fn probe_main_layout() -> anyhow::Result<serde_json::Value> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(OffscreenPlatform(Rc::downgrade(&window))))?;
    let view = SetupWindow::new()?;
    view.set_native_video_enabled(true);
    view.show()?;
    let draw = |width: u32, height: u32| {
        window.request_redraw();
        window.set_size(slint::PhysicalSize::new(width, height));
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![PremultipliedRgbaColor::default(); (width * height) as usize];
        window.draw_if_needed(|r| {
            r.render(&mut pixels, width as usize);
        });
        // Geometry callbacks run as deferred change handlers.
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, width as usize);
        });
        pixels
    };
    draw(2560, 1080);
    std::thread::sleep(std::time::Duration::from_millis(40));
    draw(2560, 1080);
    anyhow::ensure!(
        view.get_native_player_x() >= 19. && view.get_native_player_y() >= 70.,
        "Initial native geometry was never initialized: {}, {}",
        view.get_native_player_x(),
        view.get_native_player_y()
    );
    view.set_music_only(false);
    view.set_has_selection(true);
    view.set_selection_playable(true);
    view.set_playback_active(true);
    view.set_video_visible(true);
    view.set_controls_shown(true);
    view.set_paused(false);
    view.set_resolving(false);
    let settle = || {
        std::thread::sleep(std::time::Duration::from_millis(40));
        draw(2560, 1080);
    };
    settle();
    anyhow::ensure!(
        !view.get_main_player_controls_visible(),
        "Main controls duplicate the native overlay"
    );
    view.set_controls_outside(true);
    settle();
    anyhow::ensure!(
        view.get_main_player_controls_visible(),
        "Outside controls disappeared"
    );
    view.set_controls_outside(false);
    settle();
    anyhow::ensure!(
        !view.get_main_player_controls_visible(),
        "Moving controls back retained a duplicate"
    );
    view.set_music_only(true);
    view.set_selected_image(slint::Image::from_rgba8(slint::SharedPixelBuffer::<
        slint::Rgba8Pixel,
    >::clone_from_slice(
        &[55, 110, 190, 255], 1, 1
    )));
    let audio_pixels = draw(2560, 1080);
    anyhow::ensure!(
        view.get_main_player_controls_visible(),
        "Audio-only playback hid all controls"
    );
    let center = audio_pixels[((view.get_native_player_y() + view.get_player_render_height() / 2.)
        as usize)
        * 2560
        + (view.get_native_player_x() + view.get_native_player_width() / 2.) as usize];
    anyhow::ensure!(
        center.blue == 190 && center.green == 110,
        "Audio-only thumbnail did not replace the black picture: {center:?}"
    );
    view.set_music_only(false);
    view.set_selected_image(Default::default());
    // Exercise the main software timeline as used by the Linux path. The
    // independent native overlay is covered by probe(), including alpha/hiding.
    view.set_native_video_enabled(false);
    view.set_timeline_probe(true);
    view.set_controls_shown(true);
    settle();
    // Test the actual shared seek track at several canvas sizes, including
    // shrinking/eviction: buffered pixels must stay behind played progress and
    // disappear immediately when their native bytes have been discarded.
    view.set_duration(100.);
    view.set_position(25.);
    for (width, height) in [(900, 700), (1920, 1080), (2560, 1080)] {
        crate::timeline::publish(
            &view,
            [crate::BufferedRange {
                start: 0.,
                estimated: false,
                end: 0.6,
            }],
        );
        let pixels = draw(width, height);
        let x = view.get_timeline_x();
        let y = view.get_timeline_y() + 1.;
        let w = view.get_timeline_width();
        anyhow::ensure!(w > 100. && x + w <= width as f32, "Timeline escaped layout");
        let at = |pixels: &[PremultipliedRgbaColor], fraction: f32| {
            pixels[(y as u32 * width + (x + w * fraction) as u32) as usize]
        };
        let orange = at(&pixels, 0.1);
        let gray = at(&pixels, 0.4);
        let base = at(&pixels, 0.8);
        anyhow::ensure!(
            orange.red > 200 && orange.green > 100 && orange.blue < 70,
            "Played portion misplaced at {width}: {orange:?}"
        );
        anyhow::ensure!(
            gray.red == 170 && gray.green == 170 && base.red == 76 && base.green == 76,
            "Buffered/unloaded track misplaced at {width}: {gray:?} {base:?}"
        );
        crate::timeline::publish(
            &view,
            [crate::BufferedRange {
                start: 0.7,
                end: 0.70001,
                estimated: false,
            }],
        );
        let pixels = draw(width, height);
        let narrow = pixels[(y as u32 * width + (x + w * 0.7 + 0.5) as u32) as usize];
        anyhow::ensure!(
            narrow.red == 170 && narrow.green == 170,
            "Subpixel buffered range disappeared at {width}: {narrow:?}"
        );
        crate::timeline::clear(&view);
        let pixels = draw(width, height);
        anyhow::ensure!(
            at(&pixels, 0.4).red == 76,
            "Evicted buffer pixels remained at {width}"
        );
    }
    draw(2560, 1080);
    let picture = (view.get_native_player_y(), view.get_native_player_height());
    view.set_has_selection(true);
    view.set_description("Description line.\n".repeat(100).into());
    view.set_description_expanded(true);
    draw(2560, 1080);
    view.set_page_scroll(-400.);
    draw(2560, 1080);
    anyhow::ensure!(
        (view.get_native_player_y() - picture.0 + 400.).abs() < 1.
            && (view.get_native_player_height() - picture.1).abs() < 1.,
        "Expanded details must scroll the picture without resizing it"
    );
    anyhow::ensure!(
        view.get_native_clip_top() > view.get_native_player_y()
            && view.get_native_clip_bottom() <= 1080.,
        "Scrolled native video escaped its fixed viewport"
    );
    view.set_page_scroll(-100000.);
    draw(2560, 1080);
    draw(2560, 1080);
    anyhow::ensure!(
        view.get_page_scroll()
            >= (view.get_page_visible_height() - view.get_page_content_height()).min(0.) - 1.,
        "Description overscrolled below content"
    );
    view.set_page_scroll(0.);
    draw(2560, 1080);
    let picture_height = view.get_player_render_height();
    view.set_controls_outside(true);
    draw(2560, 1080);
    anyhow::ensure!(
        (view.get_player_render_height() - picture_height).abs() < 1.,
        "Moving controls outside resized the picture"
    );
    anyhow::ensure!(
        (view.get_native_player_height() - picture.1 - 72.).abs() < 1.,
        "Outside controls did not add their own height"
    );
    view.set_description_expanded(false);
    draw(2560, 1080);
    anyhow::ensure!(
        view.get_page_scroll() == 0.,
        "Description collapse retained scroll"
    );
    view.set_settings_open(true);
    view.set_settings_tab(2);
    draw(1180, 600);
    draw(1180, 600);
    anyhow::ensure!(
        view.get_settings_content_height() > view.get_settings_visible_height() + 20.,
        "Storage page has no scroll range: content={}, viewport={}",
        view.get_settings_content_height(),
        view.get_settings_visible_height()
    );
    window
        .window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(650., 300.),
            delta_x: 0.,
            delta_y: -150.,
        });
    draw(1180, 600);
    anyhow::ensure!(
        view.get_settings_scroll_offset() < -1.,
        "Storage wheel did not scroll"
    );
    // Reach the actual last delete row, not merely a nonzero scroll offset.
    for (width, height) in [(1180, 600), (1366, 768), (1920, 1080)] {
        draw(width, height);
        draw(width, height);
        view.set_settings_scroll_offset(0.);
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(width as f32 / 2. + 80., height as f32 / 2.),
                delta_x: 0.,
                delta_y: -100000.,
            });
        draw(width, height);
        draw(width, height);
        let dialog_height = ((height as f32) - 60.).min(580.);
        if width == 1180 {
            let pixels = draw(width, height);
            let image = image::RgbaImage::from_fn(width, height, |x, y| {
                let p = pixels[(y * width + x) as usize];
                image::Rgba([p.red, p.green, p.blue, p.alpha])
            });
            let path = std::path::PathBuf::from(std::env::var_os("RUST_EXTERNAL_ROOT").unwrap())
                .join("data/storage-bottom-final.png");
            image.save(path)?;
        }
        let viewport_bottom =
            (height as f32 - dialog_height) / 2. + dialog_height - 22. - 34. - 12.;
        anyhow::ensure!(
            view.get_settings_storage_bottom() <= viewport_bottom + 1.,
            "Storage last delete row is clipped at {width}x{height}: bottom={}, viewport_bottom={viewport_bottom}",
            view.get_settings_storage_bottom()
        );
        anyhow::ensure!(
            view.get_settings_storage_bottom() > 100.,
            "Storage bottom metrics missing"
        );
    }
    // Expanded term chips and theme controls must all reach their actual bottom.
    view.set_blocked_channels(
        (0..35)
            .map(|i| format!("Blocked channel {i}"))
            .collect::<Vec<_>>()
            .join(", ")
            .into(),
    );
    for tab in [0, 1, 2, 3, 4, 5] {
        view.set_settings_tab(tab);
        for _ in 0..6 {
            draw(1180, 600);
        }
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(970., 300.),
                delta_x: 0.,
                delta_y: -100000.,
            });
        for _ in 0..6 {
            draw(1180, 600);
        }
        anyhow::ensure!(
            view.get_settings_body_bottom() <= view.get_settings_viewport_bottom() + 1.
                && view.get_settings_body_bottom() > 100.,
            "Settings tab {tab} cannot reach its bottom: {}, viewport {}, offset {}, content {}",
            view.get_settings_body_bottom(),
            view.get_settings_viewport_bottom(),
            view.get_settings_scroll_offset(),
            view.get_settings_content_height()
        );
    }
    view.set_settings_open(false);
    view.set_music_layout(true);
    let art_size = |pixels: &[PremultipliedRgbaColor], width: usize, height: usize| {
        let x = width - 240;
        let color = |x: usize, y: usize| {
            let p = pixels[y * width + x];
            [p.red, p.green, p.blue, p.alpha]
        };
        let target = color(x, 150);
        let top = (0..150)
            .rev()
            .take_while(|&y| color(x, y) == target)
            .count();
        let bottom = (150..height).take_while(|&y| color(x, y) == target).count();
        let left = (0..x)
            .rev()
            .take_while(|&i| color(i, 150) == target)
            .count();
        let right = (x..width).take_while(|&i| color(i, 150) == target).count();
        [left + right, top + bottom]
    };
    let card_run = |pixels: &[PremultipliedRgbaColor], width: usize, height: usize| {
        let mut longest = 0;
        let mut run = 0;
        for y in 74..height {
            let p = pixels[y * width + width - 240];
            if [p.red, p.green, p.blue] == [43, 39, 33] {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        longest
    };
    let normal_pixels = draw(1180, 780);
    let normal = art_size(&normal_pixels, 1180, 780);
    let tall_pixels = draw(1920, 1080);
    let tall = art_size(&tall_pixels, 1920, 1080);
    anyhow::ensure!(
        normal[0].abs_diff(normal[1]) <= 2 && tall[0].abs_diff(tall[1]) <= 2,
        "Music artwork must remain square: {normal:?} → {tall:?}"
    );
    anyhow::ensure!(
        tall[1] <= 305 && normal[1] >= 160 && normal[1] <= 305,
        "Square art failed to fit the music pane: {normal:?} → {tall:?}"
    );
    let short_pixels = draw(1180, 640);
    let short = art_size(&short_pixels, 1180, 640);
    anyhow::ensure!(
        short[0].abs_diff(short[1]) <= 2 && short[1] < normal[1],
        "Short-screen cover must shrink and remain square: {short:?} vs {normal:?}"
    );
    anyhow::ensure!(
        card_run(&normal_pixels, 1180, 780) < 138 && card_run(&tall_pixels, 1920, 1080) >= 138,
        "Reserved card must hide on short windows and fit on tall windows"
    );
    view.set_music_extra_card(false);
    let hidden_card = draw(1920, 1080);
    anyhow::ensure!(
        card_run(&hidden_card, 1920, 1080) < 138,
        "Disabled music card is still visible"
    );
    view.set_music_extra_card(true);
    // Render real lyric transitions at intermediate frames, then verify the
    // clock stops changing pixels after settling. No player or network needed.
    view.set_lyrics_enabled(true);
    view.set_lyrics_available(true);
    view.set_caption_choice(1);
    view.set_caption_text("First line".into());
    view.set_caption_next("Second line".into());
    view.set_lyric_following("Third line".into());
    draw(1180, 780);
    view.set_lyric_previous("First line".into());
    view.set_caption_text("Second line".into());
    view.set_caption_next("Third line".into());
    view.set_lyrics_animate(true);
    view.set_lyrics_frame(1);
    let initial = draw(1180, 780);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let middle = draw(1180, 780);
    std::thread::sleep(std::time::Duration::from_millis(230));
    let settled = draw(1180, 780);
    std::thread::sleep(std::time::Duration::from_millis(40));
    let idle = draw(1180, 780);
    let differs = |a: &[PremultipliedRgbaColor], b: &[PremultipliedRgbaColor]| {
        a.iter()
            .zip(b)
            .any(|(a, b)| [a.red, a.green, a.blue, a.alpha] != [b.red, b.green, b.blue, b.alpha])
    };
    anyhow::ensure!(
        differs(&initial, &middle) && differs(&middle, &settled),
        "Lyrics skipped intermediate animation frames"
    );
    anyhow::ensure!(
        !differs(&settled, &idle),
        "Stationary lyrics continued repainting"
    );
    let tall_lyrics = draw(1920, 1080);
    for (name, pixels, width, height) in [
        ("normal", normal_pixels, 1180, 780),
        ("tall", tall_pixels, 1920, 1080),
        ("lyrics", settled, 1180, 780),
        ("tall-lyrics", tall_lyrics, 1920, 1080),
    ] {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            let p = pixels[(y * width + x) as usize];
            image::Rgba([p.red, p.green, p.blue, p.alpha])
        });
        image.save(
            std::path::PathBuf::from(std::env::var_os("RUST_EXTERNAL_ROOT").unwrap())
                .join(format!("data/music-layout-{name}-20261022.png")),
        )?;
    }
    view.hide()?;
    Ok(
        serde_json::json!({"initial_native_geometry":true,"picture_scrolls_with_details":true,"native_viewport_clipped":true,"outside_controls_preserve_picture":true,"storage_scrollable":true,"description_scroll_reset":true,"music_art_normal":normal,"music_art_tall":tall,"responsive_music_card":true,"lyrics_animation":true}),
    )
}

// Render the same desktop component above the host monitor's height so the
// responsive extra card and its animated art can be tested without a display change.
#[cfg(feature = "native-overlay-test")]
pub(crate) fn probe_music_skins() -> anyhow::Result<serde_json::Value> {
    use anyhow::Context;
    use std::cell::Cell;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(OffscreenPlatform(Rc::downgrade(&window))))?;
    let view = SetupWindow::new()?;
    view.set_music_layout(true);
    view.set_music_only(true);
    view.set_music_extra_card(true);
    view.set_has_selection(true);
    view.set_selection_playable(true);
    view.set_selected_title("Mochune · Theme study".into());
    view.set_position(88.);
    view.set_duration(216.);
    view.set_clock("1:28 / 3:36".into());
    view.set_volume(70.);
    view.set_selected_id("probe-0".into());
    view.set_collection_title("Local theme fixtures".into());
    // Opaque RGB stripes catch inherited icon colorization on real photo slots.
    let mut photo = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(48, 48);
    for (i, pixel) in photo.make_mut_slice().iter_mut().enumerate() {
        let rgb = match (i % 48) / 16 {
            0 => [237, 17, 37],
            1 => [19, 227, 53],
            _ => [23, 47, 239],
        };
        *pixel = slint::Rgba8Pixel::new(rgb[0], rgb[1], rgb[2], 255);
    }
    let photo = slint::Image::from_rgba8(photo);
    view.set_selected_image(photo.clone());
    let rows: Vec<crate::VideoCard> = (0..80)
        .map(|i| crate::VideoCard {
            source_id: format!("probe-{i}").into(),
            title: format!("Track {i} · A long local fixture title for clipping").into(),
            channel: "Fixture artist".into(),
            duration_text: "3:36".into(),
            available: i != 2,
            music_available: i != 2,
            thumbnail: photo.clone(),
            ..Default::default()
        })
        .collect();
    view.set_videos(slint::ModelRc::new(slint::VecModel::from(rows)));
    view.set_rec_indices(slint::ModelRc::new(slint::VecModel::from(
        (0..80).collect::<Vec<i32>>(),
    )));
    view.set_playlists(slint::ModelRc::new(slint::VecModel::from(
        (0..30)
            .map(|i| crate::PlaylistCard {
                id: format!("playlist-{i}").into(),
                name: format!("Playlist {i}").into(),
                count: "80 songs".into(),
            })
            .collect::<Vec<_>>(),
    )));
    view.set_playlist_names(slint::ModelRc::new(slint::VecModel::from(vec![
        "Playlist one".into(),
        "Playlist two".into(),
    ])));
    view.show()?;
    let draw = || {
        window.set_size(slint::PhysicalSize::new(1180, 1080));
        window.request_redraw();
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![PremultipliedRgbaColor::default(); 1180 * 1080];
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 1180);
        });
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 1180);
        });
        pixels
    };
    let root =
        std::path::PathBuf::from(std::env::var_os("RUST_EXTERNAL_ROOT").context("external root")?)
            .join("data/music-skins-test");
    std::fs::create_dir_all(&root)?;
    let mut results = Vec::new();
    for (kind, name) in [(0, "default"), (1, "pastelduck"), (2, "pixelduck")] {
        let memory = crate::music_skin::preview(&view, kind, 0)?;
        let pixels = draw();
        let image = image::RgbaImage::from_fn(1180, 1080, |x, y| {
            let p = pixels[(y * 1180 + x) as usize];
            image::Rgba([p.red, p.green, p.blue, p.alpha])
        });
        image.save(root.join(format!("{name}-tall.png")))?;
        if kind > 0 {
            crate::music_skin::preview(&view, kind, 3)?;
            let later = draw();
            let changed = pixels
                .iter()
                .zip(later.iter())
                .filter(|(a, b)| {
                    [a.red, a.green, a.blue, a.alpha] != [b.red, b.green, b.blue, b.alpha]
                })
                .count();
            anyhow::ensure!(view.get_skin_card_visible(), "Tall music card was hidden");
            anyhow::ensure!(changed > 50, "GIF produced no visible animation");
            results.push(serde_json::json!({"skin":name,"reserved_artwork_bytes":memory,"changed_pixels":changed,"card_visible":true}));
        }
    }
    crate::music_skin::preview(&view, 0, 0)?;
    let baseline = draw();
    let reference =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("themes/reference/skin.json");
    crate::music_skin::preview_file(&view, &reference)?;
    let themed = draw();
    let changed_sections = [
        ("header", 20usize, 30usize, 45usize, 10usize),
        ("library", 30, 140, 160, 250),
        ("list", 250, 170, 300, 250),
        ("player", 850, 170, 250, 300),
    ]
    .map(|(name, x, y, width, height)| {
        let changed = (y..y + height)
            .flat_map(|y| (x..x + width).map(move |x| y * 1180 + x))
            .filter(|i| {
                let a = baseline[*i];
                let b = themed[*i];
                [a.red, a.green, a.blue, a.alpha] != [b.red, b.green, b.blue, b.alpha]
            })
            .count();
        (name, changed)
    });
    for (name, changed) in changed_sections {
        anyhow::ensure!(changed > 10, "Reference theme did not change {name}");
    }
    image::RgbaImage::from_fn(1180, 1080, |x, y| {
        let p = themed[(y * 1180 + x) as usize];
        image::Rgba([p.red, p.green, p.blue, p.alpha])
    })
    .save(root.join("reference-tall.png"))?;
    view.set_theme_header(false);
    view.set_theme_library(false);
    view.set_theme_list(false);
    view.set_theme_player(false);
    let toggled = draw();
    anyhow::ensure!(
        baseline
            .iter()
            .zip(&toggled)
            .all(|(a, b)| [a.red, a.green, a.blue, a.alpha] == [b.red, b.green, b.blue, b.alpha]),
        "Disabled sections did not restore defaults"
    );
    view.set_theme_header(true);
    view.set_theme_library(true);
    view.set_theme_list(true);
    view.set_theme_player(true);
    crate::music_skin::preview(&view, 0, 0)?;
    let restored = view.global::<crate::MusicSkin>();
    anyhow::ensure!(
        restored.get_font_family().as_str() == "DejaVu Sans"
            && restored.get_title_size() == 17.
            && restored.get_icon_scale() == 1.
            && !restored.get_enabled(),
        "Default skin retained custom typography"
    );
    anyhow::ensure!(
        restored.get_images().iter().all(|i| i.size().width == 0),
        "Default skin retained artwork"
    );
    let mut full_results = Vec::new();
    for name in ["pastelduck", "pixelduck", "crt-full"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("themes/{name}/skin.json"));
        let reserved = crate::music_skin::preview_file(&view, &path)?;
        let full = draw();
        for rgb in [[237, 17, 37], [19, 227, 53], [23, 47, 239]] {
            for (label, left, right) in [("list", 240usize, 808usize), ("album", 824, 1180)] {
                let pixels = full
                    .iter()
                    .enumerate()
                    .filter(|(i, p)| {
                        let x = i % 1180;
                        x >= left && x < right && [p.red, p.green, p.blue] == rgb
                    })
                    .count();
                anyhow::ensure!(
                    pixels > 100,
                    "{name}: {label} photo lost RGB color {rgb:?} ({pixels} pixels)"
                );
            }
        }
        anyhow::ensure!(
            baseline
                .iter()
                .zip(&full)
                .any(|(a, b)| a.red != b.red || a.green != b.green || a.blue != b.blue),
            "{name}-full did not render"
        );
        image::RgbaImage::from_fn(1180, 1080, |x, y| {
            let p = full[(y * 1180 + x) as usize];
            image::Rgba([p.red, p.green, p.blue, p.alpha])
        })
        .save(root.join(format!("{name}-full-tall.png")))?;
        // Exercise actual pointer routing through the populated virtual list.
        use slint::platform::{PointerEventButton, WindowEvent};
        let position = slint::LogicalPosition::new(520., 290.);
        view.window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        let hover = draw();
        view.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        let pressed = draw();
        let changed = |a: &[PremultipliedRgbaColor], b: &[PremultipliedRgbaColor]| {
            a.iter()
                .zip(b)
                .filter(|(a, b)| {
                    a.red != b.red || a.green != b.green || a.blue != b.blue || a.alpha != b.alpha
                })
                .count()
        };
        let hover_pixels = changed(&full, &hover);
        let pressed_pixels = changed(&hover, &pressed);
        anyhow::ensure!(
            hover_pixels > 0 && pressed_pixels > 0,
            "{name}: row hover/pressed did not render ({hover_pixels}/{pressed_pixels})"
        );
        view.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
        view.window().dispatch_event(WindowEvent::PointerExited);
        view.window().dispatch_event(WindowEvent::PointerScrolled {
            position,
            delta_x: 0.,
            delta_y: -500.,
        });
        draw();
        anyhow::ensure!(
            view.get_results_scroll() < -1.,
            "{name}: virtual list did not scroll"
        );
        let scroll = view.get_results_scroll();
        view.set_results_scroll(0.);
        view.set_lyrics_enabled(true);
        view.set_lyrics_available(true);
        view.set_caption_choice(1);
        view.set_caption_text("The current lyric line".into());
        view.set_caption_next("The next lyric line".into());
        view.set_lyric_previous("The previous lyric line".into());
        view.set_lyric_following("The following lyric line".into());
        draw();
        window.set_size(slint::PhysicalSize::new(380, 1080));
        window.request_redraw();
        let mut compact_pixels = vec![PremultipliedRgbaColor::default(); 380 * 1080];
        window.draw_if_needed(|r| {
            r.render(&mut compact_pixels, 380);
        });
        anyhow::ensure!(view.get_compact_music(), "Compact layout did not activate");
        draw();
        view.set_lyrics_enabled(false);
        view.set_lyrics_available(false);
        // Open the music filter via its actual hit area (the separate rec
        // filter signal targets the video browser, not the music toolbar).
        let filter_position = slint::LogicalPosition::new(747., 110.);
        view.window().dispatch_event(WindowEvent::PointerMoved {
            position: filter_position,
        });
        view.window().dispatch_event(WindowEvent::PointerPressed {
            position: filter_position,
            button: PointerEventButton::Left,
        });
        view.window().dispatch_event(WindowEvent::PointerReleased {
            position: filter_position,
            button: PointerEventButton::Left,
        });
        draw();
        view.window().dispatch_event(WindowEvent::KeyPressed {
            text: slint::platform::Key::Escape.into(),
        });
        view.window().dispatch_event(WindowEvent::KeyReleased {
            text: slint::platform::Key::Escape.into(),
        });
        draw();
        full_results.push(serde_json::json!({"theme":name,"reserved_artwork_bytes":reserved,"hover_pixels":hover_pixels,"pressed_pixels":pressed_pixels,"scroll_offset":scroll,"lyrics_compact_menu_rendered":true}));
    }
    // Instrument row-data fetches independently from renderer RSS: an eager
    // repeater over 10,000 entries would exceed this bound immediately.
    struct CountedIndices {
        reads: Cell<usize>,
    }
    impl slint::Model for CountedIndices {
        type Data = i32;
        fn row_count(&self) -> usize {
            10_000
        }
        fn row_data(&self, row: usize) -> Option<i32> {
            self.reads.set(self.reads.get() + 1);
            (row < 10_000).then_some((row % 80) as i32)
        }
        fn model_tracker(&self) -> &dyn slint::ModelTracker {
            &()
        }
    }
    let indices = Rc::new(CountedIndices {
        reads: Cell::new(0),
    });
    view.set_rec_indices(slint::ModelRc::from(indices.clone()));
    draw();
    view.set_results_scroll(-380_000.);
    draw();
    let virtual_reads = indices.reads.get();
    anyhow::ensure!(
        virtual_reads < 2_000,
        "List eagerly read {virtual_reads} rows"
    );
    view.set_results_scroll(0.);
    // Theme data must be inert outside music mode, including the legacy player.
    view.set_music_layout(false);
    let video_themed = draw();
    crate::music_skin::preview(&view, 0, 0)?;
    let video_default = draw();
    anyhow::ensure!(
        video_themed
            .iter()
            .zip(&video_default)
            .all(|(a, b)| [a.red, a.green, a.blue, a.alpha] == [b.red, b.green, b.blue, b.alpha]),
        "Music theme leaked into video mode"
    );
    view.hide()?;
    Ok(
        serde_json::json!({"skins":results,"default_restored":true,"reference_sections":changed_sections,"disabled_sections_restore_default":true,"full_themes":full_results,"video_mode_unchanged":true,"virtual_list":{"rows":10000,"row_data_reads":virtual_reads}}),
    )
}
