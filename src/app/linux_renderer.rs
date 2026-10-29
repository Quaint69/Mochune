//! Linux uses software Music UI and OpenGL Video UI. Only metadata crosses exec.
use super::*;
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
use std::sync::{OnceLock, atomic::AtomicBool};

const OPENGL_OVERRIDE: &str = "--linux-opengl-ui";
static STARTUP_MUSIC: OnceLock<bool> = OnceLock::new();
const HANDOFF: &str = "--linux-renderer-handoff";
const LIMIT: usize = 1024 * 1024;
static REQUEST: Mutex<Option<PathBuf>> = Mutex::new(None);
static RESTORING: AtomicBool = AtomicBool::new(false);
static RESTORED: OnceLock<(Geometry, u64, u32)> = OnceLock::new();
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Geometry {
    width: f32,
    height: f32,
    maximized: bool,
}
#[derive(Serialize, Deserialize)]
struct Handoff {
    version: u8,
    music: bool,
    #[serde(default)]
    source_mode: i32,
    sessions: [Option<session::Session>; 2],
    geometry: Geometry,
    started_ms: u64,
    switches: u32,
    repeat_list: bool,
    repeat_song: bool,
    repeat_video: bool,
    shuffle: bool,
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub(crate) fn enabled() -> bool {
    !std::env::args().any(|a| a == OPENGL_OVERRIDE)
}
pub(super) fn switch_pending() -> bool {
    REQUEST.lock().unwrap().is_some()
}
pub(crate) fn restoring() -> bool {
    RESTORING.load(Ordering::Relaxed)
}
fn data_dir() -> PathBuf {
    crate::identity::data_directory()
}
pub(crate) fn startup_music() -> bool {
    *STARTUP_MUSIC.get_or_init(|| {
        if std::env::args().any(|a| a == "--start-video") {
            return false;
        }
        if std::env::args().any(|a| a == "--start-music") {
            return true;
        }
        let smoke = std::env::args().any(|a| a == "--smoke-ui");
        if smoke && std::env::var_os("MOCHUNE_SMOKE_VIDEO").is_some() {
            return false;
        }
        if smoke && std::env::var_os("MOCHUNE_SMOKE_MUSIC").is_some() {
            return true;
        }
        let folder = data_dir();
        read_json::<Settings>(folder.join("settings.json")).last_music_mode
    })
}
pub(crate) fn software_music() -> bool {
    enabled() && startup_music()
}

fn capture(w: &SetupWindow, st: &State) -> session::Session {
    session::Session {
        collection: session::Collection {
            videos: st.videos.clone(),
            title: w.get_collection_title().to_string(),
            playlist: w.get_playlist_id().to_string(),
            history: w.get_history_view(),
        },
        selected: st.selected.clone(),
        queue: st.queue.clone(),
        queue_index: st.queue_index,
        queue_is_playlist: st.queue_is_playlist,
        queue_has_current: st.queue_has_current,
        music_playlist_pending: st.music_playlist_pending,
        music_shuffle_visited: st.music_shuffle_visited.clone(),
        position: w.get_position() as f64,
        duration: w.get_duration(),
        query: w.get_query().to_string(),
        quality: w.get_quality_index(),
        save_to: w.get_save_to(),
        browse_tab: w.get_browse_tab(),
    }
}
pub(super) fn prepare(w: &SetupWindow, state: &Arc<Mutex<State>>, paths: &Paths) -> Result<()> {
    let st = state.lock().unwrap();
    let mut sessions = st.sessions.clone();
    let music = w.get_music_layout();
    sessions[usize::from(music)] = Some(capture(w, &st));
    let size = w.window().size().to_logical(w.window().scale_factor());
    let data = Handoff {
        version: 1,
        music,
        source_mode: if music {0}else{w.global::<crate::Sources>().get_pending_mode().clamp(0,3)},
        sessions,
        geometry: Geometry {
            width: size.width,
            height: size.height,
            maximized: w.window().is_maximized(),
        },
        repeat_list: w.get_repeat(),
        repeat_song: w.get_repeat_one(),
        repeat_video: w.get_repeat_video(),
        shuffle: w.get_shuffle(),
        started_ms: now_ms(),
        switches: RESTORED.get().map_or(1, |v| v.2 + 1),
    };
    let bytes = serde_json::to_vec(&data)?;
    anyhow::ensure!(
        bytes.len() <= LIMIT,
        "Session exceeds the 1 MiB handoff limit"
    );
    let path = paths.temp.join(format!(
        "linux-renderer-handoff-{}.json",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    if let Err(error) = file.write_all(&bytes) {
        let _ = fs::remove_file(&path);
        return Err(error.into());
    }
    *REQUEST.lock().unwrap() = Some(path);
    crate::diagnostics::record(
        "renderer_handoff_requested",
        serde_json::json!({"software_music":music,"metadata_bytes":bytes.len()}),
    );
    Ok(())
}
pub(super) fn restore(w: &SetupWindow, state: &Arc<Mutex<State>>, paths: &Paths) -> bool {
    if !enabled() {
        return false;
    }
    let args: Vec<_> = std::env::args().collect();
    let Some(path) = args
        .iter()
        .position(|a| a == HANDOFF)
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
    else {
        return false;
    };
    let result = (|| -> Result<Handoff> {
        anyhow::ensure!(
            path.parent() == Some(paths.temp.as_path())
                && path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("linux-renderer-handoff-")),
            "Invalid renderer handoff location"
        );
        let mut bytes = Vec::new();
        fs::File::open(&path)?
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= LIMIT, "Renderer handoff too large");
        let data: Handoff = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            data.version == 1 && data.music == software_music(),
            "Renderer handoff mode/version mismatch"
        );
        fs::remove_file(&path)?;
        Ok(data)
    })();
    match result {
        Ok(data) => {
            w.set_repeat(data.repeat_list);
            w.set_repeat_one(data.repeat_song);
            w.set_repeat_video(data.repeat_video);
            w.set_shuffle(data.shuffle);
            state.lock().unwrap().sessions = data.sessions;
            let _ = RESTORED.set((data.geometry, data.started_ms, data.switches));
            RESTORING.store(true, Ordering::Relaxed);
            // Reuse the normal per-mode restoration, without replacing the inactive snapshot.
            w.set_music_layout(!data.music);
            w.invoke_mode_change(data.music);
            RESTORING.store(false, Ordering::Relaxed);
            if !data.music && data.source_mode>0 {w.global::<crate::Sources>().invoke_select_mode(data.source_mode.clamp(0,3));}
            true
        }
        Err(error) => {
            w.invoke_notify(format!("Renderer session could not be restored: {error}").into());
            false
        }
    }
}
pub(crate) fn restore_window(w: &SetupWindow) -> Option<bool> {
    let (g, _, _) = RESTORED.get()?;
    if !g.maximized && g.width.is_finite() && g.height.is_finite() {
        w.window().set_size(slint::LogicalSize::new(
            g.width.clamp(320., 7680.),
            g.height.clamp(400., 4320.),
        ));
    }
    Some(g.maximized)
}
pub(crate) fn shown(w: &SetupWindow, smoke: bool) {
    if !enabled() {
        return;
    }
    let software = software_music();
    w.set_graphics_info(
        if software {
            "Software UI · audio only"
        } else {
            "OpenGL UI · video capable"
        }
        .into(),
    );
    slint::Timer::single_shot(Duration::ZERO, move || {
        let elapsed = RESTORED.get().map(|v| now_ms().saturating_sub(v.1));
        crate::diagnostics::record(
            "renderer_ui_ready",
            serde_json::json!({"software_music":software,"handoff_ms":elapsed,"measurement":"event loop after window show; not compositor presentation"}),
        );
        eprintln!(
            "Linux renderer: {}; relaunch to UI-ready: {:?} ms",
            if software { "software" } else { "OpenGL" },
            elapsed
        );
    });
    if smoke
        && std::env::var_os("MOCHUNE_SMOKE_RENDERER_SWITCH").is_some()
        && RESTORED.get().map_or(0, |v| v.2) < 2
    {
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_secs(1), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_mode_change(!w.get_music_layout());
            }
        });
    }
}
pub(crate) fn exec_requested() -> std::io::Result<()> {
    let Some(path) = REQUEST.lock().unwrap().take() else {
        return Ok(());
    };
    let mut args = std::env::args_os().skip(1);
    let mut kept = Vec::new();
    while let Some(arg) = args.next() {
        if arg == HANDOFF {
            let _ = args.next();
        } else if arg != "--start-music" && arg != "--start-video" {
            kept.push(arg);
        }
    }
    let data: Handoff = serde_json::from_slice(&fs::read(&path)?)?;
    let error = Command::new(std::env::current_exe()?)
        .args(kept)
        .arg(if data.music {
            "--start-music"
        } else {
            "--start-video"
        })
        .arg(HANDOFF)
        .arg(&path)
        .exec();
    let _ = fs::remove_file(path);
    Err(error)
}

