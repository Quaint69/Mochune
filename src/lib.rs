use slint::{ComponentHandle, Model};
slint::include_modules!();
#[cfg(any(target_os = "windows", feature = "native-overlay-test"))]
mod native_ui {
    include!(env!("MOCHUNE_NATIVE_UI"));
}
#[cfg(any(target_os = "windows", feature = "native-overlay-test"))]
use native_ui::NativePlayerOverlay;
mod app;
pub mod identity;
mod music_skin;
mod music_theme;
mod window_position;
mod music_window;
mod window_repaint;
#[cfg(target_os = "windows")]
mod window_frame;
pub fn validate_music_theme(path: &std::path::Path) -> anyhow::Result<serde_json::Value> {
    music_skin::validate_file(path)
}
pub mod playlists;
pub use app::{probe_recommendations, probe_youtube};
mod diagnostics;
#[cfg(target_os = "windows")]
pub fn run_video_worker(parent: isize) -> anyhow::Result<()> {
    video::run_video_worker(parent)
}
mod timeline;
mod video;
#[cfg(feature = "native-overlay-test")]
pub fn probe_native_overlay() -> anyhow::Result<serde_json::Value> {
    video::probe_native_overlay()
}
#[cfg(feature = "native-overlay-test")]
pub fn probe_main_layout() -> anyhow::Result<serde_json::Value> {
    video::probe_main_layout()
}
#[cfg(feature="native-overlay-test")]
pub fn probe_music_skins()->anyhow::Result<serde_json::Value>{video::probe_music_skins()}
pub use diagnostics::{initialize as init_diagnostics, record as diagnostics_event};
#[cfg(feature = "showcase")]
#[doc(hidden)]
pub fn apply_showcase_theme(window: &SetupWindow, path: &std::path::Path) -> anyhow::Result<()> {
    music_skin::preview_file(window, path).map(|_| ())
}

fn native_window_icon() -> slint::winit_030::winit::window::Icon {
    let pixels = image::load_from_memory(include_bytes!("../ui/assets/logo-128.png"))
        .expect("bundled window icon")
        .into_rgba8();
    let (width, height) = pixels.dimensions();
    slint::winit_030::winit::window::Icon::from_rgba(pixels.into_raw(), width, height)
        .expect("valid window icon")
}
fn native_window_attributes(
    attributes: slint::winit_030::winit::window::WindowAttributes,
) -> slint::winit_030::winit::window::WindowAttributes {
    let icon = native_window_icon();
    let attributes = attributes.with_window_icon(Some(icon.clone()));
    #[cfg(target_os = "linux")]
    let attributes = attributes.with_transparent(false);
    #[cfg(target_os = "windows")]
    let attributes = {
        use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
        // ICON_BIG is separate from the titlebar's window icon on Windows.
        attributes.with_taskbar_icon(Some(icon))
    };
    attributes
}
// A native resize can preserve old softbuffer pixels even after Slint's layout
// changed. Invalidate the whole desktop only at geometry transitions.
pub(crate) fn repaint_window(window: &slint::Window) {
    use slint::private_unstable_api::re_exports::{LogicalRect, WindowInner};
    let size = window.size().to_logical(window.scale_factor());
    WindowInner::from_pub(window).window_adapter().renderer().mark_dirty_region(
        LogicalRect::from_size((size.width, size.height).into()).into(),
    );
    window.request_redraw();
}

