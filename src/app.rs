mod about;
mod audio_collection;
mod sources;
pub(crate) mod audio_cache;
mod availability;
mod browse_view;
mod cache_policy;
mod captions;
mod connectivity;
mod description;
mod details;
mod download;
mod helper_cache;
mod helper_error;
mod history;
mod home;
mod metadata;
mod music;
mod music_retry;
mod music_taste;
pub(crate) mod pages;
mod play_queue;
mod published;
mod recommendations;
mod search;
mod session;
#[cfg(target_os = "linux")]
pub(crate) mod linux_renderer;
mod settings_terms;
mod stream_cache;
use crate::{SetupWindow, VideoCard};
use anyhow::{Context, Result, bail};
use gst::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    quality: i32,
    music: bool,
    last_music_mode: bool,
    volume: f32,
    download_folder: String,
    light_mode: bool,
    immersive: bool,
    launch_maximized: bool,
    music_extra_card: bool,
    music_skin: i32,
    music_skin_path: String,
    skin_animations: bool,
    theme_header: bool,
    theme_library: bool,
    theme_list: bool,
    theme_player: bool,
    theme_video: bool,
    controls_outside: bool,
    legacy_quality: bool,
    interpolation_enabled:bool,
    interpolation_mode:i32,
    interpolation_auto_fallback:bool,
    audio_effects: crate::audio_effects::Config,
    interests: String,
    recommendation_mode: i32,
    recommendation_count: i32,
    minimum_recommendation_views: u64,
    auto_related: bool,
    use_history: bool,
    hide_watched: bool,
    hide_alternate_versions: bool,
    blocked_channels: String,
    blocked_keywords: String,

    hardware: bool,
    cache_enabled: bool,
    stream_cache_enabled: bool,
    audio_cache_enabled: bool,
    audio_cache_limit_mib: u64,
    remember: bool,
    #[serde(skip)]
    format_override: Option<String>,
    // Bounded, request-only refresh hints; never written to user preferences.
    #[serde(skip)]
    recommendation_avoid: Vec<String>,
    #[serde(skip)]
    recommendation_defer: Vec<String>,
    #[serde(skip)]
    playback_request: bool,
    #[serde(skip)]
    seed_recommendations: bool,
    #[serde(skip)]
    public_fallback: bool,
    #[serde(skip)]
    comment_request: Option<bool>,
    #[serde(skip)]
    resume_position: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            quality: 2,
            music: false,
            last_music_mode: true,
            volume: 70.,
            download_folder: download::default_folder().to_string_lossy().into_owned(),
            light_mode: false,
            immersive: true,
            launch_maximized: true,
            music_extra_card: true,
            music_skin: 0,
            music_skin_path: String::new(),
            skin_animations: true,
            theme_header: true,
            theme_library: true,
            theme_list: true,
            theme_player: true,
            theme_video: true,
            controls_outside: false,
            legacy_quality: false,
            interpolation_enabled:false,
            interpolation_mode:0,
            interpolation_auto_fallback:true,
            audio_effects: Default::default(),
            interests: "technology, music, science".into(),
            recommendation_mode: 0,
            recommendation_count: 30,
            minimum_recommendation_views: 10_000,
            auto_related: true,
            use_history: true,
            hide_watched: false,
            hide_alternate_versions: true,
            blocked_channels: String::new(),
            blocked_keywords: String::new(),

            hardware: true,

            cache_enabled: false,
            stream_cache_enabled: true,
            audio_cache_enabled: false,
            audio_cache_limit_mib: 256,
            remember: true,
            format_override: None,
            recommendation_avoid: Vec::new(),
            recommendation_defer: Vec::new(),
            playback_request: false,
            seed_recommendations: false,
            public_fallback: false,
            comment_request: None,
            resume_position: 0.,
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Video {
    id: String,
    title: String,
    channel: String,
    duration: f64,
    #[serde(default)]
    views: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    published: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    music: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    genres: Vec<String>,
    #[serde(skip)]
    unavailable: String,
    #[serde(skip)]
    audio_unavailable: bool,
}
impl Video {
    fn playable(&self, music: bool) -> bool {
        !self.id.is_empty() && self.unavailable.is_empty() && (!music || !self.audio_unavailable)
    }
    fn url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }
    fn card(&self) -> VideoCard {
        VideoCard {
            title: self.title.clone().into(),
            channel: self.channel.clone().into(),
            duration_text: if self.duration > 0. {
                clock(self.duration).into()
            } else {
                "—".into()
            },
            views_text: self.views.map(view_count).unwrap_or_default().into(),
            detail: [
                self.views.map(view_count),
                (self.duration > 0.).then(|| clock(self.duration)),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
            .into(),
            thumbnail: slint::Image::default(),
            source_id: self.id.clone().into(),
            downloaded: false,
            available: self.playable(false),
            music_available: self.playable(true),
            unavailable_reason: if !self.unavailable.is_empty() {
                self.unavailable.clone()
            } else if self.audio_unavailable {
                "Audio-only stream unavailable".into()
            } else if self.id.is_empty() {
                "Unsupported source".into()
            } else {
                String::new()
            }
            .into(),
        }
    }
}
#[derive(Default)]
struct State {
    anime_playback: Option<Arc<sources::Playback>>,
    settings: Settings,
    history: [Vec<Video>; 2],
    history_error: Option<String>,
    recent_played: [Vec<String>; 2],
    likes_seed_cursor: usize,
    removed_history: [std::collections::HashSet<String>; 2],
    unavailable: std::collections::BTreeMap<String, (String, bool)>,
    captions: Option<Arc<captions::Controller>>,
    videos: Vec<Video>,
    selected: Option<Video>,
    format_options: Vec<FormatOption>,
    formats_for: Option<String>,
    queue: Vec<Video>,
    queue_index: usize,
    queue_is_playlist: bool,
    music_discover_pending: bool,
    music_follow_discover: bool,
    music_search_anchor: Option<String>,
    music_artist: Option<(String,String,i32)>,
    music_playlist_pending: bool,
    music_shuffle_visited: Vec<String>,
    queue_has_current: bool,
    cover_tx: Option<mpsc::Sender<(Video, bool)>>,
    collection_art_tx: Option<mpsc::Sender<(u64, Vec<Video>)>>,
    active_music: bool,
    related_for: Option<String>,
    play_browse_epoch: u64,
    sessions: [Option<session::Session>; 2],
    resume_position: Option<f64>,
}
#[derive(Clone)]
struct Paths {
    data: PathBuf,
    cache: PathBuf,
    temp: PathBuf,
    cache_policy: Arc<cache_policy::Policy>,
    audio: Arc<audio_cache::Store>,
    network: Arc<connectivity::Connection>,
}
impl Paths {
    fn new() -> Self {
        let base = crate::identity::data_directory();
        let paths = Self {
            audio: audio_cache::Store::open(base.join("audio-cache")),
            network: connectivity::Connection::new(false),
            cache: base.join("thumbnails"),
            temp: base.join("tmp"),
            data: base,
            cache_policy: Arc::new(cache_policy::Policy::default()),
        };
        for dir in [&paths.data, &paths.cache, &paths.temp] {
            let _ = fs::create_dir_all(dir);
        }
        if !cfg!(windows) {
            crate::diagnostics::initialize(&paths.data);
        }
        paths
    }
    fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        crate::playlists::write_atomic(
            &self.data.join(name),
            &serde_json::to_vec_pretty(value)?,
            false,
        )
    }
}
fn read_json<T: for<'a> Deserialize<'a> + Default>(path: PathBuf) -> T {
    fs::read(path)
        .ok()
        .and_then(|s| serde_json::from_slice(&s).ok())
        .unwrap_or_default()
}
fn sanitize_volume(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0., 100.)
    } else {
        70.
    }
}
fn settings(window: &SetupWindow) -> Settings {
    Settings {
        quality: window.get_quality_index(),
        music: window.get_music_only(),
        last_music_mode: window.get_music_layout(),
        volume: sanitize_volume(window.get_volume()),
        download_folder: window.get_download_folder().to_string(),
        light_mode: window.get_light_mode(),
        immersive: window.get_immersive(),
        launch_maximized: window.get_launch_maximized(),
        music_extra_card: window.get_music_extra_card(),
        music_skin: window.get_music_skin_choice().clamp(0, 3),
        music_skin_path: window.get_music_skin_path().to_string(),
        skin_animations: window.get_skin_animations(),
        theme_header: window.get_theme_header(),
        theme_library: window.get_theme_library(),
        theme_list: window.get_theme_list(),
        theme_player: window.get_theme_player(),
        theme_video: window.get_theme_video(),
        controls_outside: window.get_controls_outside(),
        legacy_quality: window.get_legacy_quality(),
        interpolation_enabled:window.get_interpolation_enabled(),
        interpolation_mode:window.get_interpolation_mode().clamp(0,2),
        interpolation_auto_fallback:window.get_interpolation_auto_fallback(),
        audio_effects: crate::audio_effects::Config::read(window),
        interests: String::new(),
        recommendation_mode: 0,
        recommendation_count: recommendations::video_limit(window.get_recommendation_count()),
        minimum_recommendation_views: [10_000, 20_000, 30_000, 40_000, 50_000]
            [window.get_minimum_views().clamp(0, 4) as usize],
        auto_related: window.get_auto_related(),
        use_history: window.get_use_history(),
        hide_watched: window.get_hide_watched(),
        hide_alternate_versions: window.get_hide_alternate_versions(),
        blocked_channels: window.get_blocked_channels().chars().take(4096).collect(),
        blocked_keywords: window.get_blocked_keywords().chars().take(4096).collect(),
        cache_enabled: window.get_cache_enabled(),
        stream_cache_enabled: window.get_stream_cache_enabled(),
        audio_cache_enabled: window.get_audio_cache_enabled(),
        audio_cache_limit_mib: [128, 256, 512, 1024]
            [window.get_audio_cache_limit().clamp(0, 3) as usize],

        hardware: true,

        remember: window.get_remember(),
        format_override: None,
        recommendation_avoid: Vec::new(),
        recommendation_defer: Vec::new(),
        playback_request: false,
        seed_recommendations: false,
        public_fallback: false,
        comment_request: None,
        resume_position: 0.,
    }
}
fn apply_settings(w: &SetupWindow, s: &Settings) {
    s.audio_effects.show(w);
    w.set_quality_index(s.quality.clamp(0, 4));
    w.set_music_only(s.music);
    w.set_volume(sanitize_volume(s.volume));
    w.set_download_folder(s.download_folder.clone().into());
    w.set_light_mode(s.light_mode);
    w.set_immersive(s.immersive);
    w.set_launch_maximized(s.launch_maximized);
    w.set_interpolation_supported(cfg!(any(target_os="linux",target_os="windows")));
    w.set_interpolation_mode(if w.get_interpolation_supported() {s.interpolation_mode.clamp(0,2).max(if s.interpolation_enabled{1}else{0})} else {0});
            w.set_interpolation_auto_fallback(s.interpolation_auto_fallback);
    w.set_music_extra_card(s.music_extra_card);
    w.set_music_skin_path(s.music_skin_path.clone().into());
    w.set_music_skin_choice(s.music_skin.max(0));
    w.set_skin_animations(s.skin_animations);
    w.set_theme_header(s.theme_header);
    w.set_theme_library(s.theme_library);
    w.set_theme_list(s.theme_list);
    w.set_theme_player(s.theme_player);
    w.set_theme_video(s.theme_video);
    w.set_controls_outside(s.controls_outside);
    w.set_legacy_quality(s.legacy_quality);
    w.set_interests(s.interests.clone().into());
    w.set_recommendation_mode(s.recommendation_mode.clamp(0, 2));
    w.set_recommendation_count(recommendations::video_limit_choice(s.recommendation_count));
    w.set_minimum_views(minimum_views_choice(s.minimum_recommendation_views));
    w.set_auto_related(s.auto_related);
    w.set_use_history(s.use_history);
    w.set_hide_watched(s.hide_watched);
    w.set_hide_alternate_versions(s.hide_alternate_versions);
    w.set_blocked_channels(s.blocked_channels.clone().into());
    w.set_blocked_keywords(s.blocked_keywords.clone().into());

    w.set_hardware(cfg!(target_os = "windows") || s.hardware);
    w.set_remember(s.remember);
    w.set_cache_enabled(s.cache_enabled);
    w.set_stream_cache_enabled(s.stream_cache_enabled);
    w.set_audio_cache_enabled(s.audio_cache_enabled);
    w.set_audio_cache_limit(limit_choice(s.audio_cache_limit_mib));
}
fn limit_choice(mib: u64) -> i32 {
    [128, 256, 512, 1024]
        .iter()
        .position(|n| *n >= mib)
        .unwrap_or(3) as i32
}
fn cached_card(paths: &Paths, video: &Video) -> VideoCard {
    let mut card = video.card();
    card.downloaded = paths.audio.contains(&video.id);
    if card.downloaded {
        card.music_available = true;
    }
    card
}
fn ui(weak: &slint::Weak<SetupWindow>, f: impl FnOnce(SetupWindow) + Send + 'static) {
    let _ = weak.upgrade_in_event_loop(f);
}
fn ui_generation(
    weak: &slint::Weak<SetupWindow>,
    generation: &Arc<AtomicU64>,
    n: u64,
    f: impl FnOnce(SetupWindow) + Send + 'static,
) {
    let generation = generation.clone();
    ui(weak, move |w| {
        if generation.load(Ordering::Relaxed) == n {
            f(w);
        }
    });
}
fn clock(seconds: f64) -> String {
    let n = seconds.max(0.) as u64;
    if n >= 3600 {
        format!("{}:{:02}:{:02}", n / 3600, (n / 60) % 60, n % 60)
    } else {
        format!("{}:{:02}", n / 60, n % 60)
    }
}
fn helper(name: &str) -> PathBuf {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    if let Some(root) = std::env::var_os("RUST_EXTERNAL_ROOT") {
        let path = PathBuf::from(root).join("tools/bin").join(&file);
        if path.exists() {
            return path;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        // The Windows runtime carries yt-dlp's official onedir distribution.
        // Keep its Python DLLs isolated from GStreamer's DLL search directory.
        if name == "yt-dlp" {
            let unpacked = exe.with_file_name("yt-dlp").join(&file);
            if unpacked.is_file() {
                return unpacked;
            }
        }
        let path = exe.with_file_name(&file);
        if path.exists() {
            return path;
        }
        if let Some(prefix) = exe.parent().and_then(Path::parent) {
            let installed = prefix.join("lib/mochune").join(&file);
            if installed.exists() {
                return installed;
            }
        }
    }
    PathBuf::from(file)
}
static NEXT_PROCESS: AtomicU64 = AtomicU64::new(0);
fn private_file(path: &Path) -> Result<fs::File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    Ok(opts.open(path)?)
}
struct TempOutput(PathBuf, PathBuf);
impl Drop for TempOutput {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let _ = fs::remove_file(&self.1);
    }
}
fn extract(
    paths: &Paths,
    s: &Settings,
    target: &str,
    flat: bool,
    generation: Option<(&AtomicU64, u64)>,
) -> Result<Value> {
    extract_using_info(paths, s, target, flat, generation, None)
}
// Resolution helpers can be substantially larger than an image worker. Keep
// browsing and playback from starting two yt-dlp process trees concurrently.
struct HelperPermit;
static PLAYBACK_PENDING: AtomicU64 = AtomicU64::new(0);
fn helper_can_start(active: bool, pending: u64, playback: bool) -> bool {
    !active && (playback || pending == 0)
}
fn finish_playback_priority(n: u64) {
    let _ = PLAYBACK_PENDING.compare_exchange(n, 0, Ordering::AcqRel, Ordering::Relaxed);
    HELPER_READY.notify_all();
}
fn wait_for_playback(generation: Option<(&AtomicU64, u64)>) -> bool {
    while PLAYBACK_PENDING.load(Ordering::Acquire) != 0 {
        if generation.is_some_and(|(g, n)| g.load(Ordering::Relaxed) != n) {
            return false;
        }
        thread::sleep(Duration::from_millis(40));
    }
    !generation.is_some_and(|(g, n)| g.load(Ordering::Relaxed) != n)
}
static HELPER_ACTIVE: Mutex<bool> = Mutex::new(false);
static HELPER_READY: std::sync::Condvar = std::sync::Condvar::new();
impl HelperPermit {
    #[cfg(test)]
    fn acquire(generation: Option<(&AtomicU64, u64)>) -> Result<Self> {
        Self::acquire_priority(generation, false)
    }
    fn acquire_priority(generation: Option<(&AtomicU64, u64)>, playback: bool) -> Result<Self> {
        let started = Instant::now();
        let mut active = HELPER_ACTIVE.lock().unwrap();
        loop {
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(65)
                    && !generation.is_some_and(|(g, n)| g.load(Ordering::Relaxed) != n),
                "Request cancelled or timed out. Please try again."
            );
            if helper_can_start(*active, PLAYBACK_PENDING.load(Ordering::Acquire), playback) {
                *active = true;
                return Ok(Self);
            }
            active = HELPER_READY
                .wait_timeout(active, Duration::from_millis(80))
                .unwrap()
                .0;
        }
    }
}
impl Drop for HelperPermit {
    fn drop(&mut self) {
        *HELPER_ACTIVE.lock().unwrap() = false;
        HELPER_READY.notify_all();
    }
}
fn extract_using_info(
    paths: &Paths,
    s: &Settings,
    target: &str,
    flat: bool,
    generation: Option<(&AtomicU64, u64)>,
    cached: Option<&[u8]>,
) -> Result<Value> {
    let queued_at = Instant::now();
    let _helper =
        HelperPermit::acquire_priority(generation, s.playback_request).inspect_err(|_| {
            crate::diagnostics::record(
                "resolver_queue_aborted",
                serde_json::json!({
                    "epoch":generation.map(|(_,n)|n),"playback":s.playback_request,
                    "queue_ms":queued_at.elapsed().as_secs_f64()*1000.
                }),
            );
        })?;
    let queue_ms = queued_at.elapsed().as_secs_f64() * 1000.;
    let id = NEXT_PROCESS.fetch_add(1, Ordering::Relaxed);
    let mut timing = HelperTiming::new(id, queue_ms, s.playback_request, cached.is_some());
    timing.epoch = generation.map(|(_, n)| n);
    let files = TempOutput(
        paths.temp.join(format!("{}-{id}.json", std::process::id())),
        paths.temp.join(format!("{}-{id}.err", std::process::id())),
    );
    // HelperPermit serializes resolver access, including cache maintenance.
    let deno_cache =
        helper_cache::Cache::open(paths.data.join("runtime-cache/deno-v1")).or_else(|_| {
            crate::diagnostics::record("runtime_cache_fallback", serde_json::json!({"request":id}));
            helper_cache::Cache::scratch(
                paths.temp.join(format!("{}-{id}.deno", std::process::id())),
            )
        })?;
    let executable = helper("yt-dlp");
    timing.unpacked = executable
        .parent()
        .is_some_and(|p| p.join("_internal").is_dir());
    timing.deno_reused = deno_cache.reused;
    let mut cmd = Command::new(executable);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW: helpers must not flash consoles.
    }
    cmd.env("DENO_DIR", &deno_cache.path);
    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd.args([
        "--ignore-config",
        "--encoding",
        "utf-8",
        "--no-warnings",
        "--no-progress",
        "--socket-timeout",
        "10",
        "--retries",
        "1",
        "--extractor-retries",
        "1",
        "--dump-single-json",
        "--no-cache-dir",
    ]);
    let deno = helper("deno");
    if deno.is_file() {
        cmd.args(["--js-runtimes", &format!("deno:{}", deno.display())]);
    }
    cmd.args([
        "--format-sort",
        if s.hardware {
            "res,vcodec:h264,proto:https"
        } else {
            "res,proto:https"
        },
    ]);
    if let Some(newest) = s.comment_request {
        cmd.args([
            "--write-comments",
            "--skip-download",
            "--ignore-no-formats-error",
            "--extractor-args",
            if newest {
                "youtube:max_comments=40,40,0,0;comment_sort=new"
            } else {
                "youtube:max_comments=40,40,0,0;comment_sort=top"
            },
        ]);
    }
    if flat {
        cmd.args([
            "--extractor-args",
            "youtubetab:approximate_date",
            "--flat-playlist",
            "--playlist-end",
            if s.music { "40" } else { "30" },
        ]);
        // A single pasted video is fully extracted even with --flat-playlist.
        // Select the requested playback format now, avoiding a second helper run.
        if crate::playlists::youtube_id_from_uri(target).is_some() {
            cmd.args(["--no-playlist", "-f", &format_selector(s)]);
        }
    } else {
        cmd.args(["--no-playlist", "-f", &format_selector(s)]);
    }
    let input = TempOutput(
        paths
            .temp
            .join(format!("{}-{id}.input.json", std::process::id())),
        paths
            .temp
            .join(format!("{}-{id}.unused", std::process::id())),
    );
    if let Some(bytes) = cached {
        private_file(&input.0)?.write_all(bytes)?;
        cmd.arg("--load-info-json").arg(&input.0);
    } else {
        cmd.arg("--").arg(target);
    }
    cmd.stdin(Stdio::null())
        .stdout(private_file(&files.0)?)
        .stderr(private_file(&files.1)?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    timing.phase = "spawn";
    let spawn_at = Instant::now();
    let mut child = cmd
        .spawn()
        .context("Cannot start yt-dlp. Check that the YouTube helper is installed.")?;
    timing.spawn_ms = spawn_at.elapsed().as_secs_f64() * 1000.;
    crate::diagnostics::record(
        "resolver_helper_started",
        serde_json::json!({"pid":child.id(),"parent_pid":std::process::id(),"backend":"short_lived_cli","console_window_disabled":cfg!(windows)}),
    );
    timing.phase = "helper";
    let start = Instant::now();
    let mut memory_sample = Instant::now() - Duration::from_secs(1);
    loop {
        if memory_sample.elapsed() >= Duration::from_millis(200) {
            memory_sample = Instant::now();
            timing.peak_rss = timing.peak_rss.max(
                helper_cache::process_memory(&child)["rss_bytes"]
                    .as_u64()
                    .unwrap_or(0),
            );
        }
        if let Some(status) = child.try_wait()? {
            timing.helper_ms = start.elapsed().as_secs_f64() * 1000.;
            timing.exit_code = status.code();
            if !status.success() {
                timing.phase = "helper_failed";
                let error = fs::File::open(&files.1).and_then(|file| {
                    let mut bytes = Vec::new();
                    file.take(16 * 1024).read_to_end(&mut bytes)?;
                    Ok(bytes)
                });
                let reason = helper_error::message(status.code(), error.as_deref().unwrap_or(&[]));
                crate::diagnostics::record(
                    "resolver_helper_failed",
                    serde_json::json!({"exit_code":status.code(),"stderr_empty":error.as_ref().is_ok_and(|b| b.is_empty()),
                        "stderr_read_error":error.as_ref().err().map(ToString::to_string),"reason":reason,"elapsed_ms":start.elapsed().as_millis()}),
                );
                bail!("{reason}");
            }
            break;
        }
        if !s.playback_request
            && s.comment_request.is_some()
            && PLAYBACK_PENDING.load(Ordering::Acquire) != 0
        {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Playback took priority. Load comments again when ready.");
        }
        if start.elapsed() > Duration::from_secs(65)
            || generation.is_some_and(|(g, n)| g.load(Ordering::Relaxed) != n)
        {
            let _ = child.kill();
            let _ = child.wait();
            timing.phase = if generation.is_some_and(|(g, n)| g.load(Ordering::Relaxed) != n) {
                "cancelled"
            } else {
                "timeout"
            };
            timing.helper_ms = start.elapsed().as_secs_f64() * 1000.;
            bail!(
                "Request {}. Please try again.",
                if timing.phase == "cancelled" {
                    "cancelled"
                } else {
                    "timed out"
                }
            );
        }
        thread::sleep(Duration::from_millis(if s.playback_request {
            15
        } else {
            80
        }));
    }
    timing.phase = "metadata";
    let parse_at = Instant::now();
    let result = metadata::read(&files.0);
    timing.parse_ms = parse_at.elapsed().as_secs_f64() * 1000.;
    if result.is_ok() {
        timing.phase = "ready";
    }
    result
}
struct HelperTiming {
    id: u64,
    backend: &'static str,
    peak_rss: u64,
    epoch: Option<u64>,
    started: Instant,
    queue_ms: f64,
    spawn_ms: f64,
    helper_ms: f64,
    parse_ms: f64,
    playback: bool,
    cached_info: bool,
    unpacked: bool,
    deno_reused: bool,
    exit_code: Option<i32>,
    phase: &'static str,
}
impl HelperTiming {
    fn new(id: u64, queue_ms: f64, playback: bool, cached_info: bool) -> Self {
        Self {
            id,
            backend: "cli",
            peak_rss: 0,
            epoch: None,
            started: Instant::now(),
            queue_ms,
            spawn_ms: 0.,
            helper_ms: 0.,
            parse_ms: 0.,
            playback,
            cached_info,
            unpacked: false,
            deno_reused: false,
            exit_code: None,
            phase: "prepare",
        }
    }
}
impl Drop for HelperTiming {
    fn drop(&mut self) {
        crate::diagnostics::record(
            "resolver_timing",
            serde_json::json!({
                "request":self.id,"backend":self.backend,"epoch":self.epoch,"playback":self.playback,"cached_info":self.cached_info,
                "helper_layout":if self.unpacked {"onedir"} else {"standalone"},
                "cli_peak_rss_bytes":self.peak_rss,
                "deno_cache_reused":self.deno_reused,"queue_ms":self.queue_ms,
                "spawn_ms":self.spawn_ms,"helper_ms":self.helper_ms,"parse_ms":self.parse_ms,
                "total_ms":self.queue_ms+self.started.elapsed().as_secs_f64()*1000.,
                "outcome":self.phase,"exit_code":self.exit_code
            }),
        );
    }
}
fn view_count(count: u64) -> String {
    let (value, suffix) = if count >= 1_000_000_000 {
        (count as f64 / 1_000_000_000., "B")
    } else if count >= 1_000_000 {
        (count as f64 / 1_000_000., "M")
    } else if count >= 1_000 {
        (count as f64 / 1_000., "K")
    } else {
        return format!("{count} views");
    };
    format!("{value:.1}{suffix} views")
}
#[derive(Clone)]
struct FormatOption {
    label: String,
    selector: Option<String>,
    quality: i32,
    music: bool,
}
fn default_format_options() -> Vec<FormatOption> {
    ["360p", "480p", "720p", "1080p", "Best", "Audio only"]
        .into_iter()
        .enumerate()
        .map(|(i, label)| FormatOption {
            label: label.into(),
            selector: None,
            quality: i.min(4) as i32,
            music: i == 5,
        })
        .collect()
}
fn estimated_bytes(format: &Value, duration: f64) -> Option<f64> {
    format["filesize"]
        .as_f64()
        .or(format["filesize_approx"].as_f64())
        .or_else(|| {
            format["tbr"]
                .as_f64()
                .filter(|_| duration > 0.)
                .map(|rate| rate * 1000. * duration / 8.)
        })
        .filter(|n| n.is_finite() && *n > 0.)
}
fn size_label(bytes: Option<f64>) -> String {
    bytes
        .map(|n| {
            if n >= 1_000_000_000. {
                format!(" · ≈{:.1} GB", n / 1_000_000_000.)
            } else {
                format!(" · ≈{:.0} MB", n / 1_000_000.)
            }
        })
        .unwrap_or_default()
}
fn playback_options(info: &Value, s: &Settings) -> (Vec<FormatOption>, usize) {
    let Some(formats) = info["formats"].as_array() else {
        return (
            default_format_options(),
            if s.music { 5 } else { s.quality as usize },
        );
    };
    let duration = info["duration"].as_f64().unwrap_or(0.);
    let audio = formats
        .iter()
        .filter(|f| {
            f["vcodec"] == "none"
                && f["acodec"].as_str().is_some_and(|v| v != "none")
                && f["has_drm"] != true
        })
        .max_by(|a, b| {
            a["abr"]
                .as_f64()
                .or(a["tbr"].as_f64())
                .unwrap_or(0.)
                .total_cmp(&b["abr"].as_f64().or(b["tbr"].as_f64()).unwrap_or(0.))
        });
    let audio_size = audio.and_then(|f| estimated_bytes(f, duration));
    let mut groups = std::collections::BTreeMap::<(u64, u64), &Value>::new();
    let rank = |f: &Value| {
        (
            s.hardware
                && f["vcodec"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("avc") || c.starts_with("h264")),
            matches!(f["protocol"].as_str(), Some("https" | "http")),
            (f["tbr"].as_f64().unwrap_or(0.) * 1000.) as u64,
        )
    };
    for f in formats {
        let Some(height) = f["height"].as_u64().filter(|h| (1..=2160).contains(h)) else {
            continue;
        };
        if f["vcodec"]
            .as_str()
            .is_none_or(|c| c == "none" || c == "images")
            || f["has_drm"] == true
            || f["format_id"].as_str().is_none()
        {
            continue;
        }
        let fps = (f["fps"].as_f64().unwrap_or(0.).max(0.) * 1000.).round() as u64;
        let entry = groups.entry((height, fps)).or_insert(f);
        if rank(f) > rank(entry) {
            *entry = f;
        }
    }
    if groups.is_empty() {
        return (
            default_format_options(),
            if s.music { 5 } else { s.quality as usize },
        );
    }
    let chosen = info["requested_formats"]
        .as_array()
        .and_then(|fs| fs.iter().find(|f| f["vcodec"] != "none"))
        .unwrap_or(info);
    let chosen_id = chosen["format_id"].as_str();
    let mut selected = None;
    let mut options = Vec::new();
    for ((height, fps), f) in groups {
        let id = f["format_id"].as_str().unwrap();
        let split = f["acodec"] == "none";
        let size = estimated_bytes(f, duration).and_then(|v| {
            if split {
                audio_size.map(|a| v + a)
            } else {
                Some(v)
            }
        });
        let rate = if fps > 0 {
            let rate = format!("{:.3}", fps as f64 / 1000.);
            format!("{} fps", rate.trim_end_matches('0').trim_end_matches('.'))
        } else {
            "fps unknown".into()
        };
        if chosen_id == Some(id)
            || (chosen["height"].as_u64() == Some(height)
                && (chosen["fps"].as_f64().unwrap_or(0.).max(0.) * 1000.).round() as u64 == fps)
        {
            selected = Some(options.len());
        }
        options.push(FormatOption {
            label: format!("{height}p · {rate}{}", size_label(size)),
            selector: Some(if split {
                format!("{id}+bestaudio")
            } else {
                id.into()
            }),
            quality: match height {
                0..=360 => 0,
                361..=480 => 1,
                481..=720 => 2,
                721..=1080 => 3,
                _ => 4,
            },
            music: false,
        });
    }
    let audio_size = if s.music {
        estimated_bytes(info, duration).or(audio_size)
    } else {
        audio_size
    };
    options.push(FormatOption {
        label: format!("Audio only{}", size_label(audio_size)),
        selector: Some("bestaudio".into()),
        quality: s.quality,
        music: true,
    });
    let selected = if s.music {
        options.len() - 1
    } else {
        selected.unwrap_or_else(|| {
            options
                .iter()
                .rposition(|o| !o.music && o.quality <= s.quality)
                .unwrap_or(0)
        })
    };
    (options, selected)
}
fn format_selector(s: &Settings) -> String {
    if let Some(selector) = &s.format_override {
        return selector.clone();
    }
    if s.music {
        return "bestaudio".into();
    }
    let height = match s.quality {
        0 => 360,
        1 => 480,
        2 => 720,
        3 => 1080,
        _ => 2160,
    };
    format!("bestvideo*[height<={height}]+bestaudio/best[height<={height}]")
}
fn video_from(v: &Value) -> Option<Video> {
    let id = v["id"].as_str()?;
    if id.len() != 11
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return None;
    }
    Some(Video {
        id: id.into(),
        title: v["title"].as_str().unwrap_or("Untitled").into(),
        channel: v["channel"]
            .as_str()
            .or(v["uploader"].as_str())
            .unwrap_or("YouTube")
            .into(),
        duration: v["duration"].as_f64().unwrap_or(0.),
        views: v["view_count"].as_u64(),
        published: published::metadata(v),
        music: recommendations::music_metadata(v),
        genres: music_taste::metadata_genres(v),
        unavailable: availability::metadata(v).0,
        audio_unavailable: availability::metadata(v).1,
    })
}
fn videos_from(v: &Value) -> Vec<Video> {
    if let Some(entries) = v["entries"].as_array() {
        entries
            .iter()
            .filter_map(video_from)
            .take(recommendations::MAX_RESULTS)
            .collect()
    } else {
        video_from(v).into_iter().collect()
    }
}
fn search_target(query: &str) -> Result<String> {
    let q = query.trim();
    if q.is_empty() {
        bail!("Type a search or a YouTube video URL.");
    }
    if let Some(uri) = crate::playlists::normalize_youtube_uri(q) {
        if let Some(id) = crate::playlists::youtube_id_from_uri(&uri) {
            return Ok(format!("https://www.youtube.com/watch?v={id}"));
        }
        return Ok(uri);
    }
    if q.contains("://") || q.starts_with("//") {
        bail!("Please use a valid YouTube URL or search words.");
    }
    Ok(format!("ytsearch30:{q}"))
}
fn compact_quality(label: &str) -> String {
    if label.starts_with("Audio only") {
        return "Audio".into();
    }
    let mut parts = label.split('·').map(str::trim);
    let height = parts.next().unwrap_or("");
    let size = match height {
        "2160p" => "4k",
        _ => height.strip_suffix('p').unwrap_or(height),
    };
    let fps = parts
        .next()
        .and_then(|v| v.strip_suffix(" fps"))
        .filter(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'));
    if let Some(fps) = fps {
        format!("{size}@{fps}")
    } else {
        size.into()
    }
}
fn compact_labels(options: &[FormatOption]) -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(
        options
            .iter()
            .map(|o| compact_quality(&o.label).into())
            .collect::<Vec<_>>(),
    ))
}
enum Browse {
    Search(String),
    Artist { uri: String, title: String, sort: i32 },
    Related(Video),
    MusicDiscovery(Video),
    FromLikes,
    Home,
    History,
    Downloads,
    Restore(session::Collection),
}
struct BrowseRequest {
    task: Browse,
    settings: Settings,
    generation: u64,
}
enum PlayerCommand {
    Play(Video, Settings, u64),
    Pause,
    Reset,
    Shutdown,
    Seek(f64, u64),
    Volume(f64),
    Connection(bool),
    RepeatVideo(bool),
    RepeatMusic(bool),
    #[cfg(target_os="windows")]
    Interpolation(i32,bool),
    SeekState(f64, bool, u64),
}
pub struct Runtime {
    player: mpsc::Sender<PlayerCommand>,
    generation: Arc<AtomicU64>,
    _controls_timer: std::rc::Rc<slint::Timer>,
    #[cfg(target_os = "windows")]
    _native_timer: Option<slint::Timer>,
    downloads: download::Guard,
    _captions: captions::Guard,
    _skin: crate::music_skin::Guard,
    _pages: pages::Guard,
    _anime: sources::Guard,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        PLAYBACK_PENDING.store(0, Ordering::Release);
        HELPER_READY.notify_all();
        self.downloads.stop();
        let _ = self.player.send(PlayerCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn attach(w: &SetupWindow, smoke: bool) -> Runtime {
    w.set_native_gpu_only(cfg!(target_os = "windows"));
    about::install(w);
    description::install(w);
    let controls_timer = std::rc::Rc::new(slint::Timer::default());
    let timer = controls_timer.clone();
    let weak = w.as_weak();
    w.on_player_activity(move || {
        if let Some(w) = weak.upgrade() {
            w.set_controls_shown(true);
            let weak = w.as_weak();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_secs(3),
                move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_controls_shown(false);
                    }
                },
            );
        }
    });
    let mut paths = Paths::new();
    if smoke && std::env::var_os("MOCHUNE_TEST_OFFLINE").is_some() {
        paths.network = connectivity::Connection::new(true);
    }
    let mut loaded: Settings = read_json(paths.data.join("settings.json"));
    // Migrate obsolete interests/source preferences without losing history.
    loaded.interests.clear();
    loaded.recommendation_mode = 0;
    loaded.minimum_recommendation_views = [10_000, 20_000, 30_000, 40_000, 50_000]
        [minimum_views_choice(loaded.minimum_recommendation_views) as usize];
    if PathBuf::from(&loaded.download_folder)
        .file_name()
        .is_some_and(|name| name == "ytlite-videos")
    {
        loaded.download_folder = download::default_folder().to_string_lossy().into_owned();
    }
    loaded.quality = loaded.quality.clamp(0, 4);
    if std::env::args().any(|a| a == "--start-music") {
        loaded.last_music_mode = true;
    }
    if std::env::args().any(|a| a == "--start-video") {
        loaded.last_music_mode = false;
    }
    #[cfg(target_os = "linux")]
    {
        loaded.last_music_mode = linux_renderer::startup_music();
    }
    loaded.music = loaded.last_music_mode;
    w.set_music_layout(loaded.last_music_mode);
    loaded.volume = sanitize_volume(loaded.volume);
    loaded.audio_cache_limit_mib =
        [128, 256, 512, 1024][limit_choice(loaded.audio_cache_limit_mib) as usize];
    apply_settings(w, &loaded);
    let skin_guard = crate::music_skin::install(w);
    paths.audio.install(w);
    if let Err(e) = paths
        .audio
        .configure(loaded.audio_cache_enabled, loaded.audio_cache_limit_mib)
    {
        w.set_cache_status(format!("Music cache: {e}").into());
    }
    paths
        .cache_policy
        .set(loaded.cache_enabled, paths.cache.clone());
    w.set_cache_status(paths.audio.summary().into());
    w.set_downloaded_count(paths.audio.count() as i32);
    let mut history_error = None;
    let history = if loaded.remember {
        match history::load(&paths) {
            Ok(history) => history,
            Err(error) => {
                // Do not overwrite unreadable history with an empty session.
                history_error = Some(format!("{error:#}"));
                w.set_status(format!("History unavailable; saving disabled: {error:#}").into());
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    let recent_played = [
        recommendations::recent_ids(&history[0]),
        recommendations::recent_ids(&history[1]),
    ];
    let state = Arc::new(Mutex::new(State {
        active_music: loaded.last_music_mode,
        settings: loaded,
        history,
        history_error,
        recent_played,
        ..State::default()
    }));
    let controls_state = state.clone();
    let controls_paths = paths.clone();
    let controls_weak = w.as_weak();
    w.on_toggle_controls_position(move || {
        let Some(w) = controls_weak.upgrade() else {
            return;
        };
        if w.get_fullscreen_mode() {
            return;
        }
        let outside = !w.get_controls_outside();
        w.set_controls_outside(outside);
        let mut state = controls_state.lock().unwrap();
        state.settings.controls_outside = outside;
        if controls_paths
            .save("settings.json", &state.settings)
            .is_err()
        {
            w.invoke_notify("Control position changed for this session; could not save it.".into());
        }
    });
    browse_view::install(w, state.clone());
    settings_terms::install(w);
    let repaint = w.as_weak();
    w.on_metadata_changed(move || {
        if let Some(w) = repaint.upgrade() {
            crate::repaint_window(w.window());
            // Present again after deferred text/layout changes, never per frame.
            let weak = w.as_weak();
            slint::Timer::single_shot(Duration::from_millis(16), move || {
                if let Some(w) = weak.upgrade() {
                    crate::repaint_window(w.window());
                }
            });
        }
    });
    let mut links = stream_cache::StreamCache::default();
    links.configure(state.lock().unwrap().settings.stream_cache_enabled);
    let stream_cache = Arc::new(Mutex::new(links));
    let generation = Arc::new(AtomicU64::new(0));
    let (browse_tx, browse_rx) = mpsc::channel::<BrowseRequest>();
    let weak = w.as_weak();
    let st = state.clone();
    let p = paths.clone();
    let gen_id = generation.clone();
    let cache = stream_cache.clone();
    thread::spawn(move || browse_worker(browse_rx, weak, st, p, gen_id, cache));
    let (player_tx, player_rx) = mpsc::channel();
    let interpolation_state=state.clone();let interpolation_paths=paths.clone();let weak=w.as_weak();
    #[cfg(target_os="windows")]
    let interpolation_tx=player_tx.clone();
    w.on_change_interpolation(move |mode|{
        let Some(w)=weak.upgrade()else{return;};
        if !w.get_interpolation_supported(){return;}
        let mode=mode.clamp(0,2);w.set_interpolation_mode(mode);
        let mut state=interpolation_state.lock().unwrap();state.settings.interpolation_mode=mode;state.settings.interpolation_enabled=mode>0;
        if interpolation_paths.save("settings.json",&state.settings).is_err(){w.invoke_notify("Interpolation changed for this session; could not save it.".into());}
        drop(state);
        #[cfg(target_os="windows")]
        if !w.get_music_only() && (w.get_playback_active() || w.get_resolving() || w.get_video_visible()) {
            let _=interpolation_tx.send(PlayerCommand::Interpolation(mode,w.get_interpolation_auto_fallback()));
        }
    });

    let player_generation = Arc::new(AtomicU64::new(0));
    let pg = player_generation.clone();
    let weak = w.as_weak();
    let st = state.clone();
    let p = paths.clone();
    let engine = crate::video::install(w);
    #[cfg(target_os = "windows")]
    let native_timer = engine.as_ref().ok().and_then(|e| e.native_timer(w));
    let local = if smoke {
        std::env::var("MOCHUNE_TEST_MEDIA").ok()
    } else {
        None
    };
    if let Some(path) = &local {
        let video = Video {
            id: if path.starts_with("http://") || path.starts_with("https://") {
                "cachetest01"
            } else {
                "local-test"
            }
            .into(),
            title: Path::new(path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            channel: "Local playback test".into(),
            duration: 0.,
            views: None,
            ..Video::default()
        };
        w.set_selected_id(video.id.clone().into());
        w.set_selected_title(video.title.clone().into());
        w.set_selected_channel(video.channel.clone().into());
        w.set_has_selection(true);
        if std::env::var_os("MOCHUNE_SMOKE_VIDEO_QUEUE").is_some() {
            let mut first = video.clone();
            first.title = "First video".into();
            let mut second = video.clone();
            second.id = "second-test".into();
            second.title = "Second video".into();
            state.lock().unwrap().videos = vec![first.clone(), second.clone()];
            browse_view::publish(&w, vec![first.card(), second.card()]);
        }
        state.lock().unwrap().selected = Some(video);
    }
    let cache = stream_cache.clone();
    let seek_revision = Arc::new(AtomicU64::new(0));
    let worker_seek_revision = seek_revision.clone();
    let worker = thread::spawn(move || {
        player_worker(
            player_rx,
            weak,
            st,
            p,
            pg,
            engine,
            local,
            cache,
            worker_seek_revision,
        )
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let gen_id = generation.clone();
    let auth = state.clone();
    w.on_search(move |query| {
        if let Some(w) = weak.upgrade() {
            if w.get_anime_mode() { w.invoke_anime_search(query); return; }
            let artist_query = pages::channel_search_query(&query);
            w.set_music_artist_search(w.get_music_layout() && artist_query.is_some());
            if let Some(uri) = pages::channel_url(&query) {
                if w.get_music_layout() {
                    w.set_music_artist_search(false);
                    w.global::<crate::BrowsePages>().invoke_close_page();
                    let title=uri.rsplit('/').next().unwrap_or("Artist").to_string();
                    queue(&w,&tx,&gen_id,Browse::Artist{uri,title,sort:w.get_music_sort()},&auth);
                } else {
                    w.global::<crate::BrowsePages>().invoke_open_channel(uri.into());
                }
                return;
            }
            if !w.get_music_layout() && matches!(w.global::<crate::BrowsePages>().get_page(), 1 | 2 | 3) {
                w.global::<crate::BrowsePages>().invoke_search_home(query);
                return;
            }
            w.global::<crate::BrowsePages>().invoke_close_page();
            let video_query = artist_query.unwrap_or(query.as_str());
            queue(&w, &tx, &gen_id, Browse::Search(video_query.into()), &auth);
            w.global::<crate::BrowsePages>().invoke_search_channels(query);
        }
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let gen_id = generation.clone();
    let auth = state.clone();
    w.on_open_music_artist(move |index| {
        let Some(w) = weak.upgrade() else { return; };
        if !w.get_music_layout() || !(0..3).contains(&index) { return; }
        let Some(artist) = w.global::<crate::BrowsePages>().get_channel_hits().row_data(index as usize) else { return; };
        let Some(uri) = pages::channel_url(&artist.uri) else { return; };
        w.set_music_artist_search(false);
        w.global::<crate::BrowsePages>().invoke_close_page();
        queue(&w, &tx, &gen_id, Browse::Artist { uri, title: artist.title.to_string(), sort:w.get_music_sort() }, &auth);
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let gen_id = generation.clone();
    let auth = state.clone();
    let sort_timer=slint::Timer::default();
    w.on_artist_sort_changed(move |sort| {
        let weak=weak.clone(); let tx=tx.clone(); let gen_id=gen_id.clone(); let auth=auth.clone();
        sort_timer.start(slint::TimerMode::SingleShot,Duration::from_millis(1),move || {
            let Some(w)=weak.upgrade() else {return;};
            if !w.get_music_layout() || w.get_browse_tab()!=6 {return;}
            let artist=auth.lock().unwrap().music_artist.clone();
            if let Some((uri,title,previous))=artist.filter(|(_,_,previous)| *previous!=sort) {
                let _=previous;
                queue(&w,&tx,&gen_id,Browse::Artist{uri,title,sort},&auth);
            }
        });
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let gen_id = generation.clone();
    let auth = state.clone();
    w.on_home(move || {
        if let Some(w) = weak.upgrade() {
            queue(&w, &tx, &gen_id, Browse::Home, &auth);
        }
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let gen_id = generation.clone();
    let auth = state.clone();
    w.on_new_recommendations(move || {
        if let Some(w) = weak.upgrade() {
            // Refresh the active context, never replace a playlist or search silently.
            let task = if w.get_music_layout() && w.get_browse_tab() == 5 {
                Browse::FromLikes
            } else if w.get_browse_tab() == 1 {
                auth.lock()
                    .unwrap()
                    .selected
                    .clone()
                    .map(|v| {
                        if w.get_music_layout() {
                            Browse::MusicDiscovery(v)
                        } else {
                            Browse::Related(v)
                        }
                    })
                    .unwrap_or(Browse::Home)
            } else {
                Browse::Home
            };
            let st = auth.lock().unwrap();
            let avoid = st.recent_played[usize::from(w.get_music_layout())].clone();
            let defer = st.videos.iter().take(50).map(|v| v.id.clone()).collect();
            drop(st);
            queue_refresh(&w, &tx, &gen_id, task, &auth, avoid, defer);
        }
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let gen_id = generation.clone();
    let auth = state.clone();
    w.on_downloads(move || {
        if let Some(w) = weak.upgrade() {
            queue(&w, &tx, &gen_id, Browse::Downloads, &auth);
        }
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let gen_id = generation.clone();
    let auth = state.clone();
    w.on_history(move || {
        if let Some(w) = weak.upgrade() {
            queue(&w, &tx, &gen_id, Browse::History, &auth);
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    let disk = paths.clone();
    w.on_remove_history(move |id| {
        let Some(w) = weak.upgrade() else {
            return;
        };
        let mut st = st.lock().unwrap();
        let mode = usize::from(w.get_music_layout());
        if let Some(error) = &st.history_error {
            w.set_status(format!("History unavailable; original files preserved: {error}").into());
            return;
        }
        let mut history = st.history.clone();
        history[mode].retain(|v| v.id != id.as_str());
        if let Err(e) = history::save(&disk, &history) {
            w.set_status(format!("Could not remove history entry: {e}").into());
            return;
        }
        st.history = history;
        if st.removed_history[mode].len() >= 100 {
            st.removed_history[mode].clear();
        }
        st.removed_history[mode].insert(id.to_string());
        if w.get_history_view() {
            st.videos.retain(|v| v.id != id.as_str());
            let cards = w
                .get_videos()
                .iter()
                .filter(|v| v.source_id != id)
                .collect::<Vec<_>>();
            browse_view::publish(&w, cards);
            w.set_status(
                format!(
                    "Local {} history · {} entries",
                    if mode == 1 { "music" } else { "video" },
                    st.videos.len()
                )
                .into(),
            );
        }
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let gen_id = generation.clone();
    let auth = state.clone();
    w.on_video_started(move |id| {
        if let Some(w) = weak.upgrade() {
            let discover = {
                let mut st = auth.lock().unwrap();
                let ready = w.get_music_layout()
                    && st.music_discover_pending
                    && st.selected.as_ref().is_some_and(|v| v.id == id.as_str())
                    && st.play_browse_epoch == gen_id.load(Ordering::Relaxed);
                if ready {
                    st.music_discover_pending = false;
                }
                ready
            };
            if discover {
                let seed = auth.lock().unwrap().selected.clone();
                if let Some(seed) = seed {
                    let rows = {
                        let mut st = auth.lock().unwrap();
                        let rows = music::discovery_rows(Some(&seed), Vec::new(), 40);
                        st.videos = rows.clone();
                        rows
                    };
                    let previous = w.get_videos();
                    browse_view::publish(
                        &w,
                        rows.iter()
                            .map(|v| {
                                previous
                                    .iter()
                                    .find(|row| row.source_id.as_str() == v.id)
                                    .unwrap_or_else(|| v.card())
                            })
                            .collect(),
                    );
                    queue(&w, &tx, &gen_id, Browse::MusicDiscovery(seed), &auth);
                }
                w.set_browse_tab(1);
                return;
            }
            let video = {
                let mut st = auth.lock().unwrap();
                if w.get_music_layout()
                    || !w.get_playlist_id().is_empty()
                    || !st.settings.auto_related
                    || crate::playlists::youtube_id_from_uri(&format!("https://youtu.be/{id}"))
                        .is_none()
                    || st.related_for.as_deref() == Some(id.as_str())
                    || st.play_browse_epoch != gen_id.load(Ordering::Relaxed)
                {
                    return;
                }
                let Some(video) = st.selected.clone().filter(|v| v.id == id.as_str()) else {
                    return;
                };
                st.related_for = Some(video.id.clone());
                video
            };
            w.set_related_after_frame(w.get_video_visible());
            queue(&w, &tx, &gen_id, Browse::Related(video), &auth);
        }
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let auth = state.clone();
    let gen_id = generation.clone();
    w.on_from_likes(move || {
        if let Some(w) = weak.upgrade().filter(|w| w.get_music_layout()) {
            queue(&w, &tx, &gen_id, Browse::FromLikes, &auth);
        }
    });
    let tx = browse_tx.clone();
    let weak = w.as_weak();
    let auth = state.clone();
    let gen_id = generation.clone();
    w.on_related(move || {
        if let Some(w) = weak.upgrade() {
            let video = auth.lock().unwrap().selected.clone();
            if let Some(video) = video {
                queue(
                    &w,
                    &tx,
                    &gen_id,
                    if w.get_music_layout() {
                        Browse::MusicDiscovery(video)
                    } else {
                        Browse::Related(video)
                    },
                    &auth,
                );
            } else {
                w.set_status("Pick a video to find related suggestions.".into());
            }
        }
    });
    let st = state.clone();
    let weak = w.as_weak();
    let selection_epoch = generation.clone();
    let selection_audio = paths.audio.clone();
    w.on_select(move |index| {
        if let Some(w) = weak.upgrade() {
            let video = st.lock().unwrap().videos.get(index as usize).cloned();
            if let Some(v) = video {
                if !v.playable(w.get_music_layout())
                    && !(w.get_music_layout() && selection_audio.contains(&v.id))
                {
                    w.set_status(
                        "This source is preserved in your playlist but cannot be played yet."
                            .into(),
                    );
                    return;
                }
                selection_epoch.fetch_add(1, Ordering::Relaxed);
                w.set_busy(false);
                select(&w, &v, index);
                {
                    let mut st = st.lock().unwrap();
                    st.resume_position = None;
                    st.music_discover_pending = w.get_music_layout()
                        && w.get_browse_tab() == -1
                        && !w.get_offline()
                        && w.get_playlist_id().is_empty();
                    st.music_follow_discover = false;
                    st.music_search_anchor = None;
                    st.removed_history[usize::from(w.get_music_layout())].remove(&v.id);
                    st.related_for = None;
                    if w.get_music_layout() {
                        st.queue_is_playlist = !w.get_playlist_id().is_empty();
                        if st.music_discover_pending {
                            music::begin_search_discovery(&mut st, &v);
                        } else {
                            st.queue = w
                                .get_rec_indices()
                                .iter()
                                .filter_map(|i| st.videos.get(i as usize))
                                .filter(|v| !v.id.is_empty())
                                .take(play_queue::LIMIT)
                                .cloned()
                                .collect();
                        }
                    } else {
                        play_queue::current(&mut st, v.clone());
                    }
                    st.queue_index = st.queue.iter().position(|q| q.id == v.id).unwrap_or(0);
                    if w.get_music_layout() {
                        st.music_playlist_pending = false;
                        st.music_shuffle_visited.clear();
                    }
                    st.selected = Some(v.clone());
                    st.format_options.clear();
                    st.formats_for = None;
                }
                play_queue::publish(&w, &st);
                music::cover(&st, v);
                w.invoke_play();
            }
        }
    });
    let st = state.clone();
    let weak = w.as_weak();
    let tx = player_tx.clone();
    let pg = player_generation.clone();
    let audio_for_play = paths.audio.clone();
    let browse_epoch = generation.clone();
    w.on_play(move || {
        if let Some(w) = weak.upgrade() {
            if w.get_anime_mode() { w.invoke_anime_replay(); return; }
            let (video, option) = {
                let st = st.lock().unwrap();
                let option = (st.formats_for.as_ref() == st.selected.as_ref().map(|v| &v.id))
                    .then(|| {
                        st.format_options
                            .get(w.get_quality_choice() as usize)
                            .cloned()
                    })
                    .flatten();
                (st.selected.clone(), option)
            };
            if let Some(video) = video {
                if !video.playable(w.get_music_layout())
                    && !(w.get_music_layout() && audio_for_play.contains(&video.id))
                {
                    w.set_player_error(
                        if video.unavailable.is_empty() {
                            "Audio-only stream unavailable"
                        } else {
                            &video.unavailable
                        }
                        .into(),
                    );
                    return;
                }
                let n = pg.fetch_add(1, Ordering::Relaxed) + 1;
                PLAYBACK_PENDING.store(n, Ordering::Release);
                browse_epoch.fetch_add(1, Ordering::Relaxed);
                w.set_busy(false);
                let mut s = settings(&w);
                s.playback_request = true;
                if w.get_music_layout() {
                    s.music = true;
                }
                {
                    let mut st = st.lock().unwrap();
                    committed_preferences(&mut s, &st.settings);
                    s.resume_position = st.resume_position.take().unwrap_or(0.);
                    st.play_browse_epoch = browse_epoch.load(Ordering::Relaxed);
                    if !w.get_music_layout() {
                        play_queue::current(&mut st, video.clone());
                    } else if st.queue.is_empty() {
                        st.queue_is_playlist = false;
                        st.queue = vec![video.clone()];
                        st.queue_index =
                            st.queue.iter().position(|v| v.id == video.id).unwrap_or(0);
                    }
                }
                play_queue::publish(&w, &st);
                if let Some(option) =
                    option.filter(|o| o.music == s.music && (o.music || o.quality == s.quality))
                {
                    s.format_override = option.selector;
                }
                w.set_playing_download(s.music && audio_for_play.contains(&video.id));
                if s.music && !w.get_music_layout() {
                    music::audio_cover(&st, video.clone());
                } else {
                    music::cover(&st, video.clone());
                    if !w.get_music_layout() {
                        w.set_selected_image(Default::default());
                    }
                }
                w.set_related_after_frame(false);
                w.set_recommendations_pending(!s.music);
                w.set_playback_active(false);
                w.set_resolving(true);
                w.set_player_error("".into());
                w.set_playback_status("Resolving YouTube streams…".into());
                let _ = tx.send(PlayerCommand::Play(video, s, n));
            }
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    let clicks = std::rc::Rc::new(slint::Timer::default());
    let click_state = std::rc::Rc::new(std::cell::RefCell::new(None::<(Instant, bool, bool)>));
    let quality_clicks = clicks.clone();
    let quality_click_state = click_state.clone();
    w.on_choose_quality(move |index| {
        // A deferred surface click must not pause the replacement stream after the menu closes.
        quality_clicks.stop();
        quality_click_state.borrow_mut().take();
        if let Some(w) = weak.upgrade() {
            if w.get_anime_mode() { w.global::<crate::Sources>().invoke_choose_quality(index); return; }
            let option = {
                let st = st.lock().unwrap();
                st.format_options.get(index as usize).cloned()
            }
            .or_else(|| default_format_options().get(index as usize).cloned());
            if let Some(option) = option {
                crate::diagnostics::record("quality_selected",serde_json::json!({"index":index,"previous_index":w.get_quality_choice(),"playing":w.get_playback_active(),"resolving":w.get_resolving(),"native":w.get_native_video_enabled()}));
                w.set_quality_choice(index);
                w.set_music_only(option.music);
                if !option.music {
                    w.set_quality_index(option.quality);
                }
                if w.get_duration() > 0. || w.get_video_visible() {
                    w.invoke_play();
                }
            }
        }
    });
    let tx = player_tx.clone();
    w.on_repeat_song_changed(move |repeat| {
        let _ = tx.send(PlayerCommand::RepeatMusic(repeat));
    });
    let tx = player_tx.clone();
    let weak = w.as_weak();
    w.on_toggle_video_repeat(move || {
        if let Some(w) = weak.upgrade() {
            let repeat = !w.get_repeat_video();
            w.set_repeat_video(repeat);
            let _ = tx.send(PlayerCommand::RepeatVideo(repeat));
        }
    });
    // Defer a single surface click so a double click can seek without pausing.
    let timer = clicks.clone();
    let state_click = click_state.clone();
    let weak = w.as_weak();
    w.on_player_click(move || {
        if let Some(w) = weak.upgrade() {
            w.invoke_player_activity();
            let old = *state_click.borrow();
            if old.is_none_or(|(t, _, _)| t.elapsed() > Duration::from_millis(700)) {
                *state_click.borrow_mut() = Some((Instant::now(), w.get_paused(), false));
            }
            let weak = w.as_weak();
            let state_click = state_click.clone();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(350),
                move || {
                    if let Some(w) = weak.upgrade()
                        && !w.get_resolving()
                        && w.get_has_selection()
                    {
                        if let Some(s) = state_click.borrow_mut().as_mut() {
                            s.2 = true;
                        }
                        if w.get_position() > 0. {
                            w.invoke_toggle_pause();
                        } else {
                            w.invoke_play();
                        }
                    }
                },
            );
        }
    });
    let feedback = std::rc::Rc::new(slint::Timer::default());
    let timer = clicks;
    let state_click = click_state;
    let weak = w.as_weak();
    let tx = player_tx.clone();
    let double_seek_revision = seek_revision.clone();
    w.on_player_double_click(move |region| {
        timer.stop();
        if let Some(w) = weak.upgrade() {
            let old = state_click.borrow_mut().take();
            let paused = old.map(|(_, p, _)| p).unwrap_or(w.get_paused());
            w.invoke_player_activity();
            if w.get_duration() > 0. && !(0.35..=0.65).contains(&region) {
                let target = (w.get_position() + if region < 0.35 { -5. } else { 5. })
                    .clamp(0., w.get_duration());
                if !w.get_playback_active() && target >= w.get_duration() - 0.25 {
                    return;
                }
                let target = target.min((w.get_duration() - 0.25).max(0.));
                w.set_position(target);
                w.set_resolving(true);
                w.set_seek_feedback(if region < 0.35 { "−5 s" } else { "+5 s" }.into());
                w.set_seek_feedback_right(region > 0.65);
                let weak = w.as_weak();
                feedback.start(
                    slint::TimerMode::SingleShot,
                    Duration::from_millis(650),
                    move || {
                        if let Some(w) = weak.upgrade() {
                            w.set_seek_feedback("".into());
                        }
                    },
                );
                let _ = tx.send(PlayerCommand::SeekState(
                    target as f64,
                    paused,
                    double_seek_revision.fetch_add(1, Ordering::AcqRel) + 1,
                ));
            } else if (0.35..=0.65).contains(&region) {
                if old.is_some_and(|(_, _, fired)| fired) {
                    let _ = tx.send(PlayerCommand::Pause);
                }
                w.invoke_toggle_fullscreen();
            }
        }
    });
    let tx = player_tx.clone();
    let pause_weak = w.as_weak();
    w.on_toggle_pause(move || {
        if let Some(w) = pause_weak.upgrade()
            && !w.get_playback_active()
        {
            w.invoke_play();
            return;
        }
        let _ = tx.send(PlayerCommand::Pause);
    });
    let tx = player_tx.clone();
    let weak = w.as_weak();
    let request_seek_revision = seek_revision.clone();
    w.on_seek(move |v| {
        if let Some(w) = weak.upgrade()
            && w.get_duration() > 0.
            && w.get_has_selection()
        {
            if !w.get_playback_active() && v >= w.get_duration() - 0.25 {
                return;
            }
            if !v.is_finite() {
                return;
            }
            let v = v.clamp(0., (w.get_duration() - 0.25).max(0.));
            let revision = request_seek_revision.fetch_add(1, Ordering::AcqRel) + 1;
            crate::diagnostics::record(
                "seek_requested",
                serde_json::json!({"target":v,"from":w.get_position(),"revision":revision}),
            );
            if !w.get_playback_active() {
                w.set_resolving(true);
            }
            w.set_position(v);
            w.set_clock(format!("{} / {}", clock(v as f64), clock(w.get_duration() as f64)).into());
            let _ = tx.send(PlayerCommand::Seek(v as f64, revision));
        }
    });
    let tx = player_tx.clone();
    let weak = w.as_weak();
    w.on_volume_change(move |v| {
        let value = sanitize_volume(v);
        if let Some(w) = weak.upgrade() {
            w.set_volume(value);
        }
        let _ = tx.send(PlayerCommand::Volume(value as f64));
    });
    let tx = player_tx.clone();
    let weak = w.as_weak();
    let connection_player = player_generation.clone();
    let connection_browse = generation.clone();
    w.on_connection_changed(move |online| {
        if !online && let Some(w) = weak.upgrade() {
            if w.get_resolving() && !w.get_playing_download() {
                let cancelled = connection_player.fetch_add(1, Ordering::Relaxed);
                finish_playback_priority(cancelled);
                w.set_recommendations_pending(false);
                w.set_playback_active(false);
                w.set_resolving(false);
                w.set_player_error("Offline — choose a completed download in Music.".into());
            }
            if w.get_busy() {
                connection_browse.fetch_add(1, Ordering::Relaxed);
                w.set_busy(false);
                if w.get_music_layout() {
                    w.invoke_downloads();
                } else {
                    w.set_status("Offline — online video search is unavailable.".into());
                }
            }
        }
        let _ = tx.send(PlayerCommand::Connection(online));
        if !online
            && let Some(w) = weak.upgrade()
            && w.get_music_layout()
            && w.get_videos().row_count() == 0
            && !w.get_busy()
        {
            w.invoke_downloads();
        }
    });
    paths.network.install(w);
    let disk = paths.clone();
    let links = stream_cache.clone();
    let weak = w.as_weak();
    w.on_clear_cache(move|category|{
        let disk=disk.clone();let links=links.clone();let weak=weak.clone();
        thread::spawn(move||{
            let mut errors=Vec::new();
            if category==0||category==3 {if let Err(e)=disk.cache_policy.clear(&disk.cache){errors.push(format!("Thumbnails: {e}"));}}
            if category==1||category==3 {links.lock().unwrap().clear();}
            if category==2||category==3 {if let Err(e)=disk.audio.clear(){errors.push(format!("Music: {e}"));}}
            let result:Result<()>=if errors.is_empty(){Ok(())}else{Err(anyhow::anyhow!(errors.join("; ")))};
            let summary=disk.audio.summary();
            ui(&weak,move|w|{
                w.set_downloaded_count(disk.audio.count() as i32);
                w.set_cache_status(match result {Ok(())=>format!("Cache cleared. {summary}. Any playing saved file is released when playback stops."),Err(e)=>format!("Could not clear all cache files: {e}")}.into());
                let model=w.get_videos();for i in 0..model.row_count(){if let Some(mut card)=model.row_data(i){card.downloaded=disk.audio.contains(card.source_id.as_str());if category==0||category==3 {card.thumbnail=Default::default();}model.set_row_data(i,card);}}
                if category==0||category==3{w.set_selected_image(Default::default());}
                if (category==2||category==3)&&w.get_collection_title()=="Cached music" {w.invoke_downloads();}
            });
        });
    });
    play_queue::install(w, state.clone(), paths.clone());
    music::install(w, state.clone(), paths.clone(), generation.clone(), smoke);
    session::install(
        w,
        state.clone(),
        paths.clone(),
        generation.clone(),
        player_generation.clone(),
        player_tx.clone(),
        browse_tx.clone(),
        smoke,
    );
    let downloads = download::install(w, state.clone(), paths.clone());
    let caption_guard = captions::install(w, paths.clone());
    state.lock().unwrap().captions = Some(caption_guard.controller.clone());
    let cancel_state = state.clone();
    let weak = w.as_weak();
    w.on_cancel_settings(move || {
        if let Some(w) = weak.upgrade() {
            let s = cancel_state.lock().unwrap().settings.clone();
            s.audio_effects.show(&w);
            w.set_download_folder(s.download_folder.clone().into());
            w.set_light_mode(s.light_mode);
            w.set_immersive(s.immersive);
            w.set_launch_maximized(s.launch_maximized);
            w.set_interpolation_mode(if w.get_interpolation_supported() {s.interpolation_mode.clamp(0,2).max(if s.interpolation_enabled{1}else{0})} else {0});
            w.set_interpolation_auto_fallback(s.interpolation_auto_fallback);
            w.set_music_extra_card(s.music_extra_card);
            w.set_music_skin_path(s.music_skin_path.clone().into());
            w.set_music_skin_choice(s.music_skin.max(0));
            // A changed index may invoke selection against a reordered catalog.
            // Restore the saved identity before asking the loader to resolve it.
            w.set_music_skin_path(s.music_skin_path.clone().into());
            w.set_skin_animations(s.skin_animations);
            w.set_theme_header(s.theme_header);
            w.set_theme_library(s.theme_library);
            w.set_theme_list(s.theme_list);
            w.set_theme_player(s.theme_player);
            w.set_theme_video(s.theme_video);
            w.invoke_skin_reload();
            w.set_recommendation_mode(s.recommendation_mode.clamp(0, 2));
            w.set_recommendation_count(recommendations::video_limit_choice(s.recommendation_count));
            w.set_minimum_views(minimum_views_choice(s.minimum_recommendation_views));
            w.set_auto_related(s.auto_related);
            w.set_use_history(s.use_history);
            w.set_hide_watched(s.hide_watched);
            w.set_hide_alternate_versions(s.hide_alternate_versions);
            w.set_blocked_channels(s.blocked_channels.into());
            w.set_blocked_keywords(s.blocked_keywords.into());
            w.set_interests(s.interests.into());
            w.set_remember(s.remember);
            w.set_hardware(cfg!(target_os = "windows") || s.hardware);
            w.set_cache_enabled(s.cache_enabled);
            w.set_stream_cache_enabled(s.stream_cache_enabled);
            w.set_audio_cache_enabled(s.audio_cache_enabled);
            w.set_audio_cache_limit(limit_choice(s.audio_cache_limit_mib));

            w.set_settings_open(false);
        }
    });

    let weak = w.as_weak();
    w.on_pick_audio_impulse(move || {
        let weak = weak.clone();
        thread::spawn(move || {
            let Some(file) = rfd::FileDialog::new().set_title("Choose impulse response").add_filter("Impulse response", &["irs", "wav"]).pick_file() else { return; };
            let path = file.to_string_lossy().into_owned();
            let result = crate::audio_effects::validate(&path);
            let _ = weak.upgrade_in_event_loop(move |w| {
                if !w.get_settings_open() { return; }
                w.set_audio_impulse_source(1);
                w.set_audio_impulse_path(path.into());
                match result {
                    Ok(info) => w.set_audio_impulse_status(info.into()),
                    Err(error) => { w.set_audio_convolver_enabled(false); w.set_audio_impulse_status(format!("Convolution off: {error:#}").into()); }
                }
            });
        });
    });
    audio_collection::install(w, paths.data.join("irs-collection"));
    let resolver_cache = stream_cache.clone();
    let st = state.clone();
    let weak = w.as_weak();
    let p = paths.clone();
    w.on_save_settings(move || {
        if let Some(w) = weak.upgrade() {
            let mut s = settings(&w);
            if s.audio_effects.convolver {
                match s.audio_effects.validate() {
                    Ok(info) => w.set_audio_impulse_status(info.into()),
                    Err(error) => {
                        s.audio_effects.convolver = false;
                        w.set_audio_convolver_enabled(false);
                        w.set_audio_impulse_status(format!("Convolution off: {error:#}").into());
                        w.invoke_notify(format!("Convolution off: {error:#}").into());
                    }
                }
            }
            let mut st = st.lock().unwrap();
            if s.remember && !st.settings.remember && st.history_error.is_none() {
                match history::load(&p) {
                    Ok(history) => st.history = history,
                    Err(error) => st.history_error = Some(format!("{error:#}")),
                }
            }
            if s.remember && let Some(error) = &st.history_error {
                w.set_status(format!("History unavailable: {error}. Repair the saved files and restart, or turn remembered history off to erase them.").into());
                return;
            }
            let result = p.save("settings.json", &s);
            if let Err(e) = &result {
                w.set_status(format!("Could not save settings: {e}").into());
                return;
            }
            let restart = (w.get_playback_active() || w.get_paused() || w.get_resolving()) && (s.audio_effects != st.settings.audio_effects || (!s.music && s.hardware != st.settings.hardware) || (cfg!(target_os="windows") && !s.music && s.interpolation_auto_fallback != st.settings.interpolation_auto_fallback));
            if restart { st.resume_position = Some(f64::from(w.get_position())); }
            let recommendation_limit_changed =
                s.recommendation_count != st.settings.recommendation_count;
            let cache_changed = s.cache_enabled != st.settings.cache_enabled;
            let audio_changed = s.audio_cache_enabled != st.settings.audio_cache_enabled
                || s.audio_cache_limit_mib != st.settings.audio_cache_limit_mib;
            st.settings = s.clone();
            st.related_for = None;
            if !s.remember {
                st.history = Default::default();
                if let Err(error) = history::clear(&p) {
                    w.set_status(format!("Could not erase saved history: {error:#}").into());
                    st.history_error = Some(format!("{error:#}"));
                } else {
                    st.history_error = None;
                }
            }
            drop(st);
            if cache_changed {
                p.cache_policy.set(s.cache_enabled, p.cache.clone());
            }
            resolver_cache
                .lock()
                .unwrap()
                .configure(s.stream_cache_enabled);
            if audio_changed {
                w.set_cache_status(
                    if s.audio_cache_enabled {
                        p.audio.summary()
                    } else {
                        "Offline music saving is off.".into()
                    }
                    .into(),
                );
            }
            if audio_changed
                && let Err(e) = p
                    .audio
                    .configure(s.audio_cache_enabled, s.audio_cache_limit_mib)
            {
                p.audio.notice(format!("Music cache: {e}"));
            }
            match result {
                Ok(()) => {
                    w.set_settings_open(false);
                    w.set_status("Settings saved.".into());
                    if restart {
                        w.invoke_play();
                    }
                    if recommendation_limit_changed && !w.get_music_layout() {
                        match w.get_browse_tab() {
                            0 => w.invoke_home(),
                            1 => w.invoke_related(),
                            _ => {}
                        }
                    }
                }
                Err(e) => w.set_status(format!("Could not save settings: {e}").into()),
            }
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    w.on_toggle_music_cache(move || {
        if let Some(w) = weak.upgrade() {
            w.invoke_save_settings();
            let saved = st.lock().unwrap().settings.audio_cache_enabled;
            if saved != w.get_audio_cache_enabled() {
                w.set_audio_cache_enabled(saved);
                w.invoke_notify("Could not save offline music preference".into());
            } else {
                w.invoke_downloads();
            }
        }
    });
    let weak = w.as_weak();
    w.on_toggle_fullscreen(move || {
        if let Some(w) = weak.upgrade() {
            let enabled = !w.window().is_fullscreen();
            w.set_fullscreen_mode(enabled);
            w.window().set_fullscreen(enabled);
        }
    });
    let pages_guard = pages::install(w, state.clone(), paths.clone());
    let anime_guard = sources::install(w, state.clone(), player_tx.clone(), player_generation.clone(), generation.clone(), smoke);
    #[cfg(target_os = "linux")]
    let restored_renderer_session = linux_renderer::restore(w, &state, &paths);
    #[cfg(not(target_os = "linux"))]
    let restored_renderer_session = false;
    if !smoke && !restored_renderer_session {
        if w.get_music_layout() {
            w.invoke_home();
        } else {
            w.global::<crate::BrowsePages>().invoke_home();
        }
    }
    Runtime {
        player: player_tx,
        generation: player_generation,
        _controls_timer: controls_timer,
        #[cfg(target_os = "windows")]
        _native_timer: native_timer,
        downloads,
        _captions: caption_guard,
        _skin: skin_guard,
        _pages: pages_guard,
        _anime: anime_guard,
        worker: Some(worker),
    }
}
fn minimum_views_choice(views: u64) -> i32 {
    (views.clamp(10_000, 50_000).div_ceil(10_000) - 1) as i32
}

fn committed_preferences(settings: &mut Settings, active: &Settings) {
    settings.audio_effects = active.audio_effects.clone();
    settings.interpolation_mode=active.interpolation_mode;
    settings.interpolation_enabled=active.interpolation_enabled;
    settings.interpolation_auto_fallback=active.interpolation_auto_fallback;
    settings.cache_enabled = active.cache_enabled;
    settings.stream_cache_enabled = active.stream_cache_enabled;
    settings.audio_cache_enabled = active.audio_cache_enabled;
    settings.audio_cache_limit_mib = active.audio_cache_limit_mib;
    settings.recommendation_mode = active.recommendation_mode;
    settings.recommendation_count = active.recommendation_count;
    settings.minimum_recommendation_views = active.minimum_recommendation_views;
    settings.auto_related = active.auto_related;
    settings.use_history = active.use_history;
    settings.hide_watched = active.hide_watched;
    settings.hide_alternate_versions = active.hide_alternate_versions;
    settings.blocked_channels = active.blocked_channels.clone();
    settings.blocked_keywords = active.blocked_keywords.clone();
}
fn queue(
    w: &SetupWindow,
    tx: &mpsc::Sender<BrowseRequest>,
    generation: &AtomicU64,
    task: Browse,
    state: &Arc<Mutex<State>>,
) {
    queue_refresh(w, tx, generation, task, state, Vec::new(), Vec::new());
}
fn queue_refresh(
    w: &SetupWindow,
    tx: &mpsc::Sender<BrowseRequest>,
    generation: &AtomicU64,
    task: Browse,
    state: &Arc<Mutex<State>>,
    avoid: Vec<String>,
    defer: Vec<String>,
) {
    let n = generation.fetch_add(1, Ordering::Relaxed) + 1;
    {
        let mut st = state.lock().unwrap();
        st.music_artist=match &task { Browse::Artist{uri,title,sort}=>Some((uri.clone(),title.clone(),*sort)), _=>None };
        st.music_discover_pending = false;
        st.music_follow_discover = matches!(&task, Browse::MusicDiscovery(_));
        st.music_search_anchor = match &task {
            Browse::MusicDiscovery(seed) => Some(seed.id.clone()),
            _ => None,
        };
    }
    w.set_results_scroll(0.);
    w.set_browse_tab(match &task {
        Browse::Home => 0,
        Browse::MusicDiscovery(_) => 1,
        Browse::FromLikes => 5,
        Browse::Artist { .. } => 6,
        Browse::Related(_) => 1,
        Browse::History => 2,
        Browse::Downloads => 4,
        Browse::Restore(c) if c.title == "Cached music" => 4,
        Browse::Restore(c) if c.history => 2,
        Browse::Restore(c) if c.title == "Recommended next" || c.title == "Similar" => 1,
        Browse::Restore(c) if c.title == "From likes" => 5,
        Browse::Restore(_) => 0,
        _ => -1,
    });
    w.set_history_view(false);
    w.set_playlist_id("".into());
    w.set_collection_title(if let Browse::Artist { title, .. } = &task { title.clone() } else { "Discover & recently played".into() }.into());
    w.set_busy(true);
    w.set_browse_error(false);
    w.set_search_notice(false);
    if matches!(&task, Browse::Search(_) | Browse::Artist { .. }) {
        w.set_results_scroll(0.);
        browse_view::publish(&w, Vec::<VideoCard>::new());
        state.lock().unwrap().videos.clear();
    }
    w.set_status(
        if matches!(&task, Browse::Related(_) | Browse::MusicDiscovery(_)) {
            "Loading recommendations…"
        } else {
            "Loading videos…"
        }
        .into(),
    );
    let mut config = settings(w);
    // Browsing follows the active UI mode, independent of an audio-only video format.
    config.music = w.get_music_layout();
    committed_preferences(&mut config, &state.lock().unwrap().settings);
    config.recommendation_avoid = avoid;
    config.recommendation_defer = defer;
    config.seed_recommendations = config.music
        && matches!(
            &task,
            Browse::MusicDiscovery(_) | Browse::Related(_) | Browse::FromLikes
        );
    let _ = tx.send(BrowseRequest {
        task,
        settings: config,
        generation: n,
    });
}
fn select(w: &SetupWindow, v: &Video, index: i32) {
    w.set_page_scroll(0.);
    w.set_quality_compact_labels(compact_labels(&default_format_options()));
    w.set_quality_labels(ModelRc::new(VecModel::from(
        default_format_options()
            .iter()
            .map(|o| slint::SharedString::from(&o.label))
            .collect::<Vec<_>>(),
    )));
    w.set_quality_choice(if w.get_music_only() {
        5
    } else {
        w.get_quality_index()
    });
    if w.get_selected_id().as_str() != v.id {
        w.set_description_expanded(false);
        w.set_description("".into());
        w.set_selected_channel_url("".into());
        w.set_subscriber_text("".into());
        w.set_rating_text("".into());
        w.set_video_stats("Loading details…".into());
        w.invoke_reset_captions();
        w.set_caption_labels(ModelRc::new(VecModel::from(vec![
            slint::SharedString::from("Captions off"),
        ])));
    }
    w.set_selected_id(v.id.clone().into());
    w.set_selected_title(v.title.clone().into());
    w.set_selected_channel(v.channel.clone().into());
    w.set_has_selection(true);
    w.set_selection_playable(v.playable(w.get_music_layout()));

    w.set_selected_image(if w.get_music_layout() {
        w.get_videos()
            .row_data(index as usize)
            .map(|c| c.thumbnail)
            .unwrap_or_default()
    } else {
        Default::default()
    });
}
fn browse_worker(
    rx: mpsc::Receiver<BrowseRequest>,
    weak: slint::Weak<SetupWindow>,
    state: Arc<Mutex<State>>,
    paths: Paths,
    generation: Arc<AtomicU64>,
    stream_cache: Arc<Mutex<stream_cache::StreamCache>>,
) {
    while let Ok(mut request) = rx.recv() {
        for newer in rx.try_iter() {
            request = newer;
        }
        let n = request.generation;
        let is_history = matches!(&request.task, Browse::History)
            || matches!(&request.task,Browse::Restore(c) if c.history);
        let restored = if let Browse::Restore(c) = &request.task {
            Some(c.clone())
        } else {
            None
        };
        if !wait_for_playback(Some((&generation, n))) {
            continue;
        }
        let direct_link = matches!(&request.task, Browse::Search(q) if crate::playlists::youtube_id_from_uri(q).is_some());
        let browse_started = Instant::now();
        let cache_epoch = stream_cache.lock().unwrap().epoch();
        let mut prefetched = std::collections::HashSet::new();
        let mut metadata_source = "yt-dlp";
        let s = &request.settings;
        let history = state.lock().unwrap().history[usize::from(s.music)].clone();
        let mut note = String::new();
        let mut search_notice = false;
        let result: Result<Vec<Video>> = (|| {
            if matches!(&request.task, Browse::Downloads)
                || paths.network.offline()
                    && !matches!(&request.task, Browse::History | Browse::Restore(_))
            {
                if !s.music && !matches!(&request.task, Browse::Downloads) {
                    bail!(
                        "Offline — video and online search need a connection. Switch to Music to play downloaded tracks."
                    );
                }
                metadata_source = "downloaded music";
                note = if paths.network.offline() {
                    "Offline · playing cached tracks"
                } else {
                    "Cached music"
                }
                .into();
                let mut tracks = paths.audio.list();
                if let Browse::Search(q) = &request.task {
                    let id = crate::playlists::youtube_id_from_uri(q);
                    let q = q.to_lowercase();
                    tracks.retain(|v| {
                        format!("{} {}", v.title, v.channel)
                            .to_lowercase()
                            .contains(&q)
                            || id.as_ref().is_some_and(|id| *id == v.id)
                    });
                }
                return Ok(tracks);
            }
            let target = match &request.task {
                Browse::Downloads => unreachable!(),
                Browse::Related(video) | Browse::MusicDiscovery(video) => {
                    if s.recommendation_mode == 0 {
                        let suggestions = recommendations::filtered_suggestions(
                            &video.id,
                            s,
                            &history,
                            video,
                            recommendations::count(s),
                            &generation,
                            n,
                        );
                        match suggestions {
                            Ok(page) => {
                                let mut contextual_seed = video.clone();
                                if page.classified {
                                    contextual_seed.music = page.music;
                                }
                                metadata_source = "YouTube watch suggestions";
                                note = format!("YouTube suggestions for {}", video.title);
                                return Ok(recommendations::filter(
                                    page.videos,
                                    s,
                                    &history,
                                    Some(&contextual_seed),
                                ));
                            }
                            Err(error) if recommendations::request_blocked(&error) => return Err(error),
                            Err(_) => {
                                note = "YouTube suggestions unavailable · topic/history fallback"
                                    .into()
                            }
                        }
                    } else {
                        note = "Related topics from your recommendation preferences".into();
                    }
                    recommendations::fallback_query(s, &history, Some(video), n as usize)
                }
                Browse::History => {
                    metadata_source = "local history";
                    note = format!("Local {} history", if s.music { "music" } else { "video" });
                    return Ok(history
                        .into_iter()
                        .map(|mut v| {
                            let (reason, _) =
                                availability::metadata(&serde_json::json!({"title":v.title}));
                            if !reason.is_empty() {
                                v.unavailable = reason;
                            }
                            v
                        })
                        .collect());
                }
                Browse::Restore(c) => {
                    if c.history {
                        note = format!("Local {} history", if s.music { "music" } else { "video" });
                        return Ok(history.clone());
                    }
                    note = c.title.clone();
                    return Ok(c.videos.clone());
                }
                Browse::Artist { uri, title, sort } => {
                    metadata_source = "artist uploads";
                    let (name,videos)=pages::music_artist_videos(&paths,s,uri,*sort,&generation,n)?;
                    note=if name.is_empty(){title.clone()}else{name};
                    return Ok(videos);
                }
                Browse::Search(q) => {
                    note = format!("Results for {q}");
                    search_target(q)?
                }
                Browse::Home | Browse::FromLikes => {
                    let from_likes = matches!(&request.task, Browse::FromLikes);
                    if from_likes || (s.use_history && s.recommendation_mode != 2) {
                        let seeds = if from_likes {
                            let library = crate::playlists::Store {
                                path: paths.data.join("library.json"),
                            }
                            .load()?;
                            let mut st = state.lock().unwrap();
                            let seeds = music::liked_seeds(&library, st.likes_seed_cursor);
                            st.likes_seed_cursor = st.likes_seed_cursor.wrapping_add(1);
                            seeds
                        } else {
                            recommendations::history_seeds(&history, s.music)
                        };
                        if from_likes && seeds.is_empty() {
                            note = "Like some songs to build suggestions from them.".into();
                            return Ok(Vec::new());
                        }
                        if !seeds.is_empty() {
                            metadata_source = if from_likes {
                                "liked music suggestions"
                            } else {
                                "recent history suggestions"
                            };
                            note = if from_likes {
                                "From likes".into()
                            } else {
                                format!(
                                    "For you · last {} {} history entries",
                                    seeds.len(),
                                    if s.music { "music" } else { "video" }
                                )
                            };
                            return home::load(
                                &weak,
                                &state,
                                &paths,
                                s,
                                &history,
                                &generation,
                                n,
                                seeds,
                                &mut prefetched,
                            );
                        }
                        if s.music || s.recommendation_mode == 1 || !history.is_empty() {
                            note=if s.music{"No music history identified yet. Search and play music to build your For you feed."}else{"No usable video history yet. Search and play videos to build your For you feed."}.into();
                            return Ok(Vec::new());
                        }
                    }
                    note = "Search and play videos to build your For you feed.".into();
                    return Ok(Vec::new());
                }
            };
            let effective = s;
            if let Some(id) = crate::playlists::youtube_id_from_uri(&target)
                && let Ok(video) = preview_metadata(&id)
            {
                metadata_source = "YouTube preview";
                return Ok(vec![video]);
            }
            let v = extract(&paths, effective, &target, true, Some((&generation, n)))?;
            if effective.stream_cache_enabled
                && v["formats"].is_array()
                && let Some(video) = video_from(&v)
            {
                let mut cache = stream_cache.lock().unwrap();
                if cache.enabled() && cache.epoch() == cache_epoch {
                    if crate::playlists::youtube_id_from_uri(&target).is_some() {
                        cache.insert(&video.id, effective, &v, Instant::now());
                    } else {
                        cache.seed(&video.id, effective, &v);
                    }
                }
            }
            let videos = videos_from(&v);
            if videos.is_empty() {
                if let Browse::Search(query) = &request.task
                    && target.starts_with("ytsearch30:")
                {
                    let reason = search::empty_reason(query)
                        .unwrap_or_else(|| "YouTube returned no results for this search.".into());
                    let selected = state.lock().unwrap().selected.clone();
                    let matches = search::local_matches(query, &history, selected.as_ref());
                    if !matches.is_empty() {
                        metadata_source = "local search fallback";
                        search_notice = true;
                        note = format!("{reason} Showing local matches.");
                        return Ok(matches);
                    }
                    bail!("{reason}");
                }
                bail!("YouTube returned no videos for this request.");
            }
            if matches!(
                &request.task,
                Browse::Home | Browse::Related(_) | Browse::MusicDiscovery(_)
            ) {
                let current = match &request.task {
                    Browse::Related(v) | Browse::MusicDiscovery(v) => Some(v),
                    _ => None,
                };
                return Ok(recommendations::filter(videos, s, &history, current));
            }
            Ok(videos)
        })();
        let result = result.map(|mut videos| {
            if s.music
                && !direct_link
                && !paths.network.offline()
                && matches!(
                    &request.task,
                    Browse::Home
                        | Browse::Related(_)
                        | Browse::MusicDiscovery(_)
                        | Browse::Search(_)
                )
            {
                videos.retain(recommendations::song_duration);
                if videos.is_empty() && matches!(&request.task, Browse::Search(_)) {
                    search_notice = true;
                    note =
                        "No songs between 1 and 10 minutes found. Try a different search.".into();
                }
            }
            if videos.is_empty() && !s.recommendation_defer.is_empty() {
                note = "No fresh matching suggestions right now. Your current song keeps playing."
                    .into();
            }
            if let Browse::MusicDiscovery(seed) = &request.task {
                videos = music::discovery_rows(Some(seed), videos, recommendations::count(s));
            }
            videos
        });
        if generation.load(Ordering::Relaxed) != n {
            continue;
        }
        match result {
            Err(e) => {
                paths.network.retry();
                let message = e.to_string();
                ui_generation(&weak, &generation, n, move |w| {
                    w.set_busy(false);
                    w.set_browse_error(true);
                    w.set_status(message.into());
                });
            }
            Ok(videos) => {
                *BROWSE_STATS.lock().unwrap() = Some(BrowseStats {
                    source: metadata_source.into(),
                    metadata_ms: browse_started.elapsed().as_secs_f64() * 1000.,
                    first_thumbnail_ms: None,
                    all_thumbnails_ms: None,
                    thumbnail_count: prefetched.len(),
                });
                let cards = videos.clone();
                let empty = videos.is_empty();
                let loading_note = note.clone();
                let selected_state = state.clone();
                let card_paths = paths.clone();
                let downloaded_view = metadata_source == "downloaded music";
                let history_home = metadata_source == "recent history suggestions";
                let music_home = s.music;
                let search_discovery = matches!(&request.task, Browse::MusicDiscovery(_));
                ui_generation(&weak, &generation, n, move |w| {
                    w.set_history_view(is_history);
                    w.set_search_notice(search_notice);
                    selected_state.lock().unwrap().videos = cards.clone();
                    if music_home {
                        music::sync_discovery_queue(&w, &selected_state, &cards, true);
                    }
                    w.set_playlist_id(
                        restored
                            .as_ref()
                            .map(|c| c.playlist.clone())
                            .unwrap_or_default()
                            .into(),
                    );
                    w.set_collection_title(
                        if metadata_source == "artist uploads" {
                            &loading_note
                        } else if search_discovery {
                            "Similar"
                        } else if loading_note == "From likes"
                            || loading_note.starts_with("Like some songs")
                        {
                            "From likes"
                        } else if let Some(c) = restored.as_ref() {
                            c.title.as_str()
                        } else if loading_note.starts_with("YouTube suggestions")
                            || loading_note.starts_with("Related topics")
                            || loading_note.starts_with("YouTube suggestions unavailable")
                        {
                            "Recommended next"
                        } else if history_home {
                            if music_home {
                                "For you · recent music"
                            } else {
                                "For you · recent history"
                            }
                        } else if downloaded_view {
                            "Cached music"
                        } else {
                            "Discover & recently played"
                        }
                        .into(),
                    );
                    let previous = w.get_videos();
                    browse_view::publish(
                        &w,
                        cards
                            .iter()
                            .map(|v| {
                                let mut card = cached_card(&card_paths, v);
                                if let Some(old) =
                                    previous.iter().find(|row| row.source_id.as_str() == v.id)
                                {
                                    card.thumbnail = old.thumbnail;
                                }
                                card
                            })
                            .collect::<Vec<_>>(),
                    );
                    // Text searches keep current playback; submitting a direct video link
                    // explicitly selects and plays that video in the current mode.
                    if direct_link
                        && restored.is_none()
                        && let Some(v) = cards.first()
                    {
                        {
                            let mut st = selected_state.lock().unwrap();
                            st.removed_history[usize::from(w.get_music_layout())].remove(&v.id);
                            st.related_for = None;
                            if !w.get_music_layout() {
                                play_queue::current(&mut st, v.clone());
                            } else {
                                st.selected = Some(v.clone());
                                if !st.queue_is_playlist {
                                    st.queue.clear();
                                }
                            }
                        }
                        select(&w, v, 0);
                        w.invoke_play();
                    }
                    play_queue::publish(&w, &selected_state);
                    w.set_status(if empty {
                        if loading_note.starts_with("Like some songs") || loading_note.starts_with("No music history") || loading_note.starts_with("No usable video history") {loading_note.as_str()}else if is_history {"No watch history yet."}else if downloaded_view {"No completed music downloads here yet. Uncached tracks need a connection."}else{"No recommendations match your filters. Adjust them in Settings → Recommendations."}.into()
                    } else {
                        format!("{loading_note} · Loading thumbnails…").into()
                    });
                });
                let next = std::sync::atomic::AtomicUsize::new(0);
                thread::scope(|scope| {
                    for _ in 0..2 {
                        scope.spawn(|| {
                            loop {
                                if generation.load(Ordering::Relaxed) != n {
                                    break;
                                }
                                let index = next.fetch_add(1, Ordering::Relaxed);
                                let Some(video) = videos.get(index) else {
                                    break;
                                };
                                if prefetched.contains(&video.id) {
                                    continue;
                                }
                                if !wait_for_playback(Some((&generation, n))) {
                                    return;
                                }
                                if let Ok((pixels, width, height)) = thumbnail(&paths, video) {
                                    if let Some(stats) = BROWSE_STATS.lock().unwrap().as_mut() {
                                        stats.thumbnail_count += 1;
                                        stats.first_thumbnail_ms.get_or_insert(
                                            browse_started.elapsed().as_secs_f64() * 1000.,
                                        );
                                    }
                                    let gen_id = generation.clone();
                                    let st = state.clone();
                                    let id = video.id.clone();
                                    ui_generation(&weak, &generation, n, move |w| {
                                        if gen_id.load(Ordering::Relaxed) != n {
                                            return;
                                        }
                                        let buffer =
                                slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                    &pixels, width, height,
                                );
                                        let image = slint::Image::from_rgba8(buffer);
                                        let model = w.get_videos();
                                        let row_index = model
                                            .row_data(index)
                                            .filter(|row| row.source_id.as_str() == id)
                                            .map(|_| index)
                                            .or_else(|| {
                                                model
                                                    .iter()
                                                    .position(|row| row.source_id.as_str() == id)
                                            });
                                        if let Some(row_index) = row_index
                                            && let Some(mut row) = model.row_data(row_index)
                                        {
                                            row.thumbnail = image.clone();
                                            model.set_row_data(row_index, row);
                                        }
                                        if w.get_music_layout()
                                            && st
                                                .lock()
                                                .unwrap()
                                                .selected
                                                .as_ref()
                                                .is_some_and(|v| v.id == id)
                                        {
                                            if w.get_selected_image().size().width < 480 {
                                                w.set_selected_image(image);
                                            }
                                        }
                                    });
                                }
                            }
                        });
                    }
                });
                if generation.load(Ordering::Relaxed) == n {
                    if let Some(stats) = BROWSE_STATS.lock().unwrap().as_mut() {
                        stats.all_thumbnails_ms =
                            Some(browse_started.elapsed().as_secs_f64() * 1000.);
                    }
                    ui_generation(&weak, &generation, n, move |w| {
                        w.set_busy(false);
                        if !empty {
                            w.set_status(
                                format!(
                                    "{note} · {} {}",
                                    videos.len(),
                                    match (w.get_music_layout(), videos.len() == 1) {
                                        (true, true) => "track",
                                        (true, false) => "tracks",
                                        (false, true) => "video",
                                        (false, false) => "videos",
                                    }
                                )
                                .into(),
                            );
                        }
                    });
                }
                if paths.cache_policy.enabled() {
                    trim_cache(&paths.cache);
                }
            }
        }
    }
}
fn metadata_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .max_idle_connections(2)
            .max_idle_connections_per_host(2)
            .max_idle_age(Duration::from_secs(15))
            .build()
            .into()
    })
}
fn preview_metadata(id: &str) -> Result<Video> {
    let url = format!(
        "https://www.youtube.com/oembed?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D{id}&format=json"
    );
    let mut response = metadata_agent().get(&url).call()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_vec()?;
    let v: Value = serde_json::from_slice(&bytes)?;
    Ok(Video {
        id: id.into(),
        title: v["title"]
            .as_str()
            .context("No title in preview")?
            .chars()
            .take(4096)
            .collect(),
        channel: v["author_name"]
            .as_str()
            .unwrap_or("YouTube")
            .chars()
            .take(4096)
            .collect(),
        duration: 0.,
        views: None,
        ..Video::default()
    })
}
struct ImagePermit;
static IMAGE_JOBS: Mutex<u8> = Mutex::new(0);
static IMAGE_READY: std::sync::Condvar = std::sync::Condvar::new();
impl ImagePermit {
    fn acquire() -> Self {
        let mut jobs = IMAGE_JOBS.lock().unwrap();
        while *jobs >= 2 {
            jobs = IMAGE_READY.wait(jobs).unwrap();
        }
        *jobs += 1;
        Self
    }
}
impl Drop for ImagePermit {
    fn drop(&mut self) {
        let mut jobs = IMAGE_JOBS.lock().unwrap();
        *jobs -= 1;
        IMAGE_READY.notify_one();
    }
}
fn decode_thumbnail(bytes: &[u8]) -> Result<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(640);
    limits.max_image_height = Some(480);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?)
}
fn thumbnail(paths: &Paths, video: &Video) -> Result<(Vec<u8>, u32, u32)> {
    thumbnail_sized(paths, video, 160, 90)
}
fn thumbnail_sized(
    paths: &Paths,
    video: &Video,
    width: u32,
    height: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    let _permit = ImagePermit::acquire();
    let medium = width > 320;
    let path = paths.cache.join(if medium {
        format!("{}-medium.jpg", video.id)
    } else {
        format!("{}.jpg", video.id)
    });
    let epoch = paths.cache_policy.epoch();
    let cached = paths.cache_policy.read(&path).or_else(|| {
        (!medium)
            .then(|| {
                paths
                    .cache_policy
                    .read(&paths.cache.join(format!("{}-medium.jpg", video.id)))
            })
            .flatten()
    });
    let decoded = if let Some(bytes) = cached.and_then(|bytes| decode_thumbnail(&bytes).ok()) {
        bytes
    } else {
        anyhow::ensure!(!paths.network.offline(), "Offline — thumbnail unavailable");
        let mut response = metadata_agent()
            .get(&format!(
                "https://i.ytimg.com/vi/{}/{}",
                video.id,
                if medium {
                    "hqdefault.jpg"
                } else {
                    "mqdefault.jpg"
                }
            ))
            .call()?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_vec()?;
        let image = decode_thumbnail(&bytes)?;
        paths.cache_policy.write(&path, &bytes, epoch);
        image
    };
    let image = decoded.thumbnail(width, height).to_rgba8();
    let (w, h) = image.dimensions();
    Ok((image.into_raw(), w, h))
}
fn trim_cache(cache: &Path) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            e.metadata()
                .ok()
                .map(|m| (e.path(), m.len(), m.modified().ok()))
        })
        .collect();
    let mut size: u64 = files.iter().map(|(_, n, _)| n).sum();
    files.sort_by_key(|(_, _, time)| *time);
    for (path, n, _) in files {
        if size <= 32 * 1024 * 1024 {
            break;
        }
        if fs::remove_file(path).is_ok() {
            size = size.saturating_sub(n);
        }
    }
}

fn cacheable_audio(info: &Value) -> bool {
    info["vcodec"].as_str() == Some("none")
        && matches!(info["protocol"].as_str(), Some("http" | "https"))
        && matches!(
            info["ext"].as_str(),
            Some("webm" | "m4a" | "mp4" | "opus" | "ogg" | "mp3" | "aac" | "flac" | "wav")
        )
}
fn player_worker(
    rx: mpsc::Receiver<PlayerCommand>,
    weak: slint::Weak<SetupWindow>,
    state: Arc<Mutex<State>>,
    paths: Paths,
    generation: Arc<AtomicU64>,
    mut engine: Result<crate::video::Engine>,
    local: Option<String>,
    stream_cache: Arc<Mutex<stream_cache::StreamCache>>,
    seek_revision: Arc<AtomicU64>,
) {
    let mut music_retry = music_retry::MusicRetry::default();
    let mut applied_seek_revision = seek_revision.load(Ordering::Acquire);
    let mut audio_lease: Option<audio_cache::Lease> = None;
    let mut last_request: Option<(Video, Settings, u64)> = None;
    let mut known_duration: Option<f64> = None;
    let mut preroll_started: Option<Instant> = None;
    let mut preroll_budget=Duration::from_secs(20);
    let mut cached_request = false;
    let mut public_stream = false;
    let mut buffering = crate::video::playback_state::Buffering::default();
    let mut loading = crate::video::playback_state::Loading::default();
    let mut playback_progress = crate::video::playback_state::Progress::default();
    let mut user_paused = false;
    let mut resolved_quality = None;
    let mut native_fallback_epoch = None;
    let mut retry_position: Option<(f64, bool)> = None;
    let mut resume_checkpoint = 0.;
    let mut startup_started: Option<Instant> = None;
    let mut last_acceleration = Instant::now();
    let mut video_idle_since: Option<Instant> = None;
    let mut loop_video = false;
    let mut loop_music = false;
    let mut playing = false;
    let mut music = false;
    let mut active: Option<String> = None;
    let mut pending: Option<(Video, Settings, f64, bool)> = None;
    let mut queued = None;
    #[cfg(target_os = "windows")]
    let mut last_health = Instant::now();
    loop {
        let mut command = if queued.is_some() {
            queued.take()
        } else {
            match rx.recv_timeout(if engine.as_ref().is_ok_and(|p| p.seeking()) {
                Duration::from_millis(20)
            } else if playing && music && pending.is_none() {
                Duration::from_secs(1)
            } else if pending.is_some() {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(200)
            }) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(_) => None,
            }
        };
        if command.is_none()
            && music_retry.take_due(generation.load(Ordering::Acquire), Instant::now())
        {
            if let Some((video, settings, n)) = last_request.clone() {
                crate::diagnostics::record(
                    "music_retry_started",
                    serde_json::json!({"video_id":video.id,"epoch":n}),
                );
                command = Some(PlayerCommand::Play(video, settings, n));
            }
        }
        if matches!(
            command,
            Some(PlayerCommand::Seek(..) | PlayerCommand::SeekState(..))
        ) {
            while let Ok(next) = rx.try_recv() {
                if matches!(next, PlayerCommand::Seek(..) | PlayerCommand::SeekState(..)) {
                    command = Some(next);
                } else {
                    queued = Some(next);
                    break;
                }
            }
        }
        if let Some(PlayerCommand::Shutdown) = command {
            break;
        }
        if let Some(command) = command {
            let loading_epoch = if let PlayerCommand::Play(_, _, n) = &command {
                Some(*n)
            } else {
                None
            };
            let loading = loading_epoch.is_some();
            let result: Result<()> = (|| {
                let p = engine.as_mut().map_err(|e| anyhow::anyhow!("{e}"))?;
                match command {
                    PlayerCommand::Play(video, s, n) => {
                        ui_generation(&weak, &generation, n, |w| {
                            crate::timeline::clear(&w);
                            w.set_buffered_estimated(false);
                        });
                        if generation.load(Ordering::Relaxed) != n {
                            return Ok(());
                        }
                        preroll_budget=Duration::from_secs(20);
                        music_retry.begin(n);
                        crate::diagnostics::record(
                            "play_request",
                            serde_json::json!({"video_id":video.id,"music":s.music,"hardware":s.hardware,"quality":s.quality}),
                        );
                        applied_seek_revision = seek_revision.load(Ordering::Acquire);
                        let startup = Instant::now();
                        startup_started = Some(startup);
                        last_request = Some((video.clone(), s.clone(), n));
                        cached_request = false;
                        public_stream = false;
                        buffering.clear();
                        playback_progress.clear();
                        let resume = if s.resume_position > 0. {
                            s.resume_position
                        } else if active.as_ref() == Some(&video.id) {
                            resume_checkpoint
                        } else {
                            0.
                        };
                        let keep_paused =
                            playing && active.as_ref() == Some(&video.id) && user_paused;
                        let (resume, keep_paused) =
                            retry_position.take().unwrap_or((resume, keep_paused));
                        user_paused = keep_paused;
                        resume_checkpoint = resume;
                        crate::diagnostics::record(
                            "playback_start_target",
                            serde_json::json!({"epoch":n,"video_id":video.id,"position":resume,"paused":keep_paused}),
                        );
                        p.stop()?;
                        audio_lease = None;
                        playing = false;
                        pending = None;
                        music = s.music;
                        let resolve_started = Instant::now();
                        let cache_epoch = stream_cache.lock().unwrap().epoch();
                        let mut resolver = "local";
                        ui_generation(&weak, &generation, n, |w| {
                            w.set_video_visible(false);
                            w.set_video_frame(slint::Image::default());
                        });
                        let saved = if s.music
                            && local.as_ref().is_none_or(|p| {
                                p.starts_with("http://") || p.starts_with("https://")
                            }) {
                            paths.audio.lookup(&video.id)
                        } else {
                            None
                        };
                        anyhow::ensure!(
                            local.as_ref().is_some_and(
                                |p| !p.starts_with("http://") && !p.starts_with("https://")
                            ) || saved.is_some()
                                || !paths.network.offline(),
                            if s.music {
                                "Offline — this track has no completed download. Choose Cached music."
                            } else {
                                "Offline — video needs an Internet connection. Cached music is still available."
                            }
                        );
                        let info = if video.id.starts_with("external:") {
                            resolver = "native source resolver";
                            let st = state.lock().unwrap();
                            let playback = st.anime_playback.as_ref()
                                .filter(|v| v.video.id == video.id)
                                .context("Source session expired; select the title again")?;
                            playback.info.clone()
                        } else if let Some(saved) = saved {
                            resolver = "completed audio cache";
                            let uri = gst::glib::filename_to_uri(&saved.path, None)?;
                            let info = serde_json::json!({"id":saved.video.id,"title":saved.video.title,"uploader":saved.video.channel,"duration":saved.video.duration,"url":uri.as_str(),"vcodec":"none"});
                            audio_lease = Some(saved);
                            info
                        } else if let Some(path) = &local {
                            let uri = if path.starts_with("http://") || path.starts_with("https://")
                            {
                                path.clone()
                            } else {
                                gst::glib::filename_to_uri(path, None)?.to_string()
                            };
                            let audio = std::env::var("MOCHUNE_TEST_AUDIO")
                                .ok()
                                .map(|p| gst::glib::filename_to_uri(p, None))
                                .transpose()?;
                            if let Some(audio) = audio {
                                serde_json::json!({"requested_formats":[{"url":uri.as_str(),"vcodec":"h264"},{"url":audio.as_str(),"vcodec":"none"}]})
                            } else {
                                serde_json::json!({"url":uri.as_str(),"vcodec":if s.music{"none"}else{"h264"},"protocol":"http","ext":"webm"})
                            }
                        } else {
                            let cached = if s.stream_cache_enabled {
                                stream_cache.lock().unwrap().lookup(&video.id, &s)
                            } else {
                                stream_cache.lock().unwrap().clear();
                                None
                            };
                            crate::diagnostics::record(
                                "stream_cache_lookup",
                                serde_json::json!({
                                    "epoch":n,"hit":cached.is_some(),"enabled":s.stream_cache_enabled,
                                    "exact":cached.as_ref().map(|c|c.exact),
                                    "age_ms":cached.as_ref().map(|c|c.resolved_at.elapsed().as_millis())
                                }),
                            );
                            let result = if let Some(entry) = cached {
                                cached_request = true;
                                resolver = if entry.exact {
                                    "memory cache"
                                } else {
                                    "cached format selection"
                                };
                                let result = if entry.exact {
                                    serde_json::from_slice(&entry.bytes)
                                        .map_err(anyhow::Error::from)
                                } else {
                                    extract_using_info(
                                        &paths,
                                        &s,
                                        &video.url(),
                                        false,
                                        Some((&generation, n)),
                                        Some(&entry.bytes),
                                    )
                                };
                                match result {
                                    Ok(info) => {
                                        let mut cache = stream_cache.lock().unwrap();
                                        if cache.enabled() && cache.epoch() == cache_epoch {
                                            cache.insert(&video.id, &s, &info, entry.resolved_at);
                                        }
                                        Ok(info)
                                    }
                                    Err(_) if generation.load(Ordering::Relaxed) == n => {
                                        stream_cache.lock().unwrap().invalidate(&video.id);
                                        cached_request = false;
                                        resolver = "YouTube refresh";
                                        extract(
                                            &paths,
                                            &s,
                                            &video.url(),
                                            false,
                                            Some((&generation, n)),
                                        )
                                    }
                                    Err(e) => Err(e),
                                }
                            } else {
                                resolver = "YouTube";
                                extract(&paths, &s, &video.url(), false, Some((&generation, n)))
                            };
                            match result {
                                Ok(v) => {
                                    if !cached_request && s.stream_cache_enabled {
                                        let mut cache = stream_cache.lock().unwrap();
                                        if cache.enabled() && cache.epoch() == cache_epoch {
                                            cache.insert(&video.id, &s, &v, Instant::now());
                                        }
                                    }
                                    v
                                }
                                Err(_) if generation.load(Ordering::Relaxed) != n => return Ok(()),
                                Err(e) => return Err(e),
                            }
                        };
                        if generation.load(Ordering::Relaxed) != n {
                            return Ok(());
                        }
                        let video = if let Some(resolved) = video_from(&info) {
                            {
                                let mut st = state.lock().unwrap();
                                if st.selected.as_ref().is_some_and(|v| v.id == resolved.id) {
                                    st.selected = Some(resolved.clone());
                                }
                                for row in &mut st.videos {
                                    if row.id == resolved.id {
                                        *row = resolved.clone();
                                    }
                                }
                                for row in &mut st.queue {
                                    if row.id == resolved.id {
                                        *row = resolved.clone();
                                    }
                                }
                            }
                            let refreshed = resolved.clone();
                            let row_state = state.clone();
                            ui_generation(&weak, &generation, n, move |w| {
                                w.set_selected_title(refreshed.title.clone().into());
                                w.set_selected_channel(refreshed.channel.clone().into());
                                let model = w.get_videos();
                                let st = row_state.lock().unwrap();
                                for (index, row) in st.videos.iter().enumerate() {
                                    if row.id == refreshed.id
                                        && let Some(old) = model.row_data(index)
                                    {
                                        let mut card = refreshed.card();
                                        card.thumbnail = old.thumbnail;
                                        card.downloaded = old.downloaded;
                                        model.set_row_data(index, card);
                                    }
                                }
                                drop(st);
                                w.invoke_rebuild_recs();
                            });
                            resolved
                        } else {
                            video
                        };
                        *STARTUP_STATS.lock().unwrap() = Some(StartupStats {
                            resolver: resolver.into(),
                            resolve_ms: resolve_started.elapsed().as_secs_f64() * 1000.,
                            pipeline_load_ms: 0.,
                            first_playing_ms: None,
                        });
                        crate::diagnostics::record(
                            "stream_resolved",
                            serde_json::json!({
                                "epoch":n,"resolver":resolver,"music":s.music,
                                "resolve_ms":resolve_started.elapsed().as_secs_f64()*1000.
                            }),
                        );
                        let details = details::Details::from_info(&info);
                        let tracks = captions::tracks(&info);
                        let caption_control = state.lock().unwrap().captions.clone();
                        let details_id = video.id.clone();
                        ui_generation(&weak, &generation, n, move |w| {
                            details.apply(&w);
                            if w.get_anime_mode() { w.set_subscriber_text("".into()); w.set_rating_text("".into()); }
                            if let Some(control) = caption_control {
                                control.publish(&w, &details_id, tracks);
                            }
                        });
                        if video.id.starts_with("external:2:0:")&&info["external_source"]==true {let index=info["source_index_bytes"].as_u64().unwrap_or(0);if index>2*1024*1024 {preroll_budget=Duration::from_secs((20+index/(1024*1024)*5).min(120));crate::diagnostics::record("source_movie_large_index",serde_json::json!({"index_bytes":index,"startup_budget_seconds":preroll_budget.as_secs()}));}}
                        let (options, selected) = if video.id.starts_with("external:") {
                            (info["source_qualities"].as_array().into_iter().flatten().map(|v|FormatOption{label:v.as_str().unwrap_or("Auto").into(),selector:None,quality:2,music:false}).collect(),info["source_choice"].as_u64().unwrap_or(0) as usize)
                        } else { playback_options(&info, &s) };
                        {
                            let mut st = state.lock().unwrap();
                            st.format_options = options.clone();
                            st.formats_for = Some(video.id.clone());
                        }
                        ui_generation(&weak, &generation, n, move |w| {
                            w.set_quality_compact_labels(compact_labels(&options));
                            w.set_quality_labels(ModelRc::new(VecModel::from(
                                options
                                    .iter()
                                    .map(|o| slint::SharedString::from(&o.label))
                                    .collect::<Vec<_>>(),
                            )));
                            w.set_quality_choice(selected as i32);
                        });
                        public_stream = info["availability"].as_str() == Some("public")
                            && info["age_limit"].as_u64().unwrap_or(0) == 0;
                        let formats = info["requested_formats"].as_array();
                        let primary = formats
                            .and_then(|f| f.iter().find(|v| v["vcodec"].as_str() != Some("none")))
                            .unwrap_or(&info);
                        if !s.music && primary["vcodec"].as_str() != Some("none") {
                            anyhow::ensure!(
                                primary["height"].as_u64().is_none_or(|h| h <= 2160),
                                "This stream exceeds the supported 4K video limit. Pick another quality."
                            );
                        }
                        resolved_quality = video_quality(primary);
                        let uri = primary["url"]
                            .as_str()
                            .context("No playable YouTube stream was returned")?;
                        let audio_format = formats
                            .and_then(|f| f.iter().find(|v| v["vcodec"].as_str() == Some("none")));
                        let audio = audio_format.and_then(|v| v["url"].as_str());
                        let headers = metadata::stream_headers(&info, primary);
                        let audio_headers =
                            metadata::stream_headers(&info, audio_format.unwrap_or(&info));
                        let capture = if music
                            && s.audio_cache_enabled
                            && audio_lease.is_none()
                            && local.as_ref().is_none_or(|p| {
                                p.starts_with("http://") || p.starts_with("https://")
                            })
                            && info["is_live"] != true
                            && info["live_status"].as_str().is_none_or(|v| v == "not_live")
                            && cacheable_audio(primary)
                        {
                            paths
                                .audio
                                .begin(video.clone(), primary["filesize"].as_u64())
                        } else {
                            None
                        };
                        if s.audio_cache_enabled && music && audio_lease.is_none() {
                            let saving = capture.is_some();
                            ui(&weak, move |w| {
                                if !w.get_audio_cache_enabled() {
                                    return;
                                }
                                w.set_cache_status(
                                    if saving {
                                        "Saving compressed audio as it arrives…"
                                    } else {
                                        "Streaming only — this source cannot be safely cached."
                                    }
                                    .into(),
                                )
                            });
                        }
                        known_duration = info["duration"]
                            .as_f64()
                            .filter(|d| d.is_finite() && *d > 0.);
                        let load_started = Instant::now();
                        p.audio_effects = s.audio_effects.clone();
                        p.interpolation_mode=s.interpolation_mode;
                        p.interpolation_auto_fallback=s.interpolation_auto_fallback;
                        if p.audio_effects.convolver && let Err(error) = p.audio_effects.validate() {
                            let notice = format!("Convolution off: {error:#}");
                            p.audio_effects.convolver = false;
                            {
                                let mut state = state.lock().unwrap();
                                state.settings.audio_effects.convolver = false;
                                let _ = paths.save("settings.json", &state.settings);
                            }
                            ui(&weak, move |w| { w.set_audio_convolver_enabled(false); w.set_audio_impulse_status(notice.clone().into()); w.invoke_notify(notice.into()); });
                        }
                        p.load_buffered(
                            uri,
                            audio,
                            music,
                            s.hardware,
                            s.volume as f64,
                            &headers,
                            &audio_headers,
                            capture,
                            (!music && p.manages_start()).then_some((resume, keep_paused)),
                            crate::video::buffering::Hints::from_metadata(
                                &info,
                                primary,
                                audio_format,
                            ),
                        )?;
                        if let Some(stats) = STARTUP_STATS.lock().unwrap().as_mut() {
                            stats.pipeline_load_ms = load_started.elapsed().as_secs_f64() * 1000.;
                        }
                        if let Some((_, retry_settings, _)) = last_request.as_mut() {
                            retry_settings.format_override = info["requested_formats"]
                                .as_array()
                                .map(|fs| {
                                    fs.iter()
                                        .filter_map(|f| f["format_id"].as_str())
                                        .collect::<Vec<_>>()
                                        .join("+")
                                })
                                .filter(|s| !s.is_empty())
                                .or_else(|| info["format_id"].as_str().map(str::to_owned));
                        }
                        preroll_started = Some(Instant::now());
                        active = Some(video.id.clone());
                        pending = Some((video, s, resume, keep_paused));
                        playing = true;
                        let downloaded = audio_lease.is_some();
                        ui_generation(&weak, &generation, n, move |w| {
                            w.set_playing_download(downloaded);
                            w.set_playback_status("Buffering embedded playback…".into());
                            w.set_paused(keep_paused);
                        });
                    }
                    PlayerCommand::Reset => {
                        music_retry.cancel();
                        p.stop()?;
                        p.release_video_process();
                        audio_lease = None;
                        playing = false;
                        pending = None;
                        active = None;
                        last_request = None;
                        known_duration = None;
                        retry_position = None;
                        cached_request = false;
                    }
                    PlayerCommand::RepeatVideo(repeat) => loop_video = repeat,
                    PlayerCommand::RepeatMusic(repeat) => loop_music = repeat,
                    PlayerCommand::SeekState(time, paused, revision) => {
                        let Some(time) = crate::video::playback_state::seek_request(
                            time,
                            p.duration().map(|d| d.seconds_f64()).or(known_duration),
                            last_request.is_some(),
                            !playing && pending.is_none(),
                        ) else {
                            applied_seek_revision = revision;
                            ui(&weak, |w| w.set_resolving(false));
                            return Ok(());
                        };

                        user_paused = paused;
                        resume_checkpoint = time;
                        if let Some((_, _, position, paused)) = pending.as_mut() {
                            *position = time;
                            *paused = user_paused;
                        }
                        applied_seek_revision = revision;
                        if (!music && p.video_process_released())
                            || p.current_state() == gst::State::Null
                        {
                            if let Some((video, settings, n)) = last_request.clone() {
                                retry_position = Some((time, paused));
                                queued = Some(PlayerCommand::Play(video, settings, n));
                            }
                        } else {
                            p.seek_with_state(time, paused)?;
                            playing = true;
                            video_idle_since = None;
                        }
                    }
                    #[cfg(target_os="windows")]
                    PlayerCommand::Interpolation(mode,auto) => {
                        if !music && let Some((video,mut settings,n))=last_request.clone() {
                            settings.interpolation_mode=mode;settings.interpolation_enabled=mode>0;
                            settings.interpolation_auto_fallback=auto;
                            retry_position=Some((p.position().map(|t|t.seconds_f64()).unwrap_or(resume_checkpoint),user_paused));
                            queued=Some(PlayerCommand::Play(video,settings,n));
                        }
                    }
                    PlayerCommand::Connection(online) => {
                        if !online && playing && !music {
                            p.stop()?;
                            p.release_video_process();
                            playing = false;
                            pending = None;
                            ui(&weak, |w| {
                                w.set_video_visible(false);
                                w.set_video_frame(Default::default());
                                w.set_resolving(false);
                                w.set_position(0.);
                                w.set_duration(0.);
                                crate::timeline::clear(&w);
                                w.set_player_error("Offline — video needs a connection. Cached music is still available.".into());
                            });
                        }
                    }
                    PlayerCommand::Pause => {
                        if !playing && active.is_some() {
                            if p.video_process_released() || p.current_state() == gst::State::Null {
                                if let Some((video, settings, n)) = last_request.clone() {
                                    retry_position = Some((0., false));
                                    queued = Some(PlayerCommand::Play(video, settings, n));
                                }
                                return Ok(());
                            }
                            video_idle_since = None;
                            user_paused = false;
                            resume_checkpoint = 0.;
                            p.seek_with_state(0., false)?;
                            playing = true;
                        } else {
                            user_paused = !user_paused;
                        }
                        p.set_paused(user_paused)?;
                    }
                    PlayerCommand::Shutdown => {
                        p.stop()?;
                        return Ok(());
                    }
                    PlayerCommand::Seek(time, revision) => {
                        let Some(time) = crate::video::playback_state::seek_request(
                            time,
                            p.duration().map(|d| d.seconds_f64()).or(known_duration),
                            last_request.is_some(),
                            !playing && pending.is_none(),
                        ) else {
                            applied_seek_revision = revision;
                            ui(&weak, |w| w.set_resolving(false));
                            return Ok(());
                        };

                        resume_checkpoint = time;
                        if let Some((_, _, position, paused)) = pending.as_mut() {
                            *position = time;
                            *paused = user_paused;
                        }
                        applied_seek_revision = revision;
                        if (!music && p.video_process_released())
                            || p.current_state() == gst::State::Null
                        {
                            if let Some((video, settings, n)) = last_request.clone() {
                                retry_position = Some((time, false));
                                queued = Some(PlayerCommand::Play(video, settings, n));
                            }
                        } else {
                            p.seek_with_state(time, user_paused)?;
                            playing = true;
                            video_idle_since = None;
                        }
                    }
                    PlayerCommand::Volume(value) => p.set_volume(value),
                }
                Ok(())
            })();
            if let Err(e) = result {
                crate::diagnostics::record(
                    "playback_setup_error",
                    serde_json::json!({"error":format!("{e:#}")}),
                );
                if loading
                    && engine.is_ok()
                    && music_retry.schedule(
                        &last_request,
                        &generation,
                        &paths,
                        &stream_cache,
                        &weak,
                        &format!("{e:#}"),
                    )
                {
                    retry_position = Some(
                        pending
                            .as_ref()
                            .map(|(_, _, position, paused)| (*position, *paused))
                            .unwrap_or((resume_checkpoint, user_paused)),
                    );
                    if let Ok(p) = engine.as_mut() {
                        let _ = p.stop();
                    }
                    pending = None;
                    playing = false;
                    preroll_started = None;
                    continue;
                }
                if loading
                    && !last_request.as_ref().is_some_and(|(_, s, _)| s.music)
                    && cached_request
                    && let Some((video, s, n)) = last_request.clone()
                    && generation.load(Ordering::Relaxed) == n
                {
                    cached_request = false;
                    stream_cache.lock().unwrap().invalidate(&video.id);
                    retry_position = pending
                        .as_ref()
                        .map(|(_, _, position, paused)| (*position, *paused));
                    queued = Some(PlayerCommand::Play(video, s, n));
                    ui(&weak, |w| {
                        w.set_playback_status("Refreshing playback link…".into())
                    });
                    continue;
                }
                if let Some((v, _, _)) = &last_request {
                    stream_cache.lock().unwrap().invalidate(&v.id);
                }
                if let Some(n) = loading_epoch {
                    finish_playback_priority(n);
                }
                if let Some((video, s, n)) = &last_request {
                    finish_playback_priority(*n);
                    if let Some(reason) = availability::failure(&e.to_string()) {
                        availability::mark(&state, &weak, video.id.clone(), reason, false);
                    } else if s.music
                        && e.to_string()
                            .to_lowercase()
                            .contains("requested format is not available")
                    {
                        availability::mark(
                            &state,
                            &weak,
                            video.id.clone(),
                            "Audio-only stream unavailable".into(),
                            true,
                        );
                    }
                }
                paths.network.retry();
                let message = e.to_string();
                playing = false;
                pending = None;
                if let Ok(p) = engine.as_mut() {
                    let _ = p.stop();
                }
                let epoch = loading_epoch.unwrap_or_else(|| generation.load(Ordering::Relaxed));
                ui_generation(&weak, &generation, epoch, move |w| {
                    w.set_resolving(false);
                    w.set_video_visible(false);
                    w.set_position(0.);
                    w.set_duration(0.);
                    crate::timeline::clear(&w);
                    w.set_playback_active(false);
                    w.set_recommendations_pending(false);
                    w.set_player_error(format!("Playback failed: {message}").into());
                    w.set_playback_status(format!("Playback failed: {message}").into());
                });
            }
        }
        if let Ok(p) = engine.as_mut() {
            while let Some(message) = p.next_message() {
                if last_request
                    .as_ref()
                    .is_some_and(|(_, _, n)| generation.load(Ordering::Relaxed) != *n)
                {
                    continue;
                }
                match message.view() {
                    gst::MessageView::Latency(_) if p.audio_effects.delay || p.audio_effects.convolver => { let _ = p.pipeline.recalculate_latency(); },
                    gst::MessageView::Error(error) => {
                        let origin = error
                            .src()
                            .map(|s| s.path_string().to_string())
                            .unwrap_or_default();
                        let detail = error.error().to_string();
                        // A bounded overwrite-only report excludes signed media URLs and cookies.
                        let report = serde_json::json!({"platform":std::env::consts::OS,"video_id":last_request.as_ref().map(|v|v.0.id.as_str()),"audio_only":music,"element":origin,"error":detail.split_whitespace().map(|v|if v.contains("http://")||v.contains("https://"){"[media URL]"}else{v}).collect::<Vec<_>>().join(" ").chars().take(512).collect::<String>(),"decoder":p.decoder.lock().unwrap().clone()});
                        let mut report = report;
                        report["debug"] = serde_json::json!(
                            error
                                .debug()
                                .unwrap_or_default()
                                .split_whitespace()
                                .map(|v| if v.contains("http://") || v.contains("https://") {
                                    "[media URL]"
                                } else {
                                    v
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                                .chars()
                                .take(4096)
                                .collect::<String>()
                        );
                        report["available_elements"] = serde_json::json!(
                            [
                                "qtdemux",
                                "h264parse",
                                "avdec_h264",
                                "openh264dec",
                                "d3d12h264dec",
                                "d3d12h265dec",
                                "d3d12vp9dec",
                                "d3d12av1dec",
                                "d3d12videosink",
                                "capsfilter",
                                "appsink",
                                "avdec_aac",
                                "opusdec",
                                "vp9dec",
                                "dav1ddec",
                                "glupload",
                                "glcolorconvert",
                                "videoconvert"
                            ]
                            .into_iter()
                            .map(|name| (name, gst::ElementFactory::find(name).is_some()))
                            .collect::<std::collections::BTreeMap<_, _>>()
                        );
                        crate::diagnostics::record("playback_error", report.clone());
                        let _ = paths.save("playback-error.json", &report);
                        if music {
                            if let Some(lease) = audio_lease.as_ref() {
                                paths.audio.invalidate(&lease.video.id);
                            }
                            if music_retry.schedule(
                                &last_request,
                                &generation,
                                &paths,
                                &stream_cache,
                                &weak,
                                &format!("{detail} {}", error.debug().unwrap_or_default()),
                            ) {
                                retry_position = Some(
                                    pending
                                        .as_ref()
                                        .map(|(_, _, position, paused)| (*position, *paused))
                                        .unwrap_or((resume_checkpoint, user_paused)),
                                );
                                let _ = p.stop();
                                pending = None;
                                playing = false;
                                preroll_started = None;
                                break;
                            }
                        }
                        if let Some((video, mut settings, n)) = last_request.clone()
                            && !music
                            && public_stream_retry(
                                public_stream,
                                &settings,
                                &format!("{detail} {}", error.debug().unwrap_or_default()),
                            )
                            && generation.load(Ordering::Relaxed) == n
                        {
                            // Refresh the same selected format; never silently lower quality.
                            settings.public_fallback = true;
                            stream_cache.lock().unwrap().invalidate(&video.id);
                            retry_position = Some(
                                pending
                                    .as_ref()
                                    .map(|(_, _, position, paused)| (*position, *paused))
                                    .unwrap_or_else(|| (resume_checkpoint, user_paused)),
                            );
                            queued = Some(PlayerCommand::Play(video, settings, n));
                            let _ = p.stop();
                            playing = false;
                            pending = None;
                            ui(&weak, move |w| {
                                w.set_resolving(true);
                                w.set_playback_status("Refreshing rejected playback link…".into());
                            });
                            break;
                        }
                        if cached_request
                            && !music
                            && let Some((video, s, n)) = last_request.clone()
                            && generation.load(Ordering::Relaxed) == n
                        {
                            cached_request = false;
                            stream_cache.lock().unwrap().invalidate(&video.id);
                            retry_position = Some(
                                pending
                                    .as_ref()
                                    .map(|(_, _, position, paused)| (*position, *paused))
                                    .unwrap_or_else(|| (resume_checkpoint, user_paused)),
                            );
                            queued = Some(PlayerCommand::Play(video, s, n));
                            let _ = p.stop();
                            playing = false;
                            pending = None;
                            ui(&weak, |w| {
                                w.set_resolving(true);
                                w.set_playback_status("Refreshing playback link…".into());
                            });
                            break;
                        }
                        let retry_decoder = p.decoder.lock().unwrap().clone();
                        if let Some((video, mut settings, n)) = last_request.clone()
                            && native_fallback_epoch != Some(n)
                            && local.is_none()
                            && generation.load(Ordering::Relaxed) == n
                            && let Some(selector) = native_format_retry(
                                &settings,
                                &retry_decoder,
                                resolved_quality,
                                &format!("{detail} {}", error.debug().unwrap_or_default()),
                            )
                        {
                            native_fallback_epoch = Some(n);
                            retry_position = Some((resume_checkpoint, user_paused));
                            settings.format_override = Some(selector);
                            stream_cache.lock().unwrap().invalidate(&video.id);
                            queued = Some(PlayerCommand::Play(video, settings, n));
                            let _ = p.stop();
                            p.release_video_process();
                            playing = false;
                            pending = None;
                            ui_generation(&weak, &generation, n, |w| {
                                w.set_resolving(true);
                                w.set_video_visible(false);
                                w.set_player_error("".into());
                                w.set_playback_status(
                                    "Retrying selected quality with another codec…".into(),
                                );
                            });
                            break;
                        }
                        if let Some((v, _, _)) = &last_request {
                            stream_cache.lock().unwrap().invalidate(&v.id);
                        }
                        if let Some((_, _, n)) = &last_request {
                            finish_playback_priority(*n);
                        }
                        paths.network.retry();
                        if let Some(lease) = audio_lease.as_ref() {
                            paths.audio.invalidate(&lease.video.id);
                        }
                        let text = if paths.network.offline() {
                            "Connection lost. Choose Cached music to keep listening offline.".into()
                        } else {
                            error.error().to_string()
                        };
                        let text = if !music
                            && [
                                "plugin",
                                "decoder",
                                "not-negotiated",
                                "not-linked",
                                "could not decode",
                            ]
                            .iter()
                            .any(|needle| text.to_lowercase().contains(needle))
                        {
                            "No compatible hardware video decoder is available for this quality. Software video decoding is disabled. Try a lower quality or audio only; driver/codec details are in playback-error.json.".to_owned()
                        } else {
                            text
                        };
                        let _ = p.stop();
                        p.release_video_process();
                        pending = None;
                        playing = false;
                        let failed_epoch = last_request
                            .as_ref()
                            .map(|v| v.2)
                            .unwrap_or_else(|| generation.load(Ordering::Acquire));
                        ui_generation(&weak, &generation, failed_epoch, move |w| {
                            w.set_resolving(false);
                            w.set_video_visible(false);
                            w.set_position(0.);
                            w.set_duration(0.);
                            crate::timeline::clear(&w);
                            w.set_playback_active(false);
                            w.set_recommendations_pending(false);
                            w.set_player_error(format!("Playback failed: {text}").into());
                            w.set_playback_status(format!("Playback failed: {text}").into());
                            // A bad queued stream must not strand the remaining explicit queue.
                            // Standalone video failures still stay visible for a manual retry.
                            if !w.get_music_layout()
                                && w.get_queue_rows().row_count() > 1
                                && w.get_queue_rows().row_data(0).is_some_and(|r| r.active)
                            {
                                crate::diagnostics::record(
                                    "video_queue_error_skip",
                                    serde_json::json!({"fullscreen":w.get_fullscreen_mode()}),
                                );
                                w.invoke_notify(
                                    "Skipping a video that could not be played.".into(),
                                );
                                w.invoke_next(true);
                            }
                        });
                    }
                    gst::MessageView::Warning(warning) => {
                        crate::diagnostics::record(
                            "gstreamer_warning",
                            serde_json::json!({"element":warning.src().map(|s|s.path_string().to_string()),"error":warning.error().to_string(),"debug":warning.debug().map(|s|s.to_string())}),
                        );
                    }
                    gst::MessageView::Eos(_) if (!music && loop_video) || (music && loop_music) => {
                        if p.seek_with_state(0., false).is_ok() {
                            user_paused = false;
                            playing = true;
                            let epoch = last_request
                                .as_ref()
                                .map(|(_, _, n)| *n)
                                .unwrap_or_else(|| generation.load(Ordering::Relaxed));
                            ui_generation(&weak, &generation, epoch, |w| {
                                w.set_resolving(true);
                                w.set_position(0.);
                                w.set_paused(false);
                            });
                        } else {
                            // A non-seekable stream can still repeat by reloading from zero.
                            playing = false;
                            let epoch = last_request
                                .as_ref()
                                .map(|(_, _, n)| *n)
                                .unwrap_or_else(|| generation.load(Ordering::Relaxed));
                            ui_generation(&weak, &generation, epoch, |w| {
                                w.invoke_play();
                            });
                        }
                    }
                    gst::MessageView::Buffering(message) => {
                        p.observe_source_buffering(message.src().map_or(0,|s|s.as_ptr() as usize),message.percent());
                        let was_waiting = buffering.waiting();
                        buffering.update(
                            message.src().map_or(0, |s| s.as_ptr() as usize),
                            message.percent(),
                        );
                        if was_waiting != buffering.waiting()
                            && let Some(suppressed) = buffering.diagnostic_transition()
                        {
                            crate::diagnostics::record(
                                "buffering_transition",
                                serde_json::json!({
                                    "waiting":buffering.waiting(),"music":music,"suppressed_transitions":suppressed,
                                    "elapsed_ms":startup_started.map(|t|t.elapsed().as_millis())
                                }),
                            );
                        }
                    }
                    gst::MessageView::Eos(_) => {
                        crate::diagnostics::record(
                            "playback_eos",
                            serde_json::json!({
                                "music":music,"video_id":last_request.as_ref().map(|v|v.0.id.as_str()),
                                "queue_len":state.lock().unwrap().queue.len()
                            }),
                        );
                        if audio_lease
                            .as_ref()
                            .is_some_and(|l| !paths.audio.contains(&l.video.id))
                        {
                            let _ = p.stop();
                            audio_lease = None;
                        }
                        playing = false;
                        pending = None;
                        resume_checkpoint = 0.;
                        if !music {
                            video_idle_since = Some(Instant::now());
                        }
                        let _ = p.keep_paused();
                        let epoch = last_request
                            .as_ref()
                            .map(|(_, _, n)| *n)
                            .unwrap_or_else(|| generation.load(Ordering::Relaxed));
                        ui_generation(&weak, &generation, epoch, |w| {
                            w.set_resolving(false);
                            w.set_paused(true);
                            w.set_controls_shown(true);
                            w.set_playback_active(false);
                            w.set_recommendations_pending(false);
                            w.set_playback_status("Playback ended".into());
                            w.invoke_next(true);
                        });
                    }
                    gst::MessageView::StateChanged(change)
                        if message
                            .src()
                            .is_some_and(|src| src == p.pipeline.upcast_ref::<gst::Object>())
                            && change.current() == gst::State::Playing =>
                    {
                        crate::diagnostics::record(
                            "playing",
                            serde_json::json!({"decoder":p.decoder.lock().unwrap().clone(),"frame_memory":p.memory_mode(),"startup_ms":startup_started.map(|t|t.elapsed().as_millis()),"startup_phases":STARTUP_STATS.lock().unwrap().as_ref().map(|s|serde_json::json!({"resolver":s.resolver,"resolve_ms":s.resolve_ms,"pipeline_load_ms":s.pipeline_load_ms}))}),
                        );
                        p.approve_audio_cache();
                        preroll_started = None;
                        if let Some((video, s, resume, keep_paused)) = pending.take() {
                            if let Some(stats) = STARTUP_STATS.lock().unwrap().as_mut() {
                                stats.first_playing_ms = startup_started
                                    .take()
                                    .map(|t| t.elapsed().as_secs_f64() * 1000.);
                                let mut history = STARTUP_HISTORY.lock().unwrap();
                                if history.len() >= 8 {
                                    history.remove(0);
                                }
                                history.push(stats.clone());
                            }
                            // Native startup has already applied this target in the worker.
                            if music || !p.manages_start() {
                                if resume > 0. {
                                    let _ = p.seek_with_state(resume, user_paused);
                                } else if keep_paused {
                                    let _ = p.keep_paused();
                                }
                            }
                            if let Some((_, _, n)) = &last_request {
                                finish_playback_priority(*n);
                            }
                            {
                                // Session-only avoidance still works when saved history is disabled.
                                let mut st = state.lock().unwrap();
                                let recent = &mut st.recent_played[usize::from(s.music)];
                                recent.retain(|id| id != &video.id);
                                recent.insert(0, video.id.clone());
                                recent.truncate(20);
                            }
                            if s.remember && !video.id.starts_with("external:") {
                                let mut st = state.lock().unwrap();
                                if st.settings.remember
                                    && st.history_error.is_none()
                                    && !st.removed_history[usize::from(s.music)].contains(&video.id)
                                {
                                    history::record(&mut st.history, s.music, video);
                                    if let Err(error) = history::save(&paths, &st.history) {
                                        ui(&weak, move |w| {
                                            w.set_status(
                                                format!("Could not save history: {error:#}").into(),
                                            )
                                        });
                                    }
                                }
                            }
                        }
                        let epoch = last_request
                            .as_ref()
                            .map(|(_, _, n)| *n)
                            .unwrap_or_else(|| generation.load(Ordering::Relaxed));
                        ui_generation(&weak, &generation, epoch, move |w| {
                            w.set_playback_active(true);
                            if music {
                                w.invoke_video_started(w.get_selected_id());
                            }
                            w.invoke_player_activity();
                            w.set_playback_status(
                                if w.get_playing_download() {
                                    "Playing cached music"
                                } else if music {
                                    "Playing audio only"
                                } else {
                                    "Playing inside Mochune"
                                }
                                .into(),
                            );
                        });
                    }
                    _ => {}
                }
            }
            if playing || pending.is_some() {
                video_idle_since = None;
            }
            if video_idle_since.is_some_and(|t| t.elapsed() >= Duration::from_secs(2)) {
                let queued_next = {
                    let st = state.lock().unwrap();
                    st.queue.len() > usize::from(st.queue_has_current)
                };
                // Keep the worker warm even if the UI is briefly busy dispatching
                // the next queued selection. A mode change/error releases it directly.
                if !queued_next {
                    p.release_video_process();
                    video_idle_since = None;
                }
            }
            let seek_result = p.poll_seek();
            match seek_result {
                Ok(true) => {
                    let revision = applied_seek_revision;
                    let current = seek_revision.clone();
                    ui(&weak, move |w| {
                        if current.load(Ordering::Acquire) == revision {
                            w.set_playback_active(true);
                            w.invoke_player_activity();
                        }
                    });
                }
                Ok(false) => {}
                Err(e) => {
                    let text = e.to_string();
                    crate::diagnostics::record("seek_failed", serde_json::json!({"error":text}));
                    let _ = p.stop();
                    playing = false;
                    pending = None;
                    ui(&weak, move |w| {
                        w.set_resolving(false);
                        w.set_playback_active(false);
                        w.set_paused(true);
                        w.set_player_error(format!("Seek failed: {text}").into());
                    });
                }
            }
            if last_acceleration.elapsed() >= Duration::from_secs(1) {
                last_acceleration = Instant::now();
                let report = p.acceleration_report(playing || pending.is_some(), music);
                ui(&weak, move |w| {
                    w.set_acceleration_verified(report.verified);
                    w.set_acceleration_status(report.status.into());
                    w.set_acceleration_details(report.details.into());
                });
            }
            #[cfg(target_os = "windows")]
            if (playing || pending.is_some()) && last_health.elapsed() >= Duration::from_secs(5) {
                last_health = Instant::now();
                let (buffered_ranges, buffered_estimated) = p.buffered_ranges();
                crate::diagnostics::record(
                    "playback_health",
                    serde_json::json!({"state":format!("{:?}",p.current_state()),"position":p.position().map(|t|t.seconds_f64()),"duration":p.duration().map(|t|t.seconds_f64()),"buffered_ranges":buffered_ranges,"buffered_estimated":buffered_estimated,"video_pts":crate::video::video_position(),"render_stats":crate::video::render_stats(),"decoder":p.decoder.lock().unwrap().clone(),"frame_memory":p.memory_mode(),"gpu_bridge":crate::video::gpu_bridge_stats(),"seek":p.seek_diagnostics(),"process_memory":crate::diagnostics::process_memory(),"acceleration":p.acceleration_report(playing || pending.is_some(), music),"pending":pending.is_some()}),
                );
            }
            if pending.is_some()
                && !p.seeking()
                && preroll_started.is_some_and(|t| t.elapsed() > preroll_budget)
            {
                crate::diagnostics::record(
                    "preroll_timeout",
                    serde_json::json!({"seconds":preroll_budget.as_secs(),"decoder":p.decoder.lock().unwrap().clone()}),
                );
                preroll_started = None;
                let report = serde_json::json!({"platform":std::env::consts::OS,"video_id":last_request.as_ref().map(|v|v.0.id.as_str()),"audio_only":music,"error":format!("Playback preroll timed out after {} seconds",preroll_budget.as_secs()),"decoder":p.decoder.lock().unwrap().clone()});
                let _ = paths.save("playback-error.json", &report);
                if music
                    && music_retry.schedule(
                        &last_request,
                        &generation,
                        &paths,
                        &stream_cache,
                        &weak,
                        "Playback startup timed out",
                    )
                {
                    retry_position = Some(
                        pending
                            .as_ref()
                            .map(|(_, _, position, paused)| (*position, *paused))
                            .unwrap_or((resume_checkpoint, user_paused)),
                    );
                    let _ = p.stop();
                    pending = None;
                    playing = false;
                    continue;
                }
                if let Some((_, _, n)) = &last_request {
                    finish_playback_priority(*n);
                }
                let _ = p.stop();
                p.release_video_process();
                pending = None;
                playing = false;
                ui(&weak, |w| {
                    w.set_resolving(false);
                    w.set_playback_active(false);
                    w.set_recommendations_pending(false);
                    w.set_player_error("Playback did not start. Try another quality or retry. Details are saved in playback-error.json.".into());
                    w.set_playback_status("Playback startup timed out".into());
                });
            }
            if playing || pending.is_some() {
                let now = Instant::now();
                let timestamp = if music {
                    p.position().map(|t| t.seconds_f64())
                } else {
                    Some(crate::video::video_position())
                };
                let advancing = playback_progress.observe(timestamp, now);
                let refill = p
                    .poll_rebuffer(playing && !music && pending.is_none(), user_paused)
                    .unwrap_or(false);
                let waiting = refill
                    || pending.is_some()
                    || p.seeking()
                    || (!user_paused && buffering.waiting() && !advancing);
                let visible =
                    loading.update(waiting, p.seeking() && pending.is_none() && !refill, now);
                let loading_revision = applied_seek_revision;
                let latest_seek = seek_revision.clone();
                let epoch = last_request
                    .as_ref()
                    .map(|v| v.2)
                    .unwrap_or_else(|| generation.load(Ordering::Relaxed));
                ui_generation(&weak, &generation, epoch, move |w| {
                    if latest_seek.load(Ordering::Acquire) != loading_revision {
                        return;
                    }
                    // First-picture readiness is published by the renderer, not StateChanged.
                    let visible = visible || (!music && !user_paused && !w.get_video_visible());
                    if w.get_resolving() != visible {
                        w.set_resolving(visible);
                    }
                });
            }
            if playing && !p.seeking() {
                let pos = crate::video::playback_state::position(
                    p.position().map(|t| t.seconds_f64()),
                    resume_checkpoint,
                    user_paused,
                );
                if pending.is_none()
                    && let Some(position) = pos.filter(|t| t.is_finite() && *t >= 0.)
                {
                    resume_checkpoint = position;
                }
                let duration = p.duration().map(|t| t.seconds_f64());
                known_duration = duration.or(known_duration);
                let (buffered_ranges, buffered_estimated) = p.buffered_ranges();
                let confirmed_ranges = p.confirmed_buffered_ranges();
                let paused = user_paused;
                let decoder = p.decoder.lock().unwrap().clone();
                let memory = p.memory_mode();
                let mode = if music {
                    "Audio only (video decoder idle)".to_owned()
                } else {
                    format!(
                        "Embedded GPU textures ({memory}, source frame rate) · decoder: {}",
                        if decoder.is_empty() {
                            "initializing"
                        } else {
                            &decoder
                        }
                    )
                };
                let epoch = last_request
                    .as_ref()
                    .map(|(_, _, n)| *n)
                    .unwrap_or_else(|| generation.load(Ordering::Relaxed));
                let position_revision = applied_seek_revision;
                let current_seek_revision = seek_revision.clone();
                ui_generation(&weak, &generation, epoch, move |w| {
                    if w.get_recommendations_pending()
                        && w.get_video_visible()
                        && w.get_playback_active()
                    {
                        w.set_recommendations_pending(false);
                        w.invoke_video_started(w.get_selected_id());
                    }
                    let ranges =
                        crate::timeline::styled_ranges(&buffered_ranges, &confirmed_ranges);
                    crate::timeline::publish(&w, ranges);
                    w.set_buffered_estimated(buffered_estimated);
                    w.set_active_decoder(decoder.into());
                    if current_seek_revision.load(Ordering::Acquire) == position_revision {
                        if let Some(pos) = pos {
                            w.set_position(pos as f32);
                        }
                        if let Some(duration) = duration {
                            w.set_duration(duration as f32);
                        }
                        w.set_clock(
                            format!(
                                "{} / {}",
                                clock(w.get_position() as f64),
                                clock(w.get_duration() as f64)
                            )
                            .into(),
                        );
                        w.set_paused(paused);
                    }
                    w.set_diagnostics(
                        format!(
                            "{mode}\nVideo queue: {} frame(s); presentation queue: 1 frame",
                            crate::video::VIDEO_QUEUE_FRAMES
                        )
                        .into(),
                    );
                });
            }
        }
    }
    if let Ok(p) = engine.as_mut() {
        let _ = p.stop();
    }
    drop(audio_lease);
}

#[derive(Serialize)]
struct BrowseStats {
    source: String,
    metadata_ms: f64,
    first_thumbnail_ms: Option<f64>,
    all_thumbnails_ms: Option<f64>,
    thumbnail_count: usize,
}
static BROWSE_STATS: Mutex<Option<BrowseStats>> = Mutex::new(None);
pub(super) fn browse_stats() -> Value {
    serde_json::to_value(&*BROWSE_STATS.lock().unwrap()).unwrap_or(Value::Null)
}

#[derive(Clone, Serialize)]
struct StartupStats {
    resolver: String,
    resolve_ms: f64,
    pipeline_load_ms: f64,
    first_playing_ms: Option<f64>,
}
static STARTUP_STATS: Mutex<Option<StartupStats>> = Mutex::new(None);
static STARTUP_HISTORY: Mutex<Vec<StartupStats>> = Mutex::new(Vec::new());
pub(super) fn startup_stats() -> Value {
    let latest = STARTUP_STATS.lock().unwrap().clone();
    serde_json::json!({"latest":latest,"recent":*STARTUP_HISTORY.lock().unwrap()})
}

pub fn probe_youtube(query: &str) -> Result<Value> {
    let paths = Paths::new();
    let s = Settings::default();
    let value = extract(&paths, &s, &search_target(query)?, true, None)?;
    let videos = videos_from(&value);
    let first = videos.first().context("No search results")?;
    let (_, width, height) = thumbnail(&paths, first)?;
    let resolved = extract(&paths, &s, &first.url(), false, None)?;
    Ok(
        serde_json::json!({"results":videos.len(),"first_title":first.title,"thumbnail":[width,height],"stream_height":resolved["height"],"split_streams":resolved["requested_formats"].as_array().map(Vec::len),"status":"search, thumbnail and stream resolution passed"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_section_settings_default_on_and_roundtrip_independently() {
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(old.theme_header && old.theme_library && old.theme_list && old.theme_player);
        let changed = Settings {
            theme_library: false,
            theme_player: false,
            ..old
        };
        let saved: Settings =
            serde_json::from_slice(&serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(saved.theme_header && saved.theme_list);
        assert!(!saved.theme_library && !saved.theme_player);
    }
    #[test]
    fn obsolete_music_restart_setting_is_ignored() {
        let settings: Settings = serde_json::from_str(r#"{"relaunch_for_music":true}"#).unwrap();
        assert!(
            !serde_json::to_string(&settings)
                .unwrap()
                .contains("relaunch_for_music")
        );
    }
    #[test]
    fn last_mode_is_saved_separately_from_audio_only_quality() {
        // Older settings used music for audio-only playback, not the visible mode.
        let legacy: Settings = serde_json::from_str(r#"{"music":true}"#).unwrap();
        assert!(legacy.last_music_mode);
        let fresh: Settings = serde_json::from_str("{}").unwrap();
        assert!(fresh.last_music_mode);
        let previous_video: Settings =
            serde_json::from_str(r#"{"last_music_mode":false}"#).unwrap();
        assert!(!previous_video.last_music_mode);
        let saved = Settings {
            last_music_mode: true,
            music: false,
            ..Settings::default()
        };
        let restored: Settings =
            serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert!(restored.last_music_mode);
        assert!(!restored.music);
    }
    #[test]
    fn manifests_and_combined_video_are_never_audio_cache_files() {
        let mut v = serde_json::json!({"vcodec":"none","protocol":"https","ext":"webm"});
        assert!(cacheable_audio(&v));
        v["protocol"] = "m3u8_native".into();
        assert!(!cacheable_audio(&v));
        v["protocol"] = "https".into();
        v["vcodec"] = "h264".into();
        assert!(!cacheable_audio(&v));
    }
    #[test]
    fn playback_has_priority_over_background_metadata() {
        assert!(!helper_can_start(false, 3, false));
        assert!(helper_can_start(false, 3, true));
        assert!(!helper_can_start(true, 3, true));
        assert!(helper_can_start(false, 0, false));
    }
    #[test]
    fn cancelled_helper_request_does_not_wait_or_start() {
        let _held = HelperPermit::acquire(None).unwrap();
        let generation = AtomicU64::new(2);
        assert!(HelperPermit::acquire(Some((&generation, 1))).is_err());
    }
    #[test]
    fn stream_link_cache_defaults_on_without_enabling_disk_caches() {
        let legacy: Settings = serde_json::from_str(r#"{"cache_enabled":false}"#).unwrap();
        assert!(legacy.stream_cache_enabled);
        assert!(!legacy.cache_enabled);
        assert!(!legacy.audio_cache_enabled);
        let off = Settings {
            stream_cache_enabled: false,
            ..Settings::default()
        };
        let saved = serde_json::to_string(&off).unwrap();
        assert!(
            !serde_json::from_str::<Settings>(&saved)
                .unwrap()
                .stream_cache_enabled
        );
        let mut staged = Settings::default();
        committed_preferences(&mut staged, &off);
        assert!(!staged.stream_cache_enabled);
    }
    #[test]
    fn legacy_account_settings_are_ignored() {
        let old: Settings =
            serde_json::from_str(r#"{"cookies":"legacy.txt","power_saver":true}"#).unwrap();
        assert!(!old.light_mode);
        assert!(!Settings::default().light_mode);
        assert!(serde_json::to_value(&old).unwrap().get("cookies").is_none());
        assert!(!old.cache_enabled);
        assert!(
            serde_json::to_value(&old)
                .unwrap()
                .get("power_saver")
                .is_none()
        );
        let mut staged = Settings {
            cache_enabled: true,

            ..Settings::default()
        };
        committed_preferences(&mut staged, &old);
        assert!(!staged.cache_enabled);
    }
    #[test]
    fn search_accepts_and_canonicalizes_pasted_links() {
        assert_eq!(
            search_target("youtu.be/abcdefghijk?si=xyz").unwrap(),
            "https://www.youtube.com/watch?v=abcdefghijk"
        );
        assert_eq!(
            search_target("music.youtube.com/watch?v=abcdefghijk&list=RD123").unwrap(),
            "https://www.youtube.com/watch?v=abcdefghijk"
        );
        assert_eq!(
            search_target("piano music").unwrap(),
            "ytsearch30:piano music"
        );
        assert!(search_target("https://youtube.com.evil.test/watch?v=abcdefghijk").is_err());
        assert!(search_target(" ").is_err());
    }
    #[test]
    fn volume_has_a_unity_ceiling_and_rejects_non_finite_values() {
        assert_eq!(sanitize_volume(140.), 100.);
        assert_eq!(sanitize_volume(-10.), 0.);
        assert_eq!(sanitize_volume(f32::NAN), 70.);
        assert_eq!(sanitize_volume(f32::INFINITY), 70.);
    }
    #[test]
    fn quality_and_music_selection() {
        let mut s = Settings::default();
        assert!(format_selector(&s).contains("height<=720"));
        s.music = true;
        assert_eq!(format_selector(&s), "bestaudio");
        s.music = false;
        s.quality = 4;
        assert!(format_selector(&s).contains("height<=2160"));
    }
    #[test]
    fn picker_uses_real_fps_and_combined_size() {
        let info = serde_json::json!({"duration":100,"requested_formats":[{"format_id":"v60","vcodec":"avc1"}],"formats":[
            {"format_id":"v30","height":720,"fps":30,"vcodec":"avc1","acodec":"none","filesize":10000000},
            {"format_id":"v60","height":720,"fps":60,"vcodec":"avc1","acodec":"none","filesize":20000000},
            {"format_id":"a","vcodec":"none","acodec":"opus","abr":128,"filesize":2000000}
        ]});
        let (options, chosen) = playback_options(&info, &Settings::default());
        assert_eq!(options.len(), 3);
        assert_eq!(options[chosen].label, "720p · 60 fps · ≈22 MB");
        assert_eq!(options[0].label, "720p · 30 fps · ≈12 MB");
        assert_eq!(options.last().unwrap().label, "Audio only · ≈2 MB");
        assert_eq!(options[chosen].selector.as_deref(), Some("v60+bestaudio"));
        assert!(options.last().unwrap().music);
        assert_eq!(size_label(None), "");
        assert_eq!(view_count(1_200_000), "1.2M views");
        let music = Settings {
            music: true,
            ..Settings::default()
        };
        let (options, chosen) = playback_options(&info, &music);
        assert!(options[chosen].music);
    }
    #[test]
    fn only_youtube_urls_are_accepted() {
        assert!(search_target("https://youtube.com.evil.test/watch?v=123").is_err());
        assert!(search_target("https://www.youtube.com/watch?v=abcdefghijk").is_ok());
        assert_eq!(search_target("--help").unwrap(), "ytsearch30:--help");
        assert!(search_target(" ").is_err());
    }
    #[test]
    fn flat_results_skip_invalid_ids() {
        let v = serde_json::json!({"entries":[{"id":"abcdefghijk","title":"Hello","channel":"World","duration":90},{"id":"../../bad"},null]});
        let videos = videos_from(&v);
        assert_eq!(videos.len(), 1);
        assert_eq!(videos[0].title, "Hello");
        assert_eq!(clock(videos[0].duration), "1:30");
    }
    #[test]
    fn quality_picker_prefers_direct_video_over_hls_at_same_resolution_and_frame_rate() {
        let info = serde_json::json!({"formats":[
            {"format_id":"hls","height":720,"fps":24,"vcodec":"avc1","acodec":"none","protocol":"m3u8_native","tbr":3000},
            {"format_id":"direct","height":720,"fps":24,"vcodec":"avc1","acodec":"none","protocol":"https","tbr":500},
            {"format_id":"audio","vcodec":"none","acodec":"opus","protocol":"https","abr":128}
        ]});
        let (options, _) = playback_options(&info, &Settings::default());
        assert_eq!(options[0].selector.as_deref(), Some("direct+bestaudio"));
    }
}

/// Diagnostic for bounded public suggestions; no cookies or media are requested.
pub fn probe_recommendations(id: &str) -> Result<Value> {
    let paths = Paths::new();
    let settings: Settings = read_json(paths.data.join("settings.json"));
    let history: Vec<Video> = if settings.remember {
        history::load(&paths)?[usize::from(settings.music)].clone()
    } else {
        Vec::new()
    };
    let mut seed = history
        .iter()
        .find(|v| v.id == id)
        .cloned()
        .or_else(|| preview_metadata(id).ok());
    let generation = AtomicU64::new(1);
    let page = if let Some(seed) = seed.as_ref() {
        recommendations::filtered_suggestions(
            id,
            &settings,
            &history,
            seed,
            recommendations::count(&settings),
            &generation,
            1,
        )?
    } else {
        recommendations::watch_suggestions(id, true)?
    };
    let raw_count = page.videos.len();
    if page.classified {
        if let Some(seed) = seed.as_mut() {
            seed.music = page.music;
        }
    }
    let videos = recommendations::filter(page.videos, &settings, &history, seed.as_ref());
    Ok(
        serde_json::json!({"source":"YouTube public watch suggestions with local filters and relevance","raw_count":raw_count,"requested":recommendations::count(&settings),"count":videos.len(),"seed_found":seed.is_some(),"videos":videos.iter().map(|v|serde_json::json!({"id":v.id,"title":v.title,"channel":v.channel,"duration":v.duration,"views":v.views})).collect::<Vec<_>>()}),
    )
}

/// A public stream URL can be rejected transiently, after public extraction.
/// The ephemeral marker bounds recovery to one fresh extraction per user request.
fn public_stream_retry(public: bool, settings: &Settings, error: &str) -> bool {
    public
        && !settings.public_fallback
        && (error.to_ascii_lowercase().contains("forbidden") || error.contains("403"))
}
#[cfg(test)]
mod public_retry_tests {
    use super::*;
    #[test]
    fn retry_only_public_stream_once() {
        let mut s = Settings::default();
        assert!(public_stream_retry(true, &s, "Forbidden"));
        assert!(!public_stream_retry(false, &s, "Forbidden"));
        assert!(!public_stream_retry(true, &s, "Not Found"));
        s.public_fallback = true;
        assert!(!public_stream_retry(true, &s, "HTTP 403"));
        s.public_fallback = false;
        assert!(public_stream_retry(true, &s, "Forbidden"));
        s.music = true;
        assert!(public_stream_retry(true, &s, "HTTP 403"));
        assert!(!public_stream_retry(false, &s, "HTTP 403"));
        s.public_fallback = true;
        assert!(!public_stream_retry(true, &s, "Forbidden"));
    }
}

#[cfg(test)]
mod player_quality_tests {
    use super::*;
    #[test]
    fn compact_badges_keep_unusual_heights_and_fractional_frame_rates() {
        assert_eq!(compact_quality("2160p · 60 fps · ≈42 MB"), "4k@60");
        assert_eq!(compact_quality("1080p · 59.94 fps"), "1080@59.94");
        assert_eq!(compact_quality("1350p · 23.976 fps"), "1350@23.976");
        assert_eq!(compact_quality("Audio only · ≈4 MB"), "Audio");
        let info = serde_json::json!({"formats":[
            {"format_id":"a","height":1350,"fps":23.976,"vcodec":"avc1","acodec":"none"},
            {"format_id":"b","height":1350,"fps":24,"vcodec":"avc1","acodec":"none"}
        ]});
        let (options, _) = playback_options(&info, &Settings::default());
        assert!(
            options
                .iter()
                .any(|o| o.label.starts_with("1350p · 23.976 fps"))
        );
        assert!(
            options
                .iter()
                .any(|o| o.label.starts_with("1350p · 24 fps"))
        );
    }
}

fn video_quality(format: &Value) -> Option<(u64, Option<f64>)> {
    let height = format["height"].as_u64().filter(|h| *h > 0)?;
    let fps = format["fps"].as_f64().filter(|f| f.is_finite() && *f > 0.);
    Some((height, fps))
}

fn native_format_retry(
    s: &Settings,
    decoder: &str,
    quality: Option<(u64, Option<f64>)>,
    error: &str,
) -> Option<String> {
    let error = error.to_ascii_lowercase();
    if s.music
        || error.contains("forbidden")
        || error.contains("403")
        || ![
            "internal data stream",
            "not-negotiated",
            "could not demultiplex",
            "decoding error",
            "decoder failed",
            "could not decode",
            "missing decoder",
            "no suitable plugin",
            "missing a plug-in",
        ]
        .iter()
        .any(|term| error.contains(term))
    {
        return None;
    }
    // The resolved stream is the authority, including unusual resolutions and
    // fractional frame rates. A codec retry must never change either dimension.
    let (height, fps) = quality.filter(|(height, _)| *height <= 2160)?;
    let fps = fps.map(|fps| format!("[fps={fps}]")).unwrap_or_default();
    let exact = format!("[height={height}]{fps}[protocol=https]");
    let h264 = format!("bestvideo[vcodec^=avc]{exact}+bestaudio[protocol=https]");
    let hevc = format!(
        "bestvideo[vcodec^=hev]{exact}+bestaudio[protocol=https]/bestvideo[vcodec^=hvc]{exact}+bestaudio[protocol=https]"
    );
    let vp9 = format!("bestvideo[vcodec^=vp9]{exact}+bestaudio[protocol=https]");
    let av1 = format!("bestvideo[vcodec^=av01]{exact}+bestaudio[protocol=https]");
    Some(if decoder.contains("vp9") {
        format!("{h264}/{hevc}/{av1}/{vp9}")
    } else if decoder.contains("h265") || decoder.contains("hevc") {
        format!("{h264}/{vp9}/{av1}/{hevc}")
    } else {
        format!("{h264}/{hevc}/{vp9}/{av1}")
    })
}
#[cfg(test)]
mod native_retry_tests {
    use super::*;
    #[test]
    fn alternate_codec_preserves_exact_resolution_and_frame_rate() {
        let s = Settings::default();
        for (height, fps) in [(720, 60.), (1080, 59.94), (2160, 60.), (1350, 23.976)] {
            let quality = Some((height, Some(fps)));
            for decoder in ["d3d12h264dec", "d3d12vp9dec"] {
                let selector =
                    native_format_retry(&s, decoder, quality, "Internal data stream error")
                        .unwrap();
                for branch in selector.split('/') {
                    assert!(branch.contains(&format!("[height={height}][fps={fps}]")));
                    assert!(!branch.contains("<="));
                }
            }
        }
        assert!(native_format_retry(&s, "", None, "Internal data stream error").is_none());
        for error in [
            "Your GStreamer installation is missing a plug-in",
            "no suitable plugins found: Missing decoder: video/x-raw",
        ] {
            let selector = native_format_retry(&s, "", Some((2160, Some(29.97))), error)
                .expect("decoder availability failures should retry with a supported codec");
            assert!(selector.contains("[height=2160][fps=29.97]"));
            assert!(selector.contains("vcodec^=hev"));
            assert!(selector.contains("vcodec^=hvc"));
            assert!(selector.contains("vcodec^=av01"));
        }
        for error in ["Forbidden 403", "Network is unreachable"] {
            assert!(native_format_retry(&s, "", Some((720, Some(60.))), error).is_none());
        }
        assert!(
            native_format_retry(
                &Settings { music: true, ..s },
                "",
                Some((720, Some(60.))),
                "Internal data stream error"
            )
            .is_none()
        );
    }
    #[test]
    fn video_choices_and_automatic_selection_stop_at_4k() {
        let s = Settings {
            quality: 4,
            ..Settings::default()
        };
        let info = serde_json::json!({"formats":[
            {"format_id":"hd","height":1080,"fps":60,"vcodec":"avc1","acodec":"none"},
            {"format_id":"uhd","height":2160,"fps":60,"vcodec":"vp9","acodec":"none"},
            {"format_id":"oversized","height":4320,"fps":60,"vcodec":"av01","acodec":"none"}
        ]});
        let (options, _) = playback_options(&info, &s);
        assert_eq!(options.iter().filter(|o| !o.music).count(), 2);
        assert!(options.iter().all(|o| !o.label.starts_with("4320")));
        assert_eq!(
            format_selector(&s),
            "bestvideo*[height<=2160]+bestaudio/best[height<=2160]"
        );
        assert!(native_format_retry(&s, "", Some((4320, Some(60.))), "decoder failed").is_none());
    }
    #[test]
    fn quality_label_tracks_actual_stream_even_when_group_prefers_another_codec() {
        let info = serde_json::json!({"format_id":"vp9", "height":720,"fps":60,"formats":[
            {"format_id":"h264","height":720,"fps":60,"vcodec":"avc1","acodec":"none"},
            {"format_id":"vp9","height":720,"fps":60,"vcodec":"vp9","acodec":"none"},
            {"format_id":"4k","height":2160,"fps":60,"vcodec":"vp9","acodec":"none"}
        ]});
        let (options, selected) = playback_options(
            &info,
            &Settings {
                quality: 4,
                ..Settings::default()
            },
        );
        assert!(options[selected].label.starts_with("720p · 60 fps"));
        assert_eq!(video_quality(&info), Some((720, Some(60.))));
    }
}