pub(super) fn rollback(w: &SetupWindow) {
    RESTORING.store(true, Ordering::Relaxed);
    w.invoke_mode_change(software_music());
    RESTORING.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handoff_retains_both_queues_positions_and_repeat_state() {
        let video = Video {
            id: "video-id".into(),
            title: "Title".into(),
            ..Default::default()
        };
        let session = session::Session {
            selected: Some(video.clone()),
            queue: vec![video],
            position: 123.,
            queue_has_current: true,
            query: "a search".into(),
            quality: 3,
            ..Default::default()
        };
        let data = Handoff {
            version: 1,
            music: true,
            source_mode: 0,
            sessions: [Some(session.clone()), Some(session)],
            geometry: Geometry {
                width: 1180.,
                height: 780.,
                maximized: false,
            },
            started_ms: now_ms(),
            switches: 1,
            repeat_list: false,
            repeat_song: true,
            repeat_video: true,
            shuffle: true,
        };
        let encoded = serde_json::to_vec(&data).unwrap();
        assert!(encoded.len() < LIMIT);
        let restored: Handoff = serde_json::from_slice(&encoded).unwrap();
        for mode in restored.sessions {
            let mode = mode.unwrap();
            assert_eq!(mode.position, 123.);
            assert_eq!(mode.selected.unwrap().id, "video-id");
            assert_eq!(mode.queue.len(), 1);
            assert_eq!(mode.query, "a search");
            assert!(mode.queue_has_current);
        }
        assert!(restored.repeat_song && restored.repeat_video && restored.shuffle);
        assert!(!restored.repeat_list);
    }
}