pub fn run_ui(smoke: bool) -> Result<(), slint::PlatformError> {
    #[cfg(target_os = "linux")]
    {
        let selector = slint::BackendSelector::new()
            .backend_name("winit".into())
            .with_winit_window_attributes_hook(native_window_attributes);
        if app::linux_renderer::software_music() {
            selector.renderer_name("software".into()).select()?;
        } else {
            selector.require_opengl_es_with_version(3, 0).select()?;
        }
    }
    #[cfg(target_os = "windows")]
    if video::native_windows_enabled() {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .with_winit_window_attributes_hook(native_window_attributes)
            .select()?;
    } else {
        #[cfg(feature = "legacy-wgpu")]
        {
            let mut gpu = slint::wgpu_30::WGPUSettings::default();
            gpu.backends = slint::wgpu_30::wgpu::Backends::DX12;
            gpu.backend_options.dx12.shader_compiler = slint::wgpu_30::wgpu::Dx12Compiler::Fxc;
            // WGPU defaults to a million D3D12 descriptors, a large up-front
            // driver allocation on integrated GPUs. Our bounded UI needs only a
            // small fraction; allow ample room for glyphs, cards and in-flight draws.
            gpu.device_required_limits.max_non_sampler_bindings = 16_384;
            // Restrict Windows presentation to Direct3D12; no OpenGL backend fallback.
            // UI allocations are small. Keep the allocator's retained per-type blocks
            // bounded; large video surfaces still get dedicated allocations.
            gpu.device_memory_hints = slint::wgpu_30::wgpu::MemoryHints::Manual {
                suballocated_device_memory_block_size: 4 * 1024 * 1024..16 * 1024 * 1024,
            };
            slint::BackendSelector::new()
                .backend_name("winit".into())
                .with_winit_window_attributes_hook(native_window_attributes)
                .require_wgpu_30(slint::wgpu_30::WGPUConfiguration::Automatic(gpu))
                .select()?;
        }
    }
    #[cfg(target_os = "linux")]
    slint::set_xdg_app_id(if std::env::var_os("MOCHUNE_DEV_LAUNCH").is_some() {
        "mochune-dev"
    } else {
        "mochune"
    })?;
    let window = SetupWindow::new()?;
    // An authoring preview deliberately bypasses app::attach: no account,
    // playback worker, library writes, recommendations or network requests.
    let preview_args: Vec<_> = std::env::args().collect();
    if let Some(i) = preview_args.iter().position(|a| a == "--preview-theme") {
        if let Some(path) = preview_args.get(i+1) {
            window.set_theme_preview(true);
            window.set_music_layout(true);
            window.set_music_only(true);
            window.set_music_skin_path(path.into());
            window.set_music_skin_choice(3);
            window.set_has_selection(true);
            window.set_selection_playable(true);
            window.set_selected_id("theme-preview".into());
            window.set_selected_title("Theme preview · A long track title for typography and clipping".into());
            window.set_collection_title("Theme preview · local fixtures".into());
            window.set_status("Preview only · R reload · Space play/pause · Like button changes state".into());
            window.set_duration(240.);
            window.set_position(83.);
            window.set_clock("1:23 / 4:00".into());
            window.set_playlists(slint::ModelRc::new(slint::VecModel::from(vec![PlaylistCard{id:"liked-songs".into(),name:"Liked songs".into(),count:"12 tracks".into()},PlaylistCard{id:"preview".into(),name:"Preview playlist".into(),count:"3 tracks".into()}])));
            let row_count = preview_args.iter().position(|a| a == "--preview-rows")
                .and_then(|i| preview_args.get(i + 1))
                .and_then(|s| s.parse::<usize>().ok()).unwrap_or(3).clamp(3, 100_000);
            if preview_args.iter().any(|a| a == "--preview-static") {
                window.set_skin_animations(false);
            }
            let rows: Vec<VideoCard> = (0..row_count).map(|i| VideoCard{source_id:if i==0 {"theme-preview".into()} else {format!("preview-{i}").into()},title:format!("{} · {i}",["A selected song", "Another song with a considerably longer title", "Unavailable track"][i%3]).into(),channel:"Local preview artist".into(),duration_text:"4:00".into(),available:i!=2,music_available:i!=2,..Default::default()}).collect();
            window.set_videos(slint::ModelRc::new(slint::VecModel::from(rows)));
            window.set_rec_indices(slint::ModelRc::new(slint::VecModel::from((0..row_count as i32).collect::<Vec<_>>())));
            let liked = std::rc::Rc::new(std::cell::Cell::new(false));
            let state = liked.clone(); window.on_song_liked(move |_,_|state.get());
            let weak=window.as_weak();window.on_toggle_like(move |_|{liked.set(!liked.get());if let Some(w)=weak.upgrade(){w.set_likes_revision(w.get_likes_revision().wrapping_add(1));}});
            let weak=window.as_weak();window.on_toggle_pause(move ||{if let Some(w)=weak.upgrade(){w.set_paused(!w.get_paused());}});
            let weak=window.as_weak();window.on_seek(move |v|{if let Some(w)=weak.upgrade(){w.set_position(v);}});
            let weak=window.as_weak();window.on_toggle_music_repeat(move ||{if let Some(w)=weak.upgrade(){w.set_repeat(!w.get_repeat());}});
            let weak=window.as_weak();window.on_toggle_lyrics(move ||{if let Some(w)=weak.upgrade(){w.set_lyrics_enabled(!w.get_lyrics_enabled());}});
            let weak=window.as_weak();window.on_cancel_settings(move ||{if let Some(w)=weak.upgrade(){w.set_settings_open(false);}});
            let _skin = music_skin::install(&window);
            return window.run();
        }
    }
    window.set_timeline_probe(smoke);
    // Native pointer events remain reliable over child buttons and the OS resize border.
    use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
    let resize_repaint = window_repaint::RestoreRedraw::new(&window);
    let chrome_window = window.as_weak();
    let page_snapshot = smoke && std::env::var_os("MOCHUNE_SMOKE_PAGES").is_some();
    window.window().on_winit_window_event(move |native, event| {
        if page_snapshot && matches!(event, WindowEvent::MouseInput {..} | WindowEvent::KeyboardInput {..}
            | WindowEvent::MouseWheel {..} | WindowEvent::Touch(_) | WindowEvent::CursorMoved {..}) {
            return EventResult::PreventDefault;
        }
        if let Some(w) = chrome_window.upgrade() {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    let scale = f64::from(native.scale_factor());
                    let y = position.y / scale;
                    let x = position.x / scale;
                    if smoke && std::env::var_os("MOCHUNE_SMOKE_HOVER_TRACE").is_some() {
                        eprintln!("hover x={:.1} y={:.1} left={} music={} full={} settings={} download={} editor={}",position.x/scale,y,w.get_left_pane_hovered(),w.get_music_layout(),w.get_fullscreen_mode(),w.get_settings_open(),w.get_download_open(),w.get_playlist_editor());
                    }
                    w.set_left_pane_hovered(
                        !w.get_music_layout()
                            && !w.get_fullscreen_mode()
                            && !w.get_settings_open()
                            && !w.get_download_open()
                            && !w.get_playlist_editor()
                            && y >= f64::from(w.get_left_pane_top())
                            && x >= 0.
                            && x < if w.get_left_pane_hovered() { 220. } else { 10. },
                    );
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    resize_repaint.schedule();
                }
                WindowEvent::Focused(true) => {
                    w.set_skin_window_active(true);
                    resize_repaint.schedule();
                }
                WindowEvent::Occluded(value) => {
                    w.set_skin_window_active(!value);
                    if !value { resize_repaint.schedule(); }
                }
                WindowEvent::CursorLeft { .. } | WindowEvent::Focused(false) => {
                    if matches!(event, WindowEvent::Focused(false)) {w.set_skin_window_active(false);}
                    w.set_left_pane_hovered(false);
                }
                _ => {}
            }
        }
        EventResult::Propagate
    });
    music_window::install(&window);
    // Slint updates TouchArea cursor shapes on pointer events. Apply visibility
    // when the idle timer fires too, while the Linux pointer is stationary.
    #[cfg(target_os = "linux")]
    {
        use slint::winit_030::WinitWindowAccessor;
        let weak = window.as_weak();
        window.on_player_cursor_change(move |hidden| {
            if let Some(w) = weak.upgrade() {
                w.window().with_winit_window(|native| native.set_cursor_visible(!hidden));
            }
        });
    }
    let _runtime = app::attach(&window, smoke);
    let startup_video_default = !window.get_music_layout();
    if smoke && std::env::var_os("MOCHUNE_SMOKE_PLAYER_LAYOUT").is_some() {
        window.set_controls_outside(std::env::var_os("MOCHUNE_SMOKE_CONTROLS_OUTSIDE").is_some());
        window.set_legacy_quality(std::env::var_os("MOCHUNE_SMOKE_LEGACY_QUALITY").is_some());
        if let Ok(width) = std::env::var("MOCHUNE_SMOKE_WINDOW_WIDTH") {
            window.window().set_size(slint::LogicalSize::new(
                width.parse().unwrap_or(1180.),
                std::env::var("MOCHUNE_SMOKE_WINDOW_HEIGHT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(780.),
            ));
        }
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
            if let Some(w) = weak.upgrade() {
                if let Ok(text) = std::env::var("MOCHUNE_SMOKE_CAPTION_TEXT") {
                    w.set_caption_text(text.into());
                }
                w.set_controls_shown(true);
                if std::env::var_os("MOCHUNE_SMOKE_LAYOUT_FULLSCREEN").is_some() {
                    w.invoke_toggle_fullscreen();
                }
                if std::env::var_os("MOCHUNE_SMOKE_LAYOUT_CAPTION_MENU").is_some() {
                    w.set_caption_labels(slint::ModelRc::new(slint::VecModel::from(vec![
                        "Captions off".into(),
                        "English · en".into(),
                        "Japanese · ja".into(),
                    ])));
                    w.set_caption_choice(1);
                    w.invoke_show_captions();
                }
                if std::env::var_os("MOCHUNE_SMOKE_LAYOUT_QUALITY_MENU").is_some() {
                    w.set_quality_open_request(w.get_quality_open_request() + 1);
                }
                if std::env::var_os("MOCHUNE_SMOKE_LAYOUT_BUSY_TAB").is_some() {
                    w.set_browse_tab(1);
                    w.set_busy(true);
                }
            }
        });
    }

    if smoke && std::env::var_os("MOCHUNE_SMOKE_ABOUT").is_some() {
        window.set_settings_open(true);
        window.set_settings_tab(4);
        if let Ok(index) = std::env::var("MOCHUNE_SMOKE_ABOUT_LICENSE") {
            window.invoke_show_license(index.parse().unwrap_or(0));
        }
    }

    if smoke && std::env::var_os("MOCHUNE_SMOKE_IMMERSIVE").is_some() {
        window.set_immersive(true);
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(1), move || {
            if let Some(w) = weak.upgrade() {
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                        position: slint::LogicalPosition::new(100., 10.),
                    });
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_DESCRIPTION_LAYOUT").is_some() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
            if let Some(w) = weak.upgrade() {
                w.set_description("Description line with enough detail to read while the player retains its size.\n".repeat(80).into());
                w.set_description_expanded(false);
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_DESCRIPTION_LINKS").is_some() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
            if let Some(w) = weak.upgrade() {
                w.set_description("Description line with selectable text.\nVisit https://example.com/watch and https://example.org/info.".into());
                w.set_description_expanded(true);
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_DESCRIPTION_COPY").is_some() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(4), move || {
            if let Some(w) = weak.upgrade() {
                w.set_description_selection_start(12);
                w.set_description_selection_end(16);
                w.set_description_selection_request(1);
                let weak = w.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_description_copy_request(1);
                    }
                });
            }
        });
    }
    let seek_results = std::rc::Rc::new(std::cell::RefCell::new(Vec::<serde_json::Value>::new()));
    let automation = slint::Timer::default();
    let mode_automation = slint::Timer::default();
    let feature_automation = slint::Timer::default();
    let session_automation = slint::Timer::default();
    let feature_results =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::<serde_json::Value>::new()));
    if smoke && std::env::var_os("MOCHUNE_SMOKE_REPEAT_SONG").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        let stage = std::cell::Cell::new(0);
        let previous = std::cell::Cell::new(0f32);
        feature_automation.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(100), move || {
            let Some(w) = weak.upgrade() else { return; };
            match stage.get() {
                0 if w.get_music_layout() && w.get_playlists().row_count() > 0 => { w.invoke_playlist_select("handoff-test".into()); stage.set(1); }
                1 if w.get_playlist_id() == "handoff-test" && w.get_videos().row_count() == 2 => {
                    w.invoke_toggle_music_repeat(); w.invoke_toggle_music_repeat();
                    results.borrow_mut().push(serde_json::json!({"repeat_one_selected":w.get_repeat_one() && !w.get_repeat()}));
                    w.invoke_select(0); stage.set(2);
                }
                2 if previous.get() > 3. && w.get_position() < 1. && w.get_playback_active() && !w.get_resolving() => {
                    results.borrow_mut().push(serde_json::json!({"repeat_same_song":w.get_selected_id()=="aaaaaaaaaaa"}));
                    w.invoke_next(false); stage.set(3);
                }
                3 if w.get_selected_id()=="bbbbbbbbbbb" && w.get_playback_active() && !w.get_resolving() => {
                    results.borrow_mut().push(serde_json::json!({"manual_next_skips_repeat_one":true}));
                    w.invoke_toggle_music_repeat();
                    results.borrow_mut().push(serde_json::json!({"repeat_cycles_off":!w.get_repeat_one() && !w.get_repeat()}));
                    stage.set(4);
                }
                _ => {}
            }
            previous.set(previous.get().max(w.get_position()));
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_MUSIC_HANDOFF").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        let stage = std::cell::Cell::new(0usize);
        let previous = std::cell::RefCell::new(String::new());
        feature_automation.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(100), move || {
            let Some(w) = weak.upgrade() else { return; };
            match stage.get() {
                0 if w.get_music_layout() && w.get_playlists().row_count() > 0 => {
                    w.invoke_playlist_select("start-test".into()); stage.set(10);
                }
                10 if w.get_playlist_id() == "start-test" && w.get_videos().row_count() > 0 => { w.invoke_select(0); stage.set(11); }
                11 if w.get_playback_active() && w.get_position() > 1. => {
                    *previous.borrow_mut() = w.get_selected_id().to_string();
                    w.invoke_playlist_select("handoff-test".into()); stage.set(1);
                }
                1 if w.get_playlist_id() == "handoff-test" && w.get_videos().row_count() == 2 => {
                    results.borrow_mut().push(serde_json::json!({"playlist_keeps_playing":w.get_playback_active() && w.get_selected_id().as_str() == previous.borrow().as_str() && w.get_position() > 1.}));
                    w.set_repeat(true); stage.set(2);
                }
                2 if w.get_selected_id() == "aaaaaaaaaaa" && w.get_playback_active() => { results.borrow_mut().push(serde_json::json!({"handoff_first":true})); stage.set(3); }
                3 if w.get_selected_id() == "bbbbbbbbbbb" && w.get_playback_active() => { results.borrow_mut().push(serde_json::json!({"handoff_second":true})); stage.set(4); }
                4 if w.get_selected_id() == "aaaaaaaaaaa" && w.get_playback_active() => { results.borrow_mut().push(serde_json::json!({"repeat_playlist":true})); stage.set(5); }
                _ => {}
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_COMPACT_MUSIC").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
            if let Some(w) = weak.upgrade() {
                w.window().set_maximized(false);
                w.window().set_size(slint::LogicalSize::new(500., 720.));
            }
            slint::Timer::single_shot(std::time::Duration::from_secs(1), move || {
                if let Some(w) = weak.upgrade() {
                    results.borrow_mut().push(serde_json::json!({"compact":w.get_compact_music(),"logical_width":w.window().size().width as f32 / w.window().scale_factor()}));
                }
            });
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_MUSIC_DISCOVERY").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        let stage = std::cell::Cell::new(0usize);
        feature_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(250),move || {
            let Some(w)=weak.upgrade() else{return;};
            match stage.get() {
                0 if w.get_music_layout() && w.get_playlists().row_count()>0 => {w.invoke_playlist_select("liked-songs".into());stage.set(1);}
                1 if w.get_playlist_id()=="liked-songs" && w.get_videos().row_count()>1 => {
                    w.invoke_select(0);stage.set(11);
                }
                11 if w.get_playback_active() && !w.get_resolving() => {
                    w.set_playlist_id("".into());w.set_browse_tab(-1);w.invoke_select(1);stage.set(2);
                }
                2 if w.get_browse_tab()==0 && w.get_busy() => {results.borrow_mut().push(serde_json::json!({"search_handoff_after_playback":w.get_playback_active()}));stage.set(3);}
                3 if w.get_browse_tab()==0 && !w.get_busy() && w.get_videos().row_count()>0 => {
                    results.borrow_mut().push(serde_json::json!({"discovery_rows":w.get_videos().row_count(),"queue_rows":w.get_queue_rows().row_count(),"current":w.get_selected_id().as_str()}));
                    w.invoke_next(false);stage.set(4);
                }
                4 if !w.get_resolving() && w.get_selected_id()!="mLW35YMzELE" => {results.borrow_mut().push(serde_json::json!({"continued_into_discovery":w.get_selected_id().as_str(),"tab":w.get_browse_tab(),"busy":w.get_busy()}));stage.set(5);}
                _=>{}
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_MUSIC_REFINEMENTS").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        let stage = std::cell::Cell::new(0usize);
        let identity = std::cell::Cell::new(0usize);
        feature_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(250), move || {
            let Some(w)=weak.upgrade() else {return;};
            let pointer=|| w.get_videos().as_any() as *const dyn std::any::Any as *const () as usize;
            match stage.get() {
                0 if w.get_music_layout() && w.get_playlists().row_count()>0 => {
                    identity.set(pointer()); w.invoke_playlist_select("liked-songs".into()); stage.set(1);
                }
                1 if w.get_playlist_id()=="liked-songs" && w.get_videos().row_count()>0 => {
                    results.borrow_mut().push(serde_json::json!({"selection_does_not_autoplay":!w.get_playback_active(),"model_reused":pointer()==identity.get()}));
                    w.invoke_select(0);stage.set(11);
                }
                11 if w.get_playback_active() && !w.get_resolving() => {
                    results.borrow_mut().push(serde_json::json!({"explicit_first":w.get_selected_id().as_str(),"model_reused":pointer()==identity.get(),"tab":w.get_browse_tab()}));
                    w.invoke_next(false); stage.set(2);
                }
                2 if w.get_selected_id()=="lmnopqrstuv" && !w.get_resolving() => {
                    results.borrow_mut().push(serde_json::json!({"next":w.get_selected_id().as_str()}));
                    w.set_music_duration(1);w.set_music_sort(2);w.invoke_rebuild_recs(); stage.set(3);
                }
                3 => {
                    results.borrow_mut().push(serde_json::json!({"filtered":w.get_rec_indices().iter().collect::<Vec<_>>(),"model_reused":pointer()==identity.get()}));
                    w.set_music_duration(0);w.set_music_sort(0);w.invoke_playlist_remove(0);stage.set(4);
                }
                4 if w.get_videos().row_count()==1 => {
                    results.borrow_mut().push(serde_json::json!({"removed_first":w.get_videos().row_data(0).map(|v|v.source_id.to_string()),"model_reused":pointer()==identity.get()}));
                    w.set_audio_cache_enabled(true);w.invoke_save_settings();w.set_cache_status("Saving compressed audio as it arrives…".into());
                    w.set_audio_cache_enabled(false);w.invoke_save_settings(); stage.set(5);
                }
                5 => {
                    results.borrow_mut().push(serde_json::json!({"cache_off":!w.get_audio_cache_enabled(),"cache_status":w.get_cache_status().as_str()}));
                    stage.set(6);
                }
                _=>{}
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_CONTROL_PLACEMENT").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        let stage = std::cell::Cell::new(0usize);
        feature_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(600),move || {
            let Some(w)=weak.upgrade() else{return;};let step=stage.get();if step>3{return;}
            results.borrow_mut().push(serde_json::json!({"step":step,"outside":w.get_controls_outside(),"overlay":w.get_controls_over_video(),"fullscreen":w.get_fullscreen_mode()}));
            match step {0=>w.invoke_toggle_controls_position(),1=>w.invoke_toggle_fullscreen(),2=>{w.invoke_toggle_controls_position();w.invoke_toggle_fullscreen();},_=>{}}
            stage.set(step+1);
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_REC_VIEW").is_some() {
        window.invoke_history();
        let weak = window.as_weak();
        let results = feature_results.clone();
        let stage = std::cell::Cell::new(0usize);
        feature_automation.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(150), move || {
            let Some(w) = weak.upgrade() else {return;};
            if w.get_busy() || w.get_videos().row_count() == 0 {return;}
            let step=stage.get();
            if step > 4 {return;}
            results.borrow_mut().push(serde_json::json!({"step":step,"rows":w.get_rec_indices().iter().collect::<Vec<_>>()}));
            match step {
                0 => {w.set_history_view(false);w.set_rec_duration(2);w.set_rec_sort(2);}
                1 => {if let Some(i)=w.get_rec_indices().row_data(0) {w.invoke_queue_add(i);} w.set_rec_duration(0);w.set_rec_sort(1);}
                2 => {w.set_rec_duration(1);}
                3 => {w.set_history_view(true);}
                4 => {w.set_history_view(false);w.set_rec_duration(0);w.set_rec_sort(0);
                    if std::env::var_os("MOCHUNE_SMOKE_REC_MENU").is_some() {w.set_rec_filter_open(1);}
                }
                _=>{}
            }
            stage.set(step+1);
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_EARLY_SEEK").is_some() {
        let weak=window.as_weak();
        let results=feature_results.clone();
        let step=std::cell::Cell::new(0usize);
        let pending=std::cell::Cell::new(None::<(std::time::Instant,f32)>);
        feature_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(50),move || {
            let Some(w)=weak.upgrade() else {return;};
            if step.get()>=8 || w.get_position()<2. || w.get_busy() {return;}
            if let Some((at,target))=pending.get() {
                if !w.get_resolving() && at.elapsed().as_millis()>800
                    && crate::video::video_position()>=target as f64-0.15
                    && (crate::video::video_position()-w.get_position() as f64).abs()<0.5 {
                    crate::diagnostics::record("early_seek_settled",serde_json::json!({"step":step.get(),"target":target,"wall_ms":at.elapsed().as_millis(),"video":crate::video::video_position(),"position":w.get_position()}));
                    step.set(step.get()+1);pending.set(None);
                }
                return;
            }
            if w.get_resolving() {return;}
            let target=w.get_position()+5.;
            let pointer=step.get()==4;
            crate::diagnostics::record("early_seek_input",serde_json::json!({"step":step.get(),"input":if pointer {"pointer"} else {"keyboard"},"from":w.get_position(),"target":target,"ranges":w.get_buffered_ranges().iter().filter(|r|r.end>r.start).map(|r|serde_json::json!({"start":r.start,"end":r.end,"estimated":r.estimated})).collect::<Vec<_>>()}));
            results.borrow_mut().push(serde_json::json!({"step":step.get(),"target":target,"pointer":pointer}));
            if pointer {smoke_seek(&w,target);} else {
                use slint::platform::{Key,WindowEvent};
                w.window().dispatch_event(WindowEvent::KeyPressed{text:Key::RightArrow.into()});
                w.window().dispatch_event(WindowEvent::KeyReleased{text:Key::RightArrow.into()});
            }
            pending.set(Some((std::time::Instant::now(),target)));
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_HOME_SCROLL").is_some() {
        for step in 0..10 {
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(3000 + step * 900), move || {
                if let Some(w) = weak.upgrade() {
                    w.window().dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(600., 450.), delta_x: 0., delta_y: -3000.,
                    });
                }
            });
        }
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_SEEK_SPAM").is_some() {
        let forward_only=std::env::var_os("MOCHUNE_SMOKE_SEEK_FORWARD").is_some();
        let end=if forward_only {52}else{42};
        let followup=forward_only && std::env::var_os("MOCHUNE_SMOKE_SEEK_TAIL").is_some();
        let tail=std::cell::Cell::new(0usize);
        let paced=forward_only && std::env::var_os("MOCHUNE_SMOKE_SEEK_PACED").is_some();
        let awaiting=std::cell::Cell::new(None::<f32>);
        let weak = window.as_weak();
        let results = feature_results.clone();
        let stage = std::cell::Cell::new(0usize);
        let started = std::cell::Cell::new(None::<std::time::Instant>);
        feature_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(25),move || {
            let Some(w)=weak.upgrade() else {return;};let step=stage.get();
            if step>end && !(followup && step==end+1) {return;}
            if !w.get_player_error().is_empty() {
                let error=w.get_player_error().to_string();
                results.borrow_mut().push(serde_json::json!({"stopped_on_error":true,"step":step,"error":error}));
                crate::diagnostics::record("seek_stress_stopped",serde_json::json!({"step":step,"error":error}));
                stage.set(end+2);return;
            }
            if step==end+1 && followup {
                if tail.get()>=10 || started.get().is_none_or(|t|t.elapsed().as_millis()<if tail.get()==0 {9000}else{100}) {return;}
                let before=w.get_position();
                w.window().dispatch_event(slint::platform::WindowEvent::KeyPressed{text:slint::platform::Key::RightArrow.into()});
                w.window().dispatch_event(slint::platform::WindowEvent::KeyReleased{text:slint::platform::Key::RightArrow.into()});
                crate::diagnostics::record("seek_stress_followup",serde_json::json!({"step":tail.get(),"before":before,"target":w.get_position()}));
                results.borrow_mut().push(serde_json::json!({"followup":tail.get(),"before":before,"after":w.get_position()}));
                tail.set(tail.get()+1);started.set(Some(std::time::Instant::now()));return;
            }
            if step==0 && w.get_position()>2. && !w.get_resolving() {w.invoke_seek(10.);stage.set(1);return;}
            if step==1 && !w.get_resolving() && w.get_position()>9. {stage.set(2);return;}
            if (2..end).contains(&step) {
                if paced && let Some(target)=awaiting.get() {
                    if started.get().is_some_and(|t|t.elapsed().as_millis()<100)
                        || w.get_resolving() || crate::video::video_position()<target as f64-0.15
                        || (crate::video::video_position()-w.get_position() as f64).abs()>0.5 {return;}
                }
                let forward=forward_only || step%2==0;let before=w.get_position();
                let key=if forward {slint::platform::Key::RightArrow}else{slint::platform::Key::LeftArrow};
                w.window().dispatch_event(slint::platform::WindowEvent::KeyPressed {text:key.into()});
                w.window().dispatch_event(slint::platform::WindowEvent::KeyReleased {text:key.into()});
                results.borrow_mut().push(serde_json::json!({"step":step,"before":before,"after":w.get_position(),"delta":if forward{5.}else{-5.}}));
                awaiting.set(Some(w.get_position()));
                stage.set(step+1);started.set(Some(std::time::Instant::now()));
            } else if step==end && !w.get_resolving() && started.get().is_some_and(|t|t.elapsed().as_secs_f64()>2.) {
                results.borrow_mut().push(serde_json::json!({"settled":true,"position":w.get_position(),"video_position":crate::video::video_position(),"paused":w.get_paused()}));stage.set(end+1);started.set(Some(std::time::Instant::now()));
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_DESCRIPTION_LAYOUT").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
            if let Some(w) = weak.upgrade() {
                let before = w.get_player_render_height();
                w.set_description_expanded(true);
                let weak = w.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
                    if let Some(w) = weak.upgrade() {
                        results.borrow_mut().push(serde_json::json!({"player_height_before":before,"player_height_after":w.get_player_render_height(),"page_content_height":w.get_page_content_height(),"page_visible_height":w.get_page_visible_height()}));
                    }
                });
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_DESCRIPTION_LAYOUT").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        slint::Timer::single_shot(std::time::Duration::from_secs(4), move || {
            if let Some(w) = weak.upgrade() {
                let before = (w.get_native_player_y(), w.get_player_render_height());
                w.set_page_scroll(-400.);
                let weak = w.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
                    if let Some(w) = weak.upgrade() {
                        results.borrow_mut().push(serde_json::json!({"picture_scrolls_with_details":w.get_native_player_y() < before.0 && (w.get_player_render_height()-before.1).abs()<1.,"native_viewport_clipped":w.get_native_clip_top()>w.get_native_player_y(),"metadata_scroll":w.get_page_scroll()}));
                    }
                });
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_DESCRIPTION_LAYOUT").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        slint::Timer::single_shot(std::time::Duration::from_secs(5), move || {
            if let Some(w) = weak.upgrade() {
                w.set_page_scroll(-400.);
                w.set_description_expanded(false);
                let weak = w.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
                    if let Some(w) = weak.upgrade() {
                        results.borrow_mut().push(serde_json::json!({"collapsed_scroll_reset":w.get_page_scroll()==0.,"collapsed_scroll":w.get_page_scroll()}));
                    }
                });
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_ISOLATED_MODES").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        let mut stage = 0;
        let mut video_id = String::new();
        let mut music_id = String::new();
        let mut resume = 0.;
        session_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(150),move||{
            let Some(w)=weak.upgrade()else{return;};
            match stage {
                0 if w.get_playback_active()&&w.get_position()>2.=>{video_id=w.get_selected_id().to_string();resume=w.get_position();w.invoke_mode_change(true);stage=1;},
                1 if w.get_music_layout()&&!w.get_resolving()=>{results.borrow_mut().push(serde_json::json!({"music_did_not_inherit":!w.get_playback_active()&&w.get_position()==0.&&w.get_selected_id().as_str()!=video_id}));w.invoke_history();stage=2;},
                2 if w.get_history_view()&&!w.get_busy()&&w.get_videos().row_count()>0=>{w.invoke_select(0);stage=3;},
                3 if w.get_playback_active()&&w.get_position()>1.=>{music_id=w.get_selected_id().to_string();w.invoke_mode_change(false);stage=4;},
                4 if !w.get_music_layout()&&!w.get_resolving()=>{results.borrow_mut().push(serde_json::json!({"video_restored_paused":!w.get_playback_active()&&w.get_paused()&&w.get_selected_id().as_str()==video_id&&w.get_position()>=resume}));w.invoke_toggle_pause();stage=5;},
                5 if w.get_playback_active()&&w.get_video_visible()&&w.get_position()>resume+0.4=>{w.invoke_mode_change(true);stage=6;},
                6 if w.get_music_layout()&&!w.get_resolving()=>{results.borrow_mut().push(serde_json::json!({"music_restored_own_track":!w.get_playback_active()&&w.get_paused()&&w.get_selected_id().as_str()==music_id&&w.get_position()>=1.}));w.invoke_toggle_pause();stage=7;},
                7 if w.get_playback_active()&&w.get_position()>1.5&&!w.get_video_visible()=>{results.borrow_mut().push(serde_json::json!({"isolated_modes_passed":w.get_active_decoder().is_empty()}));stage=9;},
                _=>{}
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_RAPID_MODES").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        let mut stage = 0;
        let mut baseline = 0.;
        session_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(150),move||{
            let Some(w)=weak.upgrade()else{return;};
            if stage==0 && w.get_playback_active() && w.get_position()>1. {
                w.invoke_mode_change(true);w.invoke_mode_change(false);
                w.invoke_select(1);
                results.borrow_mut().push(serde_json::json!({"immediate_restored_selection":w.get_selected_id().as_str()=="second-test"}));stage=1;
            } else if stage==1 && w.get_playback_active() && w.get_video_visible() && w.get_position()>0.5 {
                baseline=w.get_position();w.invoke_mode_change(true);w.invoke_mode_change(false);w.invoke_toggle_pause();stage=2;
            } else if stage==2 && w.get_playback_active() && w.get_video_visible() && w.get_position()>baseline+0.4 {
                results.borrow_mut().push(serde_json::json!({"rapid_modes_playback_recovered":!w.get_music_layout()&&w.get_selected_id().as_str()=="second-test"}));stage=9;
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_FEATURES").is_some() {
        let weak = window.as_weak();
        let results = feature_results.clone();
        let mut stage = 0;
        let started = std::time::Instant::now();
        let download = std::env::var("MOCHUNE_SMOKE_DOWNLOAD").ok();
        let history = std::env::var_os("MOCHUNE_SMOKE_HISTORY_REMOVE").is_some();
        let captions = std::env::var_os("MOCHUNE_SMOKE_CAPTIONS").is_some();
        let lyrics_states = std::env::var_os("MOCHUNE_SMOKE_LYRICS_STATES").is_some();
        let cancel = std::env::var_os("MOCHUNE_SMOKE_DOWNLOAD_CANCEL").is_some();
        let mut cancel_at = None;
        feature_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(200),move|| {
            let Some(w)=weak.upgrade() else{return;};
            if lyrics_states {
                let record=||serde_json::json!({"stage":stage,"enabled":w.get_lyrics_enabled(),"available":w.get_lyrics_available(),"choice":w.get_caption_choice(),"language":w.get_lyrics_language_label().as_str()});
                match stage {
                    0 if w.get_lyrics_available() && !w.get_caption_text().is_empty() => {results.borrow_mut().push(record());w.invoke_toggle_lyrics();stage=1;}
                    1 if !w.get_lyrics_enabled() && w.get_caption_choice()==0 => {results.borrow_mut().push(record());w.invoke_toggle_lyrics();stage=2;}
                    2 if w.get_lyrics_enabled() && !w.get_caption_text().is_empty() => {results.borrow_mut().push(record());w.invoke_reset_captions();stage=3;}
                    3 if !w.get_lyrics_available() && w.get_lyrics_enabled() => {results.borrow_mut().push(record());w.invoke_toggle_lyrics();stage=4;}
                    4 if !w.get_lyrics_available() && !w.get_lyrics_enabled() => {results.borrow_mut().push(record());w.invoke_toggle_lyrics();stage=5;}
                    5 if !w.get_lyrics_available() && w.get_lyrics_enabled() => {results.borrow_mut().push(record());stage=9;}
                    _=>{}
                }
            } else if history {
                if stage==0 {w.invoke_history();stage=1;}
                else if stage==1 && w.get_history_view() && !w.get_busy() {
                    let before=w.get_videos().row_count();
                    if let Some(row)=w.get_videos().row_data(0){w.invoke_remove_history(row.source_id);}
                    results.borrow_mut().push(serde_json::json!({"history_before":before,"history_after":w.get_videos().row_count()}));stage=9;
                }
            } else if let Some(ref mode)=download {
                if stage==0 && !w.get_resolving() && w.get_duration()>0. && w.get_quality_labels().row_count()>1 {results.borrow_mut().push(serde_json::json!({"opened_download_at":started.elapsed().as_secs_f64()}));if std::env::var_os("MOCHUNE_SMOKE_HEADER_DOWNLOAD").is_some(){
                    use slint::platform::{WindowEvent,PointerEventButton};
                    let point=slint::LogicalPosition::new(w.window().size().width as f32/w.window().scale_factor()-86.,38.);
                    w.window().dispatch_event(WindowEvent::PointerPressed{position:point,button:PointerEventButton::Left});
                    w.window().dispatch_event(WindowEvent::PointerReleased{position:point,button:PointerEventButton::Left});
                }else{w.invoke_open_download();}stage=1;}
                else if stage==1 && w.get_download_ready() {
                    let labels=w.get_download_labels();
                    let index=if mode=="audio" {labels.iter().position(|s|s.to_lowercase().contains("audio"))}else{labels.iter().position(|s|s.contains("360p"))}.unwrap_or(0);
                    results.borrow_mut().push(serde_json::json!({"download_choice":index,"ready_at":started.elapsed().as_secs_f64()}));w.set_download_choice(index as i32);w.invoke_start_download();results.borrow_mut().push(serde_json::json!({"download_popover_closed":!w.get_download_open()}));if std::env::var_os("MOCHUNE_SMOKE_BACKGROUND_DOWNLOAD").is_some(){w.invoke_history();}stage=2;
                } else if stage==2 {
                    if cancel && w.get_download_running() && w.get_download_progress()>0. {cancel_at.get_or_insert(std::time::Instant::now());}
                    if cancel_at.is_some_and(|t|t.elapsed().as_millis()>200){w.invoke_cancel_download();stage=3;}
                    if !w.get_download_running(){results.borrow_mut().push(serde_json::json!({"download_status":w.get_download_status().as_str(),"history_interaction":w.get_history_view()}));stage=9;}
                }
            } else if captions && stage==0 && w.get_caption_labels().row_count()>1 {
                w.set_description_expanded(true);w.invoke_choose_caption(1);stage=1;
            }
            if captions && stage==1 && !w.get_caption_text().is_empty() {
                results.borrow_mut().push(serde_json::json!({"caption":w.get_caption_text().as_str(),"at":w.get_position(),"after_seconds":started.elapsed().as_secs_f64()}));stage=9;
            }
        });
    }

    if smoke && std::env::var_os("MOCHUNE_SMOKE_FRESH_HOME").is_some() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
            let Some(w) = weak.upgrade() else { return };
            assert!(w.get_music_layout(), "Fresh installs must start in Music");
            w.invoke_mode_change(false);
            let p = w.global::<BrowsePages>();
            assert_eq!(p.get_page(), 1, "First mode switch must open Home");
            assert!(!p.get_busy(), "Seedless Home must settle without a helper request");
            assert_eq!(p.get_rows().row_count(), 0, "Fresh Home must not retain loading slots");
            w.invoke_toggle_window_maximized();
            let weak = w.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(600), move || {
                if let Some(w) = weak.upgrade() {
                    w.window().set_maximized(false);
                    w.window().set_size(slint::LogicalSize::new(1060., 720.));
                    println!("Fresh Music-to-Video Home settled without a second Home click; restore repaint scheduled");
                }
            });
        });
    }

    if smoke && std::env::var_os("MOCHUNE_SMOKE_MODE_SWITCH").is_some() {
        let weak = window.as_weak();
        let started = std::time::Instant::now();
        let stage = std::cell::Cell::new(0);
        mode_automation.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(250),
            move || {
                let steps = [(8, false), (18, true), (28, false), (38, true)];
                let index = stage.get();
                if index < steps.len()
                    && started.elapsed().as_secs() >= steps[index].0
                    && let Some(w) = weak.upgrade()
                {
                    w.invoke_mode_change(steps[index].1);
                    stage.set(index + 1);
                }
            },
        );
    }
    let paused_mode_automation = slint::Timer::default();
    if smoke && std::env::var_os("MOCHUNE_SMOKE_PAUSED_MODE").is_some() {
        let weak = window.as_weak();
        let stage = std::cell::Cell::new(0);
        paused_mode_automation.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(250),
            move || {
                if let Some(w) = weak.upgrade() {
                    match stage.get() {
                        0 if w.get_position() > 3. && !w.get_resolving() => {
                            w.invoke_toggle_pause();
                            stage.set(1);
                        }
                        1 if w.get_paused() => {
                            w.invoke_mode_change(false);
                            stage.set(2);
                        }
                        2 if w.get_video_visible() && !w.get_resolving() && w.get_paused() => {
                            w.invoke_mode_change(true);
                            stage.set(3);
                        }
                        3 if !w.get_resolving() && w.get_paused() && w.get_music_only() => {
                            w.set_status("Paused mode switch passed".into());
                            stage.set(4);
                        }
                        _ => {}
                    }
                }
            },
        );
    }
    let eos_automation = slint::Timer::default();
    if smoke && std::env::var_os("MOCHUNE_SMOKE_QUEUE_EOS").is_some() {
        let weak = window.as_weak();
        let stage = std::cell::Cell::new(0);
        eos_automation.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                if let Some(w) = weak.upgrade() {
                    match stage.get() {
                        0 if w.get_playlists().row_count() > 0 => {
                            w.invoke_playlist_import();
                            stage.set(1);
                        }
                        1 if w.get_status().starts_with("Imported") => {
                            w.invoke_playlist_select("liked-songs".into());
                            stage.set(2);
                        }
                        2 if w.get_videos().row_count() == 2 => {
                            w.set_repeat(true);
                            w.invoke_select(0);
                            stage.set(3);
                        }
                        3 if w.get_selected_title() == "Second song" => {
                            stage.set(4);
                        }
                        4 if w.get_selected_title() == "First song" => {
                            w.set_status("Queue EOS and repeat passed".into());
                            stage.set(5);
                        }
                        _ => {}
                    }
                }
            },
        );
    }
    let video_queue_automation = slint::Timer::default();
    if smoke && std::env::var_os("MOCHUNE_SMOKE_VIDEO_QUEUE_EOS").is_some() {
        let weak = window.as_weak();
        let stage = std::cell::Cell::new(0);
        let checks = feature_results.clone();
        video_queue_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(100),move||{
            let Some(w)=weak.upgrade()else{return;};
            if w.get_resolving() { return; }
            match stage.get() {
                0 if w.get_position()>0.3 && w.get_duration()>2. => {
                    w.set_repeat_video(false);
                    w.invoke_queue_add(1);
                    if !w.get_fullscreen_mode() {w.invoke_toggle_fullscreen();}
                    w.invoke_seek(w.get_duration()-0.6);
                    stage.set(1);
                },
                1 if w.get_selected_id()=="second-test" && w.get_position()>0.3 && w.get_playback_active()=>{
                    checks.borrow_mut().push(serde_json::json!({"fullscreen_queue_autoplay":w.get_fullscreen_mode() && w.get_queue_rows().row_count()==1 && w.get_queue_rows().row_data(0).is_some_and(|r|r.active && r.title=="Second video")}));
                    w.invoke_seek(w.get_duration()-0.6);
                    stage.set(2);
                },
                2 if w.get_queue_rows().row_count()==0 && !w.get_playback_active()=>{
                    checks.borrow_mut().push(serde_json::json!({"queue_stops_when_empty":true}));
                    stage.set(3);
                },_=>{}
            }
        });
    }
    let gesture_automation = slint::Timer::default();
    if smoke && std::env::var_os("MOCHUNE_SMOKE_GESTURES").is_some() {
        let weak = window.as_weak();
        let stage = std::cell::Cell::new(0);
        let started = std::cell::Cell::new(None::<std::time::Instant>);
        let results = seek_results.clone();
        let target = std::cell::Cell::new(0.);
        gesture_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(50),move||{if let Some(w)=weak.upgrade(){
            let s=stage.get();
            if (s==0||s==2)&&w.get_position()>6.&&!w.get_resolving(){
                let region=if s==0{0.85}else{0.15};target.set((w.get_position()+if s==0{5.}else{-5.}).clamp(0.,w.get_duration()));
                let width=w.window().size().width as f32/w.window().scale_factor()-427.;let x=20.+width*region;let y=70.+width*9./32.;
                for _ in 0..2{w.window().dispatch_event(slint::platform::WindowEvent::PointerPressed{position:slint::LogicalPosition::new(x,y),button:slint::platform::PointerEventButton::Left});w.window().dispatch_event(slint::platform::WindowEvent::PointerReleased{position:slint::LogicalPosition::new(x,y),button:slint::platform::PointerEventButton::Left});}
                started.set(Some(std::time::Instant::now()));stage.set(s+1);
            }else if (s==1||s==3)&&!w.get_resolving()&&let Some(t)=started.get(){
                if t.elapsed().as_millis()>25 && !w.get_paused() && (w.get_position()-target.get()).abs()<1. && (crate::video::video_position()-target.get() as f64).abs()<1. {results.borrow_mut().push(serde_json::json!({"target":target.get(),"position":w.get_position(),"paused":w.get_paused(),"video_position":crate::video::video_position(),"latency_ms":t.elapsed().as_secs_f64()*1000.}));stage.set(s+1);}
            }
        }});
    }
    let library_automation = slint::Timer::default();
    let isolation_automation = slint::Timer::default();
    if smoke && std::env::var_os("MOCHUNE_SMOKE_VIDEO_LIBRARY").is_some() {
        let weak = window.as_weak();
        let stage = std::cell::Cell::new(0);
        let checks = feature_results.clone();
        isolation_automation.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(150),move||{
            let Some(w)=weak.upgrade()else{return;};
            match stage.get(){
                0 if !w.get_music_layout() && w.get_playlists().row_count()==2=>{
                    checks.borrow_mut().push(serde_json::json!({"video_no_default":w.get_save_to()==-1 && w.get_video_playlist_id().is_empty()}));
                    w.invoke_playlist_select("video-one".into());stage.set(1);
                },
                1 if w.get_video_playlist_rows().row_count()==2=>{
                    checks.borrow_mut().push(serde_json::json!({"playlist_does_not_replace_queue":w.get_queue_rows().row_count()==0}));
                    w.invoke_playlist_enqueue(0);w.invoke_playlist_enqueue(1);stage.set(2);
                },
                2 if w.get_queue_rows().row_count()==2=>{w.invoke_play();stage.set(3);},
                3 if w.get_position()>0. && w.get_queue_rows().row_count()==3=>{
                    checks.borrow_mut().push(serde_json::json!({"current_on_top":w.get_queue_rows().row_data(0).is_some_and(|r|r.active)}));
                    w.invoke_playlist_play(0);stage.set(4);
                },
                4 if w.get_selected_title()=="Video A" && w.get_position()>0.=>{
                    checks.borrow_mut().push(serde_json::json!({"manual_replaces_current_only":w.get_queue_rows().iter().map(|r|r.title.to_string()).collect::<Vec<_>>()==["Video A","Video B"]}));stage.set(5);
                },
                5 if w.get_selected_title()=="Video B" && w.get_position()>0.=>{
                    checks.borrow_mut().push(serde_json::json!({"eos_consumed_previous":w.get_queue_rows().row_count()==1 && w.get_queue_rows().row_data(0).is_some_and(|r|r.active)}));stage.set(6);
                },
                6 if w.get_queue_rows().row_count()==0 && !w.get_playback_active()=>{w.invoke_mode_change(true);stage.set(7);},
                7 if w.get_music_layout() && w.get_playlist_names().iter().any(|s|s=="Music only")=>{
                    checks.borrow_mut().push(serde_json::json!({"music_has_own_library":w.get_playlists().row_count()==1 && !w.get_has_selection()}));
                    w.invoke_playlist_create("New music playlist".into());stage.set(8);
                },
                8 if w.get_collection_title()=="New music playlist"=>{w.invoke_mode_change(false);stage.set(9);},
                9 if !w.get_music_layout() && w.get_playlists().row_count()==2 && w.get_video_playlist_id()=="video-one"=>{w.invoke_playlist_create("New video playlist".into());stage.set(10);},
                10 if w.get_video_playlist_name()=="New video playlist"=>{w.invoke_playlist_add(-1);stage.set(11);},
                11 if w.get_video_playlist_rows().row_count()==1=>{w.invoke_playlist_remove(0);stage.set(12);},
                12 if w.get_video_playlist_rows().row_count()==0=>{w.invoke_playlist_delete();stage.set(13);},
                13 if w.get_playlists().row_count()==2 && w.get_save_to()==-1=>{
                    checks.borrow_mut().push(serde_json::json!({"video_library_lifecycle":w.get_video_playlist_id().is_empty() && w.get_video_playlist_rows().row_count()==0}));stage.set(14);
                },_=>{}
            }
        });
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_LIBRARY").is_some() {
        let weak = window.as_weak();
        let stage = std::cell::Cell::new(0);
        library_automation.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(250),
            move || {
                if let Some(w) = weak.upgrade() {
                    match stage.get() {
                        0 if w.get_playlists().row_count() > 0 => {
                            w.invoke_playlist_create("Roadtrip".into());
                            stage.set(1);
                        }
                        1 if w.get_collection_title() == "Roadtrip" => {
                            w.invoke_playlist_import();
                            stage.set(2);
                        }
                        2 if w.get_status().starts_with("Imported") => {
                            w.invoke_playlist_select("liked-songs".into());
                            stage.set(3);
                        }
                        3 if w.get_videos().row_count() == 2 => {
                            w.invoke_playlist_rename("Evening".into());
                            stage.set(4);
                        }
                        4 if w.get_collection_title() == "Evening" => {
                            w.invoke_playlist_move(1, -1);
                            stage.set(5);
                        }
                        5 if w
                            .get_videos()
                            .row_data(0)
                            .is_some_and(|v| v.title == "Second song") =>
                        {
                            w.invoke_playlist_export(false);
                            stage.set(6);
                        }
                        6 if w.get_status().starts_with("Exported") => {
                            w.invoke_select(0);
                            stage.set(7);
                        }
                        7 if w.get_position() > 1. => {
                            w.invoke_next(false);
                            stage.set(8);
                        }
                        8 if w.get_selected_title() == "First song" => {
                            w.invoke_previous();
                            stage.set(9);
                        }
                        9 if w.get_selected_title() == "Second song" => {
                            w.invoke_playlist_remove(0);
                            stage.set(10);
                        }
                        10 if w.get_videos().row_count() == 1 => {
                            w.invoke_playlist_add(0);
                            stage.set(11);
                        }
                        11 if w.get_status() == "Already in this playlist" => {
                            w.set_status("Music library workflow passed".into());
                            stage.set(12);
                        }
                        _ => {}
                    }
                }
            },
        );
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_AUTOPLAY").is_some() {
        let weak = window.as_weak();
        let triggered = std::cell::Cell::new(false);
        let reloaded = std::cell::Cell::new(false);
        let reload = std::env::var_os("MOCHUNE_SMOKE_RELOAD").is_some();
        let audio_switch = std::env::var_os("MOCHUNE_SMOKE_AUDIO_SWITCH").is_some();
        let paused_seek = std::env::var_os("MOCHUNE_SMOKE_PAUSED_SEEK").is_some();
        let pause_once = std::cell::Cell::new(false);
        let seek_test = std::env::var_os("MOCHUNE_SMOKE_SEEK_TEST").is_some();
        let audio_seek = std::env::var_os("MOCHUNE_SMOKE_MUSIC").is_some();
        let seek_step = std::cell::Cell::new(0usize);
        let seek_started = std::cell::Cell::new(None::<std::time::Instant>);
        let results = seek_results.clone();
        automation.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(250),
            move || {
                if let Some(w) = weak.upgrade()
                    && !triggered.get()
                    && (w.get_has_selection() || w.get_videos().row_count() > 0)
                    && !w.get_busy()
                {
                    triggered.set(true);
                    if w.get_videos().row_count() > 0 {
                        w.invoke_select(0);
                    } else {
                        w.invoke_play();
                    }
                }
                if seek_test && let Some(w) = weak.upgrade() && !w.get_resolving() && w.get_position() >= 3. {
                    let compare = std::env::var("MOCHUNE_SMOKE_SEEK_COMPARE").ok();
                    let midpoint = (w.get_duration() * 0.5).floor();
                    let targets = if compare.is_some() {[midpoint, midpoint+5., midpoint]} else {[20., 2., 12.]};
                    let step = seek_step.get();
                    if step < targets.len() {
                        if paused_seek && step==0 && !pause_once.get() {
                            pause_once.set(true); w.invoke_toggle_pause(); return;
                        }
                        if let Some(started) = seek_started.get() {
                            if started.elapsed().as_secs_f64() >= 2. && (compare.is_none() || (w.get_position()-targets[step]).abs()<0.5) && (audio_seek || (crate::video::video_position() - w.get_position() as f64).abs() < 0.5) {
                                results.borrow_mut().push(serde_json::json!({"target":targets[step],"position":w.get_position(),"video_position":if audio_seek {None}else{Some(crate::video::video_position())},"audio_only":audio_seek,"paused":w.get_paused()}));
                                if paused_seek && step==0 {w.invoke_toggle_pause();}
                                seek_step.set(step+1); seek_started.set(None);
                            }
                        } else {
                            if compare.is_some() && step==0 && !w.get_paused() {w.invoke_toggle_pause();return;}
                            if compare.as_deref()==Some("keyboard") && step>0 {
                                let key=if step==1 {slint::platform::Key::RightArrow}else{slint::platform::Key::LeftArrow};
                                w.window().dispatch_event(slint::platform::WindowEvent::KeyPressed{text:key.into()});
                                w.window().dispatch_event(slint::platform::WindowEvent::KeyReleased{text:key.into()});
                            } else {smoke_seek(&w, targets[step]);}
                            seek_started.set(Some(std::time::Instant::now()));
                        }
                    }
                }
                if (reload || audio_switch)
                    && let Some(w) = weak.upgrade()
                    && triggered.get()
                    && !reloaded.get()
                    && !w.get_resolving()
                    && w.get_position() >= 5.
                {
                    reloaded.set(true);
                    let choices=w.get_quality_labels();
                    let desired=if audio_switch {"Audio only"} else {"480p"};
                    if let Some(index)=choices.iter().position(|label|label.starts_with(desired)) {
                        w.set_quality_choice(index as i32); w.invoke_choose_quality(index as i32);
                    } else if !audio_switch {w.set_quality_index(1);w.invoke_play();}
                }
            },
        );
    }
    if smoke && std::env::var_os("MOCHUNE_SMOKE_INTERPOLATION").is_some(){window.set_interpolation_mode(if std::env::var("MOCHUNE_SMOKE_INTERPOLATION").as_deref()==Ok("60"){2}else{1});}
    let visual_probe = slint::Timer::default();
    let visual_samples = std::rc::Rc::new(std::cell::RefCell::new(Vec::<u64>::new()));
    if smoke && std::env::var_os("MOCHUNE_SMOKE_VISUAL_TIMING").is_some() {
        let weak = window.as_weak();
        let samples = visual_samples.clone();
        let started = std::time::Instant::now();
        visual_probe.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                if started.elapsed().as_secs() < 5 || samples.borrow().len() >= 30 {
                    return;
                }
                if let Some(w) = weak.upgrade()
                    && w.get_video_visible()
                    && !w.get_resolving()
                    && let Ok(pixels) = w.window().take_snapshot()
                {
                    // Sample moving picture content only, excluding playback controls.
                    let mut hash = 14695981039346656037u64;
                    for y in (100..(pixels.height() / 2)).step_by(7) {
                        for x in (50..(pixels.width() / 2)).step_by(7) {
                            let i = ((y * pixels.width() + x) * 4) as usize;
                            for value in &pixels.as_bytes()[i..i + 3] {
                                hash = (hash ^ u64::from(*value)).wrapping_mul(1099511628211);
                            }
                        }
                    }
                    samples.borrow_mut().push(hash);
                }
            },
        );
    }
    if smoke {
        if std::env::var_os("MOCHUNE_SMOKE_LIGHT").is_some() {
            window.set_light_mode(true);
        }
        if std::env::var_os("MOCHUNE_SMOKE_VIDEO").is_some() {
            window.set_music_layout(false);
            window.set_music_only(false);
        }
        if std::env::var_os("MOCHUNE_SMOKE_MUSIC").is_some() {
            window.invoke_mode_change(true);
        }
        window.set_remember(false);
        if let Ok(index) = std::env::var("MOCHUNE_SMOKE_QUALITY")
            && let Ok(index) = index.parse::<i32>()
        {
            window.set_quality_index(index.clamp(0, 4));
        }
        if std::env::var_os("MOCHUNE_SMOKE_SCROLL_SEARCH").is_some() {
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                if let Some(w) = weak.upgrade() {
                    w.set_results_scroll(-300.);
                    w.invoke_search("full".into());
                }
            });
        }
        if std::env::var_os("MOCHUNE_SMOKE_REPEAT_VIDEO").is_some() {
            window.invoke_toggle_video_repeat();
        }
        if let Ok(category) = std::env::var("MOCHUNE_SMOKE_CLEAR_CATEGORY") {
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                if let Some(w) = weak.upgrade() {
                    w.invoke_clear_cache(category.parse().unwrap_or(3));
                }
            });
        }
        if let Ok(name) = std::env::var("MOCHUNE_SMOKE_PAGES") {
            app::pages::preview(&window, &name);
        } else if std::env::var_os("MOCHUNE_SMOKE_DOWNLOADS").is_some() {
            window.invoke_downloads();
        } else if std::env::var_os("MOCHUNE_SMOKE_HOME").is_some() {
            window.invoke_home();
        } else if std::env::var_os("MOCHUNE_TEST_MEDIA").is_none()
            && let Ok(query) = std::env::var("MOCHUNE_SMOKE_QUERY")
        {
            window.invoke_search(query.into());
        }
        if std::env::var_os("MOCHUNE_SMOKE_SETTINGS").is_some() {
            window.set_settings_open(true);
            if let Ok(tab) = std::env::var("MOCHUNE_SMOKE_SETTINGS_TAB") {
                window.set_settings_tab(tab.parse::<i32>().unwrap_or(0).clamp(0, 5));
            }

            if std::env::var_os("MOCHUNE_SMOKE_SETTINGS_TRANSFER").is_some() {
                let weak = window.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_secs(1), move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_export_playlist(1);
                        w.invoke_playlist_export(false);
                    }
                });
                let weak = window.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
                    if let Some(w) = weak.upgrade() {
                        w.invoke_playlist_import();
                    }
                });
            }
        }
        if smoke && std::env::var_os("MOCHUNE_SMOKE_CACHE_OFF").is_some() {
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(5), move || {
                if let Some(w) = weak.upgrade() {
                    w.set_cache_enabled(false);
                    w.invoke_save_settings();
                }
            });
        }
        if smoke && std::env::var_os("MOCHUNE_SMOKE_CACHE_CANCEL").is_some() {
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(1), move || {
                if let Some(w) = weak.upgrade() {
                    w.set_cache_enabled(!w.get_cache_enabled());
                    w.invoke_cancel_settings();
                }
            });
        }
        if std::env::var_os("MOCHUNE_SMOKE_FULLSCREEN").is_some() {
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
                if let Some(w) = weak.upgrade() {
                    w.invoke_toggle_fullscreen();
                }
            });
        }
        let delay = std::env::var("MOCHUNE_SMOKE_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(2)
            .clamp(2, 90);
        if std::env::var_os("MOCHUNE_SMOKE_QUALITY_POPUP").is_some() {
            let weak = window.as_weak();
            slint::Timer::single_shot(
                std::time::Duration::from_secs(delay.saturating_sub(1)),
                move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_controls_shown(true);
                        w.invoke_show_quality();
                    }
                },
            );
        }
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(delay), move || {
            if let Some(w) = weak.upgrade()
                && std::env::var_os("MOCHUNE_SMOKE_SHOW_CONTROLS").is_some()
            {
                w.set_controls_shown(true);
            }
            if std::env::var_os("MOCHUNE_SMOKE_NO_SNAPSHOT").is_none()
                && let Some(w) = weak.upgrade()
                && let (Ok(pixels), Some(root)) = (
                    w.window().take_snapshot(),
                    std::env::var_os("RUST_EXTERNAL_ROOT"),
                )
            {
                let path = std::path::PathBuf::from(root).join("data/Mochune/ui-smoke.png");
                let _ = image::save_buffer(
                    path,
                    pixels.as_bytes(),
                    pixels.width(),
                    pixels.height(),
                    image::ColorType::Rgba8,
                );
            }
            if let Some(w) = weak.upgrade()
                && let Some(root) = std::env::var_os("RUST_EXTERNAL_ROOT")
            {
                let mut report = serde_json::json!({"music_skin":crate::music_skin::stats(),"skin_status":w.get_skin_status().as_str(),"skin_error":w.get_skin_error(),"startup":crate::app::startup_stats(),"browse":crate::app::browse_stats(),"thumbnail_size":w.get_videos().row_data(0).map(|v|[v.thumbnail.size().width,v.thumbnail.size().height]),"status":w.get_status().as_str(),"music_layout":w.get_music_layout(),"paused":w.get_paused(),"playlist_count":w.get_playlists().row_count(),"collection":w.get_collection_title().as_str(),"rows":w.get_videos().iter().map(|v|v.title.to_string()).collect::<Vec<_>>(),"playback":w.get_playback_status().as_str(),"diagnostics":w.get_diagnostics().as_str(),"position":w.get_position(),"resolving":w.get_resolving(),"title":w.get_selected_title().as_str(),"music":w.get_music_only(),"quality_index":w.get_quality_index(),"embedded_video_visible":w.get_video_visible(),"gpu_frames":w.get_video_frames(),"graphics":w.get_graphics_info().as_str(),"fullscreen":w.get_fullscreen_mode(),"controls_shown":w.get_controls_shown(),"render_stats":crate::video::render_stats(),"visual_samples":*visual_samples.borrow(),"frame_timing":crate::video::frame_timing_stats(),"seek_results":*seek_results.borrow(),"video_position":crate::video::video_position(),"active_decoder":w.get_active_decoder().as_str(),"acceleration_verified":w.get_acceleration_verified(),"acceleration_status":w.get_acceleration_status().as_str(),"acceleration_details":w.get_acceleration_details().as_str(),"hardware_support":w.get_hardware_support().as_str(),"quality_labels":w.get_quality_labels().iter().map(|s|s.to_string()).collect::<Vec<_>>(),"quality_choice":w.get_quality_choice(),"video_size":[w.get_video_frame().size().width,w.get_video_frame().size().height],"source_frame_rate":true,"cache_enabled":w.get_cache_enabled(),"stream_cache_enabled":w.get_stream_cache_enabled()});
                report["interpolation"]=crate::video::interpolation_stats();
                report["interpolation_status"]=w.get_interpolation_status().to_string().into();
                report["source_fps"]=w.get_video_source_fps().into();
                report["interpolation_badge"]=w.get_interpolation_badge().to_string().into();
                report["anime_mode"] = w.get_anime_mode().into();
                report["anime_status"] = w.get_anime_status().to_string().into();
                report["caption_choice"] = w.get_caption_choice().into();
                report["caption_status"] = w.get_caption_status().to_string().into();
                report["caption_labels"] = w.get_caption_labels().iter().map(|s|s.to_string()).collect::<Vec<_>>().into();
                report["source_mode"] = w.global::<Sources>().get_mode().into();
                report["source_rows"] = w.global::<Sources>().get_rows().row_count().into();
                report["manga_page"] = w.global::<Sources>().get_page().into();
                report["manga_prefetched"] = w.global::<Sources>().get_loaded_pages().into();
                report["manga_reader_rows"] = w.global::<Sources>().get_reader_pages().row_count().into();
                report["source_bookmarks"] = w.global::<Sources>().get_bookmarks().row_count().into();
                report["manga_page_count"] = w.global::<Sources>().get_page_count().into();
                report["manga_image_size"] = serde_json::json!([w.global::<Sources>().get_page_image().size().width,w.global::<Sources>().get_page_image().size().height]);
                report["browse_page"] = w.global::<BrowsePages>().get_page().into();
                report["search_loading"] = w.get_search_loading().into();
                report["can_go_back"] = w.global::<BrowsePages>().get_can_go_back().into();
                report["home_mode"] = w.global::<BrowsePages>().get_home_mode().into();
                report["home_has_more"] = w.global::<BrowsePages>().get_home_has_more().into();
                report["page_rows"] = w.global::<BrowsePages>().get_rows().row_count().into();
                report["page_image_count"] = w.global::<BrowsePages>().get_rows().iter().filter(|r| r.image.size().width > 0).count().into();
                report["page_image_bytes"] = w.global::<BrowsePages>().get_rows().iter().map(|r| { let size = r.image.size(); u64::from(size.width) * u64::from(size.height) * 4 }).sum::<u64>().into();
                report["page_artwork_rows"] = w.global::<BrowsePages>().get_rows().iter().filter(|r| r.image.size().width > 0).count().into();
                report["home_section_counts"] = serde_json::json!(w.global::<BrowsePages>().get_sections().iter().map(|section| section.rows.row_count()).collect::<Vec<_>>());
                report["comments_open"] = w.global::<BrowsePages>().get_comments_open().into();
                report["comment_rows"] = w.global::<BrowsePages>().get_comments().row_count().into();
                report["page_busy"] = w.global::<BrowsePages>().get_busy().into();
                report["page_status"] = w.global::<BrowsePages>().get_status().to_string().into();

                if let Some(report) = report.as_object_mut() {
                    report.insert("timeline".into(), serde_json::json!({
                        "x": w.get_timeline_x(), "y": w.get_timeline_y(), "width": w.get_timeline_width(),
                        "ranges": w.get_buffered_ranges().iter().filter(|r| r.end > r.start)
                            .map(|r| [r.start, r.end]).collect::<Vec<_>>(),
                        "estimated": w.get_buffered_estimated(), "duration": w.get_duration(),
                    }));
                    report.insert(
                        "maximized".into(),
                        serde_json::json!(w.window().is_maximized()),
                    );
                    report.insert(
                        "native_window".into(),
                        serde_json::json!(w.window().with_winit_window(|native| {
                            let size = native.inner_size();
                            serde_json::json!({"maximized": native.is_maximized(), "width": size.width, "height": size.height})
                        })),
                    );
                    report.insert(
                        "launch_maximized".into(),
                        serde_json::json!(w.get_launch_maximized()),
                    );
                    report.insert(
                        "description_links".into(),
                        serde_json::json!(
                            w.get_description_links()
                                .iter()
                                .map(|v| v.to_string())
                                .collect::<Vec<_>>()
                        ),
                    );
                    report.insert(
                        "has_selection".into(),
                        serde_json::json!(w.get_has_selection()),
                    );
                    report.insert(
                        "selected_image_size".into(),
                        serde_json::json!([
                            w.get_selected_image().size().width,
                            w.get_selected_image().size().height
                        ]),
                    );
                    report.insert(
                        "caption_next".into(),
                        serde_json::json!(w.get_caption_next().as_str()),
                    );
                    report.insert(
                        "rec_indices".into(),
                        serde_json::json!(w.get_rec_indices().iter().collect::<Vec<_>>()),
                    );
                }
                if let (serde_json::Value::Object(extra), Some(report)) = (
                    serde_json::json!({"controls_outside":w.get_controls_outside(),"legacy_quality":w.get_legacy_quality(),"controls_over_video":w.get_controls_over_video(),"browse_tab":w.get_browse_tab(),"compact_quality_labels":w.get_quality_compact_labels().iter().map(|v|v.to_string()).collect::<Vec<_>>()}),
                    report.as_object_mut(),
                ) {
                    report.extend(extra);
                }
                if let serde_json::Value::Object(extra) = serde_json::json!({"page_scroll":w.get_page_scroll(),"immersive":w.get_immersive(),"header_window_controls":w.get_immersive() && !w.get_fullscreen_mode(),"left_pane_hovered":w.get_left_pane_hovered(),"left_pane_top":w.get_left_pane_top(),"queue_scroll":w.get_queue_scroll(),"library_scroll":w.get_library_scroll(),"queue_titles":w.get_queue_rows().iter().map(|r|r.title.to_string()).collect::<Vec<_>>(),"queue_active":w.get_queue_rows().iter().map(|r|r.active).collect::<Vec<_>>(),"video_playlist_id":w.get_video_playlist_id().as_str(),"video_playlist_titles":w.get_video_playlist_rows().iter().map(|r|r.title.to_string()).collect::<Vec<_>>(),"playlist_names":w.get_playlist_names().iter().map(|r|r.to_string()).collect::<Vec<_>>(),"save_to":w.get_save_to(),"startup_video_default":startup_video_default,"playback_active":w.get_playback_active(),"related_after_frame":w.get_related_after_frame(),"download_open":w.get_download_open(),"feature_results":*feature_results.borrow(),"history_view":w.get_history_view(),"source_ids":w.get_videos().iter().map(|v|v.source_id.to_string()).collect::<Vec<_>>(),"selected_id":w.get_selected_id().as_str(),"lyrics_enabled":w.get_lyrics_enabled(),"lyrics_available":w.get_lyrics_available(),"description_length":w.get_description().len(),"subscriber_text":w.get_subscriber_text().as_str(),"rating_text":w.get_rating_text().as_str(),"video_stats":w.get_video_stats().as_str(),"caption_labels":w.get_caption_labels().iter().map(|s|s.to_string()).collect::<Vec<_>>(),"caption_status":w.get_caption_status().as_str(),"caption_text":w.get_caption_text().as_str(),"download_status":w.get_download_status().as_str(),"download_progress":w.get_download_progress(),"audio_cache_enabled":w.get_audio_cache_enabled(),"cache_status":w.get_cache_status().as_str(),"offline":w.get_offline(),"connection_status":w.get_connection_status().as_str(),"playing_download":w.get_playing_download(),"repeat_video":w.get_repeat_video(),"results_scroll":w.get_results_scroll()})
                {
                    report.as_object_mut().unwrap().extend(extra);
                }
                report["layout"] = serde_json::json!({
                    "player_rect":[w.get_native_player_x(),w.get_native_player_y(),w.get_native_player_width(),w.get_native_player_height()]
                });
                let _ = std::fs::write(
                    std::path::PathBuf::from(root).join("data/Mochune/ui-smoke.json"),
                    report.to_string(),
                );
            }
            let _ = slint::quit_event_loop();
        });
    }
    // Apply after the native window exists and has been shown. Initial map/size
    // events can undo a maximize request made before show, especially frameless.
    window.show()?;
    #[cfg(target_os = "windows")]
    let _native_frame = window_frame::install_when_ready(&window);
    let launch_maximized = window.get_launch_maximized();
    #[cfg(target_os = "linux")]
    let launch_maximized = app::linux_renderer::restore_window(&window).unwrap_or(launch_maximized);
    if !window.window().is_fullscreen() {
        window.window().set_maximized(launch_maximized);
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
            if let Some(w) = weak.upgrade()
                && w.get_launch_maximized() == launch_maximized
                && !w.window().is_fullscreen()
            {
                // Retry the native request even if Slint's logical flag is already true.
                w.window()
                    .with_winit_window(|native| native.set_maximized(launch_maximized));
                w.window().set_maximized(launch_maximized);
            }
        });
    }
    #[cfg(target_os = "linux")]
    app::linux_renderer::shown(&window, smoke);
    slint::run_event_loop()?;
    window.hide()?;
    #[cfg(target_os = "linux")]
    {
        drop(_runtime);
        app::linux_renderer::exec_requested().map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    }
    Ok(())
}

fn smoke_seek(w: &SetupWindow, target: f32) {
    if std::env::var_os("MOCHUNE_SMOKE_DRAG_SEEK").is_none() {
        w.invoke_seek(target);
        return;
    }
    use slint::platform::{PointerEventButton, WindowEvent};
    w.invoke_player_activity();
    // Follow the actual track, including resized/maximized and outside controls.
    let width = w.get_timeline_width();
    let x = w.get_timeline_x();
    let y = w.get_timeline_y() + 1.;
    let start = slint::LogicalPosition::new(x + width * 0.1, y);
    let end = slint::LogicalPosition::new(x + width * target / w.get_duration(), y);
    w.window().dispatch_event(WindowEvent::PointerPressed {
        position: start,
        button: PointerEventButton::Left,
    });
    w.window()
        .dispatch_event(WindowEvent::PointerMoved { position: end });
    let weak = w.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(350), move || {
        if let Some(w) = weak.upgrade() {
            w.window().dispatch_event(WindowEvent::PointerReleased {
                position: end,
                button: PointerEventButton::Left,
            });
        }
    });
}

pub fn doctor() -> anyhow::Result<serde_json::Value> {
    gst::init()?;
    #[cfg(target_os = "windows")]
    let (elements, presentation): (&[&str], &str) = (
        &[
            "uridecodebin",
            "d3d12videosink",
            "capsfilter",
            "autoaudiosink",
            "souphttpsrc",
        ],
        "Native D3D12 swapchain / software UI (GStreamer >= 1.26)",
    );
    #[cfg(not(target_os = "windows"))]
    let (elements, presentation): (&[&str], &str) = (
        &[
            "uridecodebin",
            "glsinkbin",
            "appsink",
            "autoaudiosink",
            "souphttpsrc",
        ],
        "shared OpenGL textures",
    );
    let missing: Vec<_> = elements
        .iter()
        .filter(|name| gst::ElementFactory::find(name).is_none())
        .copied()
        .collect();
    Ok(
        serde_json::json!({"backend":"GStreamer","version":gst::version_string().to_string(),"embedded":true,"presentation":presentation,"missing_plugins":missing}),
    )
}
