// GPU texture integration adapted from Slint's MIT-licensed GStreamer example.
// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT
use crate::SetupWindow;
use anyhow::{Context, Result, bail};
#[cfg(not(target_os = "windows"))]
use glow::HasContext;
use gst::prelude::*;
#[cfg(not(target_os = "windows"))]
use gst_gl::prelude::*;
mod acceleration;
mod buffered;
pub(crate) mod buffering;
mod delivery;
mod indexed_http;
#[cfg(target_os = "linux")]
mod interpolation;
pub(crate) fn interpolation_stats()->serde_json::Value {
    #[cfg(target_os="linux")] { interpolation::stats() }
    #[cfg(not(target_os="linux"))] { serde_json::json!({"supported":false}) }
}
#[cfg(any(target_os = "windows", feature = "native-overlay-test"))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod native_overlay;
pub(crate) mod playback_state;
#[cfg(any(target_os = "windows", test))]
mod process_protocol;
mod rebuffer;
mod source_hls;
#[cfg(test)]
mod seek_integration;
#[cfg(all(target_os = "windows", feature = "legacy-wgpu"))]
mod windows_gpu;
#[cfg(target_os = "windows")]
mod windows_native;
#[cfg(target_os = "windows")]
mod windows_overlay;
#[cfg(target_os = "windows")]
mod windows_process;
use slint::ComponentHandle;
#[cfg(all(target_os = "windows", feature = "legacy-wgpu"))]
use windows_gpu::TextureSink;
// Decoder reference-picture storage stays driver-managed. Only trim our
// downstream queue, where extra high-resolution GPU frames are avoidable.
pub const VIDEO_QUEUE_FRAMES: u32 = if cfg!(target_os = "windows") { 1 } else { 3 };
static LAST_VIDEO_PTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn video_position() -> f64 {
    LAST_VIDEO_PTS.load(Ordering::Acquire) as f64 / 1_000_000_000.
}
static RENDER_PASSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DRAW_MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static IMPORT_MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn render_stats() -> (u64, u64, u64) {
    (
        RENDER_PASSES.load(Ordering::Relaxed),
        DRAW_MICROS.load(Ordering::Relaxed),
        IMPORT_MICROS.load(Ordering::Relaxed),
    )
}

#[cfg(target_os = "windows")]
pub fn gpu_bridge_stats() -> serde_json::Value {
    #[cfg(feature = "legacy-wgpu")]
    if !native_windows_enabled() {
        return windows_gpu::stats();
    }
    windows_native::stats()
}

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

// Opt-in diagnostics: record submitted frame timing without pixel readback.
type FrameTimes = Mutex<Vec<(std::time::Instant, u64)>>;
static FRAME_TIMES: std::sync::OnceLock<Option<FrameTimes>> = std::sync::OnceLock::new();
#[cfg(any(not(target_os = "windows"), feature = "legacy-wgpu"))]
fn record_presented_frame(pts: u64) {
    if let Some(times) = FRAME_TIMES.get_or_init(|| {
        std::env::var_os("MOCHUNE_SMOKE_VISUAL_TIMING").map(|_| Mutex::new(Vec::new()))
    }) {
        let mut times = times.lock().unwrap();
        if times.len() < 10_000 {
            times.push((std::time::Instant::now(), pts));
        }
    }
}
pub fn frame_timing_stats() -> serde_json::Value {
    let Some(Some(times)) = FRAME_TIMES.get() else {
        return serde_json::Value::Null;
    };
    let times = times.lock().unwrap();
    // Exclude startup and any diagnostic snapshot readbacks (the first 5 seconds only).
    let mut wall = Vec::new();
    let mut media = Vec::new();
    for pair in times.windows(2) {
        if pair[0].0.duration_since(times[0].0).as_secs_f64() < 1.0
            || pair[1].0.duration_since(times[0].0).as_secs_f64() > 4.0
        {
            continue;
        }
        wall.push(pair[1].0.duration_since(pair[0].0).as_secs_f64() * 1000.);
        if let Some(delta) = pair[1].1.checked_sub(pair[0].1) {
            media.push(delta as f64 / 1_000_000.);
        }
    }
    wall.sort_by(f64::total_cmp);
    media.sort_by(f64::total_cmp);
    let quantile = |v: &[f64], q: f64| {
        v.get(((v.len().saturating_sub(1)) as f64 * q) as usize)
            .copied()
    };
    serde_json::json!({"submitted_frames":times.len(),"sample_intervals":wall.len(),"interval_ms_p50":quantile(&wall,0.5),"interval_ms_p95":quantile(&wall,0.95),"interval_ms_max":wall.last(),"media_interval_ms_p50":quantile(&media,0.5),"intervals_over_50ms":wall.iter().filter(|t| **t>50.).count()})
}

#[derive(Default)]
struct FrameMailbox {
    next: Option<(gst_video::VideoInfo, gst::Buffer)>,
    epoch: u64,
    flushing: bool,
    #[cfg(target_os="linux")]
    received:Option<std::time::Instant>,
    #[cfg(target_os="linux")]
    timed:bool,
    #[cfg(target_os="linux")]
    motion:Option<std::sync::Weak<interpolation::worker::Shared>>,
    #[cfg(target_os="linux")]
    due:Option<std::time::Instant>,
}
impl FrameMailbox {
    fn clear(&mut self, flushing: bool) {
        self.next = None;
        #[cfg(target_os="linux")] {self.received=None;self.due=None;self.timed=false;if let Some(w)=self.motion.take().and_then(|w|w.upgrade()){w.cancel();}}
        self.epoch = self.epoch.wrapping_add(1);
        self.flushing = flushing;
    }
}

// ASYNC_DONE has no seek identity at the pipeline level. Require a buffer
// following this seek's SEGMENT on each active output before completing it.
#[derive(Default)]
struct SeekProgress {
    expected: Option<gst::Seqnum>,
    segments: u8,
    buffers: u8,
    video_pts: Option<gst::ClockTime>,
    video_duration: Option<gst::ClockTime>,
    target: f64,
    target_seen: bool,
    started: Option<std::time::Instant>,
    segment_ms: [Option<u64>; 2],
    output_ms: [Option<u64>; 2],
}
impl SeekProgress {
    fn begin(&mut self, seq: gst::Seqnum, target: f64) {
        self.expected = Some(seq);
        self.started = Some(std::time::Instant::now());
        self.segment_ms = [None; 2];
        self.output_ms = [None; 2];
        self.segments = 0;
        self.buffers = 0;
        self.video_pts = None;
        self.video_duration = None;
        self.target = target;
        self.target_seen = false;
    }
    fn segment(&mut self, seq: gst::Seqnum, stream: u8) {
        if self.expected == Some(seq) {
            self.segments |= stream;
            self.segment_ms[usize::from(stream == 2)] =
                self.started.map(|at| at.elapsed().as_millis() as u64);
        } else {
            self.segments &= !stream;
        }
        self.buffers &= !stream;
        if stream == 2 {
            self.target_seen = false;
        }
    }
    fn video(&mut self, pts: Option<gst::ClockTime>, duration: Option<gst::ClockTime>) {
        self.video_pts = pts;
        self.video_duration = duration;
        if self.expected.is_some()
            && self.segments & 2 != 0
            && pts.is_some_and(|pts| {
                (pts.seconds_f64() - self.target).abs()
                    <= duration.map_or(0.75, |d| (d.seconds_f64() + 0.05).max(0.75))
            })
        {
            // A successful target sample is evidence for this seek's lifetime,
            // not a narrow window that disappears before the worker next polls.
            self.target_seen = true;
        }
    }
    fn buffer(&mut self, stream: u8) {
        if self.expected.is_some() && self.segments & stream != 0 {
            if self.buffers & stream == 0 {
                self.output_ms[usize::from(stream == 2)] =
                    self.started.map(|at| at.elapsed().as_millis() as u64);
            }
            self.buffers |= stream;
        }
    }
    fn stages(&self) -> serde_json::Value {
        serde_json::json!({"audio_segment_ms":self.segment_ms[0],"video_segment_ms":self.segment_ms[1],
            "audio_preroll_ms":self.output_ms[0],"video_target_frame_ms":self.output_ms[1]})
    }
    fn ready(&self, required: u8) -> bool {
        self.expected.is_some() && self.buffers & required == required
    }
    fn ready_for_target(&self, required: u8) -> bool {
        self.ready(required) && (required & 2 == 0 || self.target_seen)
    }
    fn ready_at_playing_clock(
        &self,
        required: u8,
        position: Option<gst::ClockTime>,
        elapsed: std::time::Duration,
    ) -> bool {
        if !self.ready(required) || required & 2 == 0 {
            return false;
        }
        let (Some(pts), Some(clock)) = (self.video_pts, position) else {
            return false;
        };
        let tolerance = self
            .video_duration
            .map_or(0.25, |d| (d.seconds_f64() + 0.05).max(0.25));
        let clock = clock.seconds_f64();
        // A decoder may suppress/drop the exact target preroll. Current-segment
        // video advancing with the new media clock still proves completion.
        // Bound the clock by this seek's target and elapsed time so an old,
        // mutually matching clock/picture pair cannot complete a new seek.
        (pts.seconds_f64() - clock).abs() <= tolerance
            && clock >= self.target - tolerance
            && clock <= self.target + elapsed.as_secs_f64() + tolerance
    }
}
fn watch_seek_output(element: &gst::Element, progress: Arc<Mutex<SeekProgress>>, stream: u8) {
    if let Some(pad) = element.static_pad("sink") {
        pad.add_probe(
            gst::PadProbeType::BUFFER
                | gst::PadProbeType::EVENT_DOWNSTREAM
                | gst::PadProbeType::EVENT_FLUSH,
            move |_, info| {
                let mut progress = progress.lock().unwrap();
                if let Some(event) = info.event() {
                    match event.view() {
                        gst::EventView::FlushStart(_) => {
                            progress.segments &= !stream;
                            progress.buffers &= !stream;
                            if stream == 2 {
                                progress.target_seen = false;
                            }
                        }
                        gst::EventView::Segment(_) => {
                            progress.segment(event.seqnum(), stream);
                        }
                        _ => {}
                    }
                } else if let Some(buffer) = info.buffer()
                    && !buffer.flags().contains(gst::BufferFlags::DECODE_ONLY)
                {
                    if stream == 2 {
                        if buffer.flags().contains(gst::BufferFlags::CORRUPTED)
                            || (progress.expected.is_some()
                                && progress.segments & 2 != 0
                                && buffer
                                    .pts()
                                    .is_some_and(|p| p.seconds_f64() + 0.1 < progress.target))
                        {
                            return gst::PadProbeReturn::Drop;
                        }
                        progress.video(buffer.pts(), buffer.duration());
                    }
                    progress.buffer(stream);
                }
                gst::PadProbeReturn::Ok
            },
        );
    }
}
struct PendingSeek {
    target: f64,
    resume: bool,
    started: std::time::Instant,
    repreroll: bool,
    await_pause: bool,
    recovered: bool,
}
#[cfg(any(target_os = "windows", test))]
struct SeekWatch {
    target: f64,
    started: std::time::Instant,
    recovered: bool,
    last_video_pts: Option<gst::ClockTime>,
}

fn range_movie_uri(uri:&str)->bool{url::Url::parse(uri).is_ok_and(|u|u.scheme()=="https"&&u.fragment().is_some_and(|s|s.starts_with("mochune-mp4=")))}

/// One pipeline supplies a shared clock for adaptive video and audio URIs.
/// Windows video runs in a disposable child process; music stays local. Linux
/// imports GPU textures. Neither path copies decoded video pixels into Rust.
pub struct Engine {
    pub pipeline: gst::Pipeline,
    pub messages: mpsc::Receiver<gst::Message>,
    pub decoder: Arc<Mutex<String>>,
    evidence: Arc<acceleration::Evidence>,
    video_sink: gst::Element,
    transient: Vec<gst::Element>,
    buffer_state: Mutex<buffered::Snapshot>,
    recovery: rebuffer::Recovery,
    source_hls: source_hls::Gate,
    ready: Arc<AtomicBool>,
    setup_error: Arc<Mutex<Option<String>>>,
    volume: Option<gst::Element>,
    frames: Arc<Mutex<FrameMailbox>>,
    presentation: Arc<AtomicBool>,
    pending_seek: Option<PendingSeek>,
    seek_progress: Arc<Mutex<SeekProgress>>,
    #[cfg(target_os = "windows")]
    native: Option<windows_native::Controller>,
    #[cfg(target_os = "windows")]
    remote: Option<windows_process::Client>,
    #[cfg(any(target_os = "windows", test))]
    seek_watch: Option<SeekWatch>,
    queued_seek: Option<f64>,
    cache_capture: Option<Arc<Mutex<crate::app::audio_cache::Capture>>>,
}
impl Engine {
    #[cfg(test)]
    pub fn load(
        &mut self,
        uri: &str,
        audio: Option<&str>,
        music: bool,
        hardware: bool,
        volume: f64,
        headers: &[(&str, &str)],
        audio_headers: &[(&str, &str)],
        capture: Option<Arc<Mutex<crate::app::audio_cache::Capture>>>,
        start: Option<(f64, bool)>,
    ) -> Result<()> {
        self.load_buffered(
            uri,
            audio,
            music,
            hardware,
            volume,
            headers,
            audio_headers,
            capture,
            start,
            buffering::Hints::default(),
        )
    }
    pub(crate) fn load_buffered(
        &mut self,
        uri: &str,
        audio: Option<&str>,
        music: bool,
        _hardware: bool,
        volume: f64,
        headers: &[(&str, &str)],
        audio_headers: &[(&str, &str)],
        capture: Option<Arc<Mutex<crate::app::audio_cache::Capture>>>,
        start: Option<(f64, bool)>,
        buffer_hints: buffering::Hints,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !self.ready.load(Ordering::Acquire) {
            if let Some(error) = self.setup_error.lock().unwrap().clone() {
                bail!("Embedded GPU setup: {error}");
            }
            if std::time::Instant::now() >= deadline {
                bail!(
                    "The embedded GPU surface is not ready. Try Play again after the window appears."
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        self.stop()?;
        self.source_hls.reset(!music && source_hls::relay_uri(uri));
        if self.source_hls.enabled {
            let late=self.video_sink.find_property("max-lateness").map(|_|self.video_sink.property::<i64>("max-lateness"));
            let qos=self.video_sink.find_property("qos").map(|_|self.video_sink.property::<bool>("qos"));
            self.source_hls.sink_defaults=Some((late,qos));
            if late.is_some(){self.video_sink.set_property("max-lateness",100_000_000i64);}
            if qos.is_some(){self.video_sink.set_property("qos",true);}
        }
        self.cache_capture = capture.clone();
        #[cfg(target_os = "windows")]
        if let Some(remote) = self.remote.as_mut() {
            if music {
                remote.shutdown();
            } else {
                self.presentation.store(true, Ordering::Release);
                return remote.load(
                    uri,
                    audio,
                    true,
                    volume,
                    headers,
                    audio_headers,
                    start.unwrap_or((0., false)),
                    buffer_hints,
                );
            }
        }
        #[cfg(target_os = "windows")]
        if !music
            && self.remote.is_none()
            && let Some(native) = &self.native
        {
            self.video_sink = native.make_sink()?;
            watch_seek_output(&self.video_sink, self.seek_progress.clone(), 2);
        }

        // Audio-only playback has no unconnected video sink waiting for preroll.
        if music {
            if self.video_sink.parent().is_some() {
                self.pipeline.remove(&self.video_sink)?;
            }
        } else {
            if self.video_sink.parent().is_none() {
                self.pipeline.add(&self.video_sink)?;
            }
        }
        self.presentation.store(!music, Ordering::Release);
        *self.decoder.lock().unwrap() = String::new();
        #[cfg(not(target_os = "windows"))]
        for name in [
            "vah264dec",
            "vah265dec",
            "vavp9dec",
            "vaav1dec",
            "vaapih264dec",
            "d3d11h264dec",
            "d3d11h265dec",
            "d3d11vp9dec",
            "d3d11av1dec",
            "d3d12h264dec",
            "d3d12h265dec",
            "d3d12vp9dec",
            "d3d12vp8dec",
            "d3d12av1dec",
            "nvh264dec",
        ] {
            if let Some(factory) = gst::ElementFactory::find(name) {
                factory.set_rank(gst::Rank::from(356));
            }
        }
        // The small native AAC decoder avoids mapping the broad FFmpeg codec
        // dependency tree for ordinary AAC music. Keep automatic fallbacks where absent.
        if let Some(factory) = gst::ElementFactory::find("faad") {
            factory.set_rank(gst::Rank::from(276));
        }
        let audio_queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 0u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 2_000_000_000u64)
            .build()?;
        let convert = gst::ElementFactory::make("audioconvert").build()?;
        let resample = gst::ElementFactory::make("audioresample").build()?;
        let gain = gst::ElementFactory::make("volume")
            .property("volume", linear_volume(volume))
            .build()?;
        let audio_sink = gst::ElementFactory::make("autoaudiosink").build()?;
        watch_seek_output(&audio_sink, self.seek_progress.clone(), 1);
        let video_queue = if music {
            None
        } else {
            Some(
                gst::ElementFactory::make("queue")
                    .property("max-size-buffers", VIDEO_QUEUE_FRAMES)
                    .property("max-size-bytes", 0u32)
                    .property("max-size-time", 0u64)
                    .build()?,
            )
        };
        self.transient = vec![
            audio_queue.clone(),
            convert.clone(),
            resample.clone(),
            gain.clone(),
            audio_sink.clone(),
        ];
        if let Some(q) = &video_queue {
            self.transient.push(q.clone());
        }
        self.pipeline.add_many(&self.transient)?;
        gst::Element::link_many([&audio_queue, &convert, &resample, &gain, &audio_sink])?;
        if let Some(q) = &video_queue {
            q.link(&self.video_sink)?;
        }
        self.volume = Some(gain);
        self.add_source(
            uri,
            video_queue,
            if audio.is_none() {
                Some(audio_queue.clone())
            } else {
                None
            },
            headers,
            capture,
            buffering::Plan::new(buffer_hints.duration, buffer_hints.primary_bps, !music),
        )?;
        if let Some(audio) = audio {
            self.add_source(
                audio,
                None,
                Some(audio_queue),
                audio_headers,
                None,
                buffering::Plan::new(buffer_hints.duration, buffer_hints.audio_bps, false),
            )?;
        }
        self.pipeline
            .set_state(if start.is_some() {
                gst::State::Paused
            } else {
                gst::State::Playing
            })
            .context("Cannot start the embedded media pipeline")?;
        Ok(())
    }
    fn add_source(
        &mut self,
        uri: &str,
        video: Option<gst::Element>,
        audio: Option<gst::Element>,
        headers: &[(&str, &str)],
        capture: Option<Arc<Mutex<crate::app::audio_cache::Capture>>>,
        plan: buffering::Plan,
    ) -> Result<()> {
        // The opt-in offline recorder currently writes a contiguous stream.
        // Keep its sequential source until the file is complete; random header/
        // index reads would otherwise abort recording before normal playback.
        let indexed_uri = indexed_http::prepare(uri, capture.is_none())?;
        let source = gst::ElementFactory::make("uridecodebin")
            .property("uri", indexed_uri.as_str())
            // A seekable queue2 ring supports fragmented-MP4 seeks without
            // downloadbuffer's fixed 512 KiB wait-ahead. That wait makes tiny,
            // low-bitrate still-image videos fetch almost to EOF before preroll.
            .property("download", false)
            .property("use-buffering", false)
            .property("buffer-duration", 0i64)
            .property("ring-buffer-max-size", plan.ring)
            .property("buffer-size", plan.initial(video.is_some()))
            .build()?;
        if range_movie_uri(uri) {
            // Regular source MP4 files may put moov after the entire mdat.
            // Do not hide the indexed source's pull/range scheduling behind a
            // streaming ring; qtdemux must jump directly to the tail index.
            source.set_property("ring-buffer-max-size",0u64);
            source.set_property("buffer-size",-1i32);
            crate::diagnostics::record("source_movie_range_adapter",serde_json::json!({"range_requests":true}));
        }
        if source_hls::relay_uri(uri) && video.is_some() {
            // HLS has demuxed queues, not the indexed HTTP ring. Bound compressed
            // lookahead independently; do not expand decoded/GPU frame queues.
            source.set_property("ring-buffer-max-size",0u64);
            source.set_property("use-buffering",true);
            source.set_property("buffer-size",4i32*1024*1024);
            source.set_property("buffer-duration",4_000_000_000i64);
            crate::diagnostics::record("source_hls_buffer_policy",serde_json::json!({"compressed_bytes":4*1024*1024,"duration_ms":4000}));
        }
        crate::diagnostics::record(
            "compressed_buffer_plan",
            serde_json::json!({"video":video.is_some(),"plan":plan,"initial_bytes":plan.initial(video.is_some())}),
        );
        hardware_video_only(&source);
        self.recovery.track(source.clone(), plan);
        let want_video = video.is_some();
        if want_video {
            self.buffer_state.lock().unwrap().track(&source);
        }
        let want_audio = audio.is_some();
        let source_started = std::time::Instant::now();
        let source_id = source.name().to_string();
        if uri.starts_with("http://") || uri.starts_with("https://") {
            source.connect("source-setup", false, move |args| {
                if let Ok(element) = args[1].get::<gst::Element>()
                    && let Some(pad) = element.static_pad("src")
                {
                    let source_id = source_id.clone();
                    crate::diagnostics::record("stream_source_ready",serde_json::json!({
                        "source":source_id,"factory":element.factory().map(|f|f.name().to_string()),
                        "elapsed_ms":source_started.elapsed().as_secs_f64()*1000.
                    }));
                    pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                        crate::diagnostics::record(
                            "stream_first_bytes",
                            serde_json::json!({
                                "source":source_id,"video":want_video,"audio":want_audio,
                                "elapsed_ms":source_started.elapsed().as_secs_f64()*1000.
                            }),
                        );
                        gst::PadProbeReturn::Remove
                    });
                }
                None
            });
        }
        // The bounded range source makes native WebM pull/index reads cheap.
        // Force push only for legacy/unknown network URLs without that support;
        // push mode replays early audio clusters on each small forward seek.
        let range_backed = indexed_uri.starts_with("mochunehttps:");
        crate::diagnostics::record("stream_container_scheduling", serde_json::json!({
            "source":source.name().to_string(),"video":want_video,"audio":want_audio,
            "range_backed":range_backed,"recording_audio":capture.is_some(),"legacy_audio_push":!want_video && want_audio && !range_backed
                && (uri.starts_with("http://") || uri.starts_with("https://"))
        }));
        if !range_backed && !want_video && want_audio && (uri.starts_with("http://") || uri.starts_with("https://"))
        {
            if let Some(bin) = source.downcast_ref::<gst::Bin>() {
                bin.connect_deep_element_added(|_, _, element| {
                    if element
                        .factory()
                        .is_some_and(|f| f.name() == "matroskademux")
                        && let Some(pad) = element.static_pad("sink")
                    {
                        pad.add_probe(gst::PadProbeType::QUERY_UPSTREAM, |_, info| {
                            if let Some(query) = info.query_mut()
                                && let gst::QueryViewMut::Scheduling(q) = query.view_mut()
                            {
                                q.set(gst::SchedulingFlags::SEQUENTIAL, 1, -1, 0);
                                q.add_scheduling_modes(&[gst::PadMode::Push]);
                                return gst::PadProbeReturn::Handled;
                            }
                            gst::PadProbeReturn::Ok
                        });
                    }
                });
            }
        }

        // Raw GPU frames are already decoded. Plain video/x-raw here makes decodebin
        // search for another decoder and report a misleading missing-plugin error.
        source.set_property(
            "caps",
            decoded_caps(want_video, want_audio).parse::<gst::Caps>()?,
        );
        source.connect("autoplug-continue", false, move |args| {
            let keep = args[2]
                .get::<gst::Caps>()
                .ok()
                .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
                .is_none_or(|name| {
                    (!name.starts_with("video/")
                        || want_video
                        || matches!(
                            name.as_str(),
                            "video/quicktime" | "video/webm" | "video/x-matroska" | "video/mpegts"
                        ))
                        && (!name.starts_with("audio/") || want_audio)
                });
            // Container types must still be demuxed. Rejecting elementary video
            // in audio-only mode exposes it unlinked before any video decoder.
            Some(keep.to_value())
        });
        if !headers.is_empty() {
            let headers: Vec<(String, String)> = headers
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            source.connect("source-setup", false, move |args| {
                if let Ok(source) = args[1].get::<gst::Element>() {
                    let mut extra = gst::Structure::new_empty("headers");
                    for (key, value) in &headers {
                        if key.eq_ignore_ascii_case("user-agent") {
                            if source.find_property("user-agent").is_some() {
                                source.set_property("user-agent", value);
                            }
                        } else {
                            extra.set(key.as_str(), value.as_str());
                        }
                    }
                    if extra.n_fields() > 0 && source.find_property("extra-headers").is_some() {
                        source.set_property("extra-headers", extra);
                    }
                }
                None
            });
        }
        if let Some(capture) = capture {
            source.connect("source-setup", false, move |args| {
                if let Ok(element) = args[1].get::<gst::Element>()
                    && let Some(pad) = element.static_pad("src")
                {
                    let capture = capture.clone();
                    let source = element.downgrade();
                    pad.add_probe(
                        gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
                        move |_, info| {
                            if let Some(buffer) = info.buffer() {
                                let mut capture = capture.lock().unwrap();
                                let total = if capture.needs_size() {
                                    source
                                        .upgrade()
                                        .and_then(|s| s.query_duration::<gst::format::Bytes>())
                                        .map(|n| *n)
                                } else {
                                    None
                                };
                                match buffer.map_readable() {
                                    Ok(bytes) => {
                                        capture.append(buffer.offset(), bytes.as_slice(), total)
                                    }
                                    Err(_) => capture.abort(),
                                }
                            } else if let Some(event) = info.event()
                                && matches!(event.view(), gst::EventView::Eos(_))
                            {
                                capture.lock().unwrap().finish();
                            }
                            gst::PadProbeReturn::Ok
                        },
                    );
                }
                None
            });
        }
        source.connect_pad_added(move |source, pad| {
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            crate::diagnostics::record("decoded_pad", serde_json::json!({"caps":caps.to_string(),"source":source.name().as_str(),"elapsed_ms":source_started.elapsed().as_secs_f64()*1000.}));
            let name = caps.structure(0).map(|s| s.name().as_str()).unwrap_or("");
            let target = if name.starts_with("video/") {
                video.as_ref()
            } else if name.starts_with("audio/") {
                audio.as_ref()
            } else {
                None
            };
            if let Some(target) = target
                && let Some(sink) = target.static_pad("sink")
                && !sink.is_linked()
            {
                if let Err(error) = pad.link(&sink) {
                    gst::element_error!(
                        source,
                        gst::CoreError::Negotiation,
                        ("Cannot connect decoded {name} to the player: {error}")
                    );
                }
            }
        });
        self.pipeline.add(&source)?;
        self.transient.push(source);
        Ok(())
    }
    pub fn stop(&mut self) -> Result<()> {
        // Restore the exact previous sink properties before returning to YouTube/Music.
        if let Some((late,qos))=self.source_hls.sink_defaults.take(){if let Some(late)=late{self.video_sink.set_property("max-lateness",late);}if let Some(qos)=qos{self.video_sink.set_property("qos",qos);}}
        self.source_hls.reset(false);
        #[cfg(target_os = "windows")]
        if let Some(remote) = self.remote.as_mut() {
            remote.stop()?;
        }
        self.presentation.store(false, Ordering::Release);
        LAST_VIDEO_PTS.store(0, Ordering::Release);
        self.pipeline.set_state(gst::State::Null)?;
        // The native bus is stopped now. Its forwarded EOS/state/error events
        // belong to the old session and must not be applied to a new Load.
        while self.messages.try_recv().is_ok() {}
        self.frames.lock().unwrap().clear(false);
        self.evidence.reset();
        // State Null is synchronous; release old decode branches before reusing the GPU sink.
        for element in self.transient.drain(..) {
            let _ = self.pipeline.remove(&element);
        }
        self.recovery.cancel();
        self.recovery = rebuffer::Recovery::default();
        *self.buffer_state.lock().unwrap() = buffered::Snapshot::default();
        self.volume = None;
        self.cache_capture = None;
        self.pending_seek = None;
        *self.seek_progress.lock().unwrap() = SeekProgress::default();
        #[cfg(any(target_os = "windows", test))]
        {
            self.seek_watch = None;
        }
        self.queued_seek = None;
        while self.messages.try_recv().is_ok() {}
        #[cfg(target_os = "windows")]
        if self.remote.is_none()
            && let Some(native) = &self.native
        {
            if self.video_sink.parent().is_some() {
                self.pipeline.remove(&self.video_sink)?;
            }
            self.video_sink = gst::ElementFactory::make("fakesink").build()?;
            let (pipeline, messages) = native_pipeline(self.decoder.clone(), self.evidence.clone());
            self.pipeline = pipeline;
            self.messages = messages;
            native.stop();
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    pub fn native_timer(&self, app: &SetupWindow) -> Option<slint::Timer> {
        self.native
            .as_ref()
            .map(|n| n.timer(app, self.ready.clone()))
    }
    pub fn current_state(&self) -> gst::State {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            return if r.status().paused {
                gst::State::Paused
            } else {
                gst::State::Playing
            };
        }
        self.pipeline.current_state()
    }
    pub fn position(&self) -> Option<gst::ClockTime> {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            return r
                .status()
                .position
                .filter(|s| s.is_finite() && *s >= 0.)
                .map(gst::ClockTime::from_seconds_f64);
        }
        self.pipeline.query_position::<gst::ClockTime>()
    }
    pub fn duration(&self) -> Option<gst::ClockTime> {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            return r
                .status()
                .duration
                .filter(|s| s.is_finite() && *s >= 0.)
                .map(gst::ClockTime::from_seconds_f64);
        }
        self.pipeline.query_duration::<gst::ClockTime>()
    }
    pub(crate) fn observe_source_buffering(&mut self,id:usize,percent:i32){self.source_hls.buffering(id,percent);}
    pub fn poll_rebuffer(&mut self, active: bool, paused: bool) -> Result<bool> {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            return Ok(r.status().rebuffering);
        }
        let position = self.position().map(|t| t.seconds_f64());
        let duration = self.duration().map(|t| t.seconds_f64());
        if self.source_hls.enabled {
            let action=self.source_hls.step(active,paused,self.seeking(),video_position(),position,duration,std::time::Instant::now());
            match action {
                source_hls::Action::Pause=>{self.pipeline.set_state(gst::State::Paused)?;crate::diagnostics::record("source_hls_refill_started",serde_json::json!({"position":position,"video_pts":video_position()}));}
                source_hls::Action::Resume=>{self.pipeline.set_state(gst::State::Playing)?;crate::diagnostics::record("source_hls_refill_finished",serde_json::json!({"position":position,"video_pts":video_position()}));}
                source_hls::Action::None=>{}
            }
            return Ok(self.source_hls.waiting());
        }
        // Refill only after measured video starvation/clock drift, never on a
        // queue percentage alone. Startup keeps its small initial threshold.
        self.recovery.observe_only = false;
        if active && !paused && self.seek_progress.lock().unwrap().video_pts.is_some() {
            self.recovery.expand_after_picture();
        }
        let waiting = self.recovery.poll(
            &self.pipeline,
            &self.transient,
            active,
            paused,
            self.pending_seek.is_some(),
            {
                let progress = self.seek_progress.lock().unwrap();
                let pts = video_position();
                let end = progress
                    .video_pts
                    .zip(progress.video_duration)
                    .map(|(p, d)| (p + d).seconds_f64())
                    .unwrap_or(pts);
                if position.is_some_and(|p| p < end) {
                    position.unwrap().max(pts)
                } else {
                    pts
                }
            },
            position,
            duration,
        )?;
        if let Some(target) = self.recovery.resync.take() {
            crate::diagnostics::record(
                "playback_refill_resync",
                serde_json::json!({"target":target}),
            );
            self.seek_with_state(target, false)?;
        }
        Ok(waiting || self.pending_seek.is_some())
    }
    /// Fractions of the current quality's retained compressed data only.
    pub fn buffered_ranges(&self) -> (Vec<(f64, f64)>, bool) {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            let status = r.status();
            return (status.buffered_ranges, status.buffered_estimated);
        }
        let mut state = self.buffer_state.lock().unwrap();
        state.refresh(&self.transient, self.duration());
        (state.ranges.clone(), state.estimated)
    }
    pub fn manages_start(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            return self.remote.is_some();
        }
        #[cfg(not(target_os = "windows"))]
        false
    }
    pub fn confirmed_buffered_ranges(&self) -> Vec<(f64, f64)> {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            return r.status().buffered_confirmed;
        }
        self.buffer_state.lock().unwrap().confirmed.clone()
    }
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn poll_start(&mut self, start: &mut playback_state::Start) -> Result<bool> {
        let (result, current, pending) = self.pipeline.state(gst::ClockTime::ZERO);
        let settled = result.is_ok_and(|r| r != gst::StateChangeSuccess::Async)
            && pending == gst::State::VoidPending;
        if !start.seek_sent && settled && current == gst::State::Paused {
            start.seek_sent = true;
            start.position = playback_state::seek_target(
                start.position,
                self.duration().map(|d| d.seconds_f64()),
            )
            .unwrap_or(0.);
            if start.position > 0. {
                self.seek_with_state(start.position, start.paused)?;
            } else {
                self.set_paused(start.paused)?;
            }
            return Ok(false);
        }
        Ok(start.seek_sent
            && !self.seeking()
            && settled
            && current
                == if start.paused {
                    gst::State::Paused
                } else {
                    gst::State::Playing
                })
    }
    pub fn video_process_released(&self) -> bool {
        #[cfg(target_os = "windows")]
        if let Some(r) = &self.remote {
            return !r.active();
        }
        false
    }
    pub fn release_video_process(&mut self) {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_mut() {
            r.shutdown();
        }
    }
    pub fn next_message(&mut self) -> Option<gst::Message> {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            *self.decoder.lock().unwrap() = r.status().decoder;
            if let Some(event) = r.next_event() {
                use process_protocol::Event;
                return Some(match event {
                    Event::State { playing, .. } => gst::message::StateChanged::builder(
                        gst::State::Ready,
                        if playing {
                            gst::State::Playing
                        } else {
                            gst::State::Paused
                        },
                        gst::State::VoidPending,
                    )
                    .src(&self.pipeline)
                    .build(),
                    Event::Buffering { percent, .. } => gst::message::Buffering::builder(percent)
                        .src(&self.pipeline)
                        .build(),
                    // Startup completion is separate from transient preroll Playing events.
                    Event::Started { .. } => gst::message::StateChanged::builder(
                        gst::State::Paused,
                        gst::State::Playing,
                        gst::State::VoidPending,
                    )
                    .src(&self.pipeline)
                    .build(),
                    Event::Eos { .. } => gst::message::Eos::builder().src(&self.pipeline).build(),
                    Event::Error {
                        message,
                        debug,
                        element,
                        ..
                    } => gst::message::Error::builder(gst::CoreError::Failed, &message)
                        .debug(&format!("{element}: {debug}"))
                        .src(&self.pipeline)
                        .build(),
                    _ => unreachable!(),
                });
            }
        }
        self.messages.try_recv().ok()
    }
    pub fn approve_audio_cache(&self) {
        if let Some(c) = &self.cache_capture {
            c.lock().unwrap().approve();
        }
    }
    pub fn acceleration_report(&self, active: bool, music: bool) -> acceleration::Report {
        #[cfg(target_os = "windows")]
        if active
            && !music
            && let Some(r) = self.remote.as_ref().filter(|r| r.active())
        {
            let status = r.status();
            return acceleration::Report {
                verified: status.verified,
                status: status.acceleration_status,
                details: status.acceleration_details,
            };
        }
        self.evidence.report(
            active,
            music,
            self.pipeline.current_state() == gst::State::Paused,
            &self.memory_mode(),
        )
    }
    pub fn memory_mode(&self) -> String {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            return r.status().memory;
        }

        self.video_sink
            .static_pad("sink")
            .and_then(|p| p.current_caps())
            .and_then(|c| c.features(0).map(|f| f.to_string()))
            .unwrap_or_else(|| "pending".into())
    }
    fn remote_active(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            self.remote.as_ref().is_some_and(|r| r.active())
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }
    pub fn seek_with_state(&mut self, seconds: f64, paused: bool) -> Result<()> {
        let Some(seconds) =
            playback_state::seek_target(seconds, self.duration().map(|d| d.seconds_f64()))
        else {
            return Ok(());
        };
        // Stop/mode-switch may race a queued input message. Do not seek a NULL
        // local pipeline after the remote worker has already been released.
        #[cfg(target_os = "windows")]
        if self.presentation.load(Ordering::Acquire)
            && self.remote.as_ref().is_some_and(|r| !r.active())
        {
            return Ok(());
        }
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_mut().filter(|r| r.active()) {
            return r.seek(seconds, paused);
        }

        if let Some(seek) = self.pending_seek.as_mut() {
            seek.resume = !paused;
            self.queued_seek = Some(seconds);
            return Ok(());
        }
        if self.pipeline.current_state() < gst::State::Paused && !self.remote_active() {
            return Ok(());
        }
        self.begin_seek(seconds, !paused, false)
    }
    fn begin_seek(&mut self, seconds: f64, resume: bool, recovered: bool) -> Result<()> {
        self.recovery.cancel();
        // Freeze the shared A/V clock while an evicted target reprerolls. Audio
        // must not race ahead while the decoder rebuilds reference pictures.
        if self.pipeline.current_state() == gst::State::Playing {
            self.frames.lock().unwrap().clear(true);
            self.pipeline.set_state(gst::State::Paused)?;
        }
        self.dispatch_seek(seconds, resume, recovered)
    }
    fn dispatch_seek(&mut self, seconds: f64, resume: bool, recovered: bool) -> Result<()> {
        let target = if seconds.is_finite() {
            seconds.max(0.)
        } else {
            0.
        };
        // Freeze source consumption before observing retention. If a backward
        // keyframe was overwritten, native queue2 may otherwise return bytes
        // from the next ring lap instead of fetching it. Reopen that miss once.
        self.buffer_state.lock().unwrap().prepare_seek();
        let backwards = self
            .position()
            .is_some_and(|p| target + 0.1 < p.seconds_f64());
        let (ranges, estimated) = self.buffered_ranges();
        crate::diagnostics::record(
            "seek_buffer_snapshot",
            serde_json::json!({"target":target,"from":self.position().map(|t|t.seconds_f64()),"ranges":ranges,"estimated":estimated,"buffers":self.recovery.budgets()}),
        );
        let assessment = self.buffer_state.lock().unwrap().assess_seek(target);
        let restart = !recovered
            && self.presentation.load(Ordering::Acquire)
            && assessment.unsafe_native_claim;
        crate::diagnostics::record(
            "seek_plan",
            serde_json::json!({"target":target,"backwards":backwards,
            "video_retention":assessment,"action":if restart {"restart_unsafe_native_cache"} else if assessment.retained==Some(true) {"reuse_video_cache"} else if assessment.retained==Some(false) {"native_fetch_missing_data"} else {"native_seek_unobserved"},
            "audio_retention":"independent","estimated_display":estimated}),
        );
        if restart {
            crate::diagnostics::record(
                "seek_evicted_keyframe",
                serde_json::json!({"target":target,"action":"reopen_source_before_decode"}),
            );
            return self.restart_seek(target, resume);
        }
        let event = gst::event::Seek::new(
            1.,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::ClockTime::from_mseconds((target * 1000.) as u64),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
        let seq = event.seqnum();
        self.seek_progress.lock().unwrap().begin(seq, target);
        self.frames.lock().unwrap().clear(true);
        #[cfg(any(target_os = "windows", test))]
        {
            self.seek_watch = None;
        }
        self.pending_seek = Some(PendingSeek {
            target,
            resume,
            started: std::time::Instant::now(),
            repreroll: false,
            await_pause: false,
            recovered,
        });
        crate::diagnostics::record(
            "seek_started",
            serde_json::json!({"target":target,"resume":resume,"seq":format!("{seq:?}"),"recovered":recovered}),
        );
        if !self.pipeline.send_event(event) {
            self.cancel_seek();
            bail!("The stream rejected seeking");
        }
        Ok(())
    }
    fn cancel_seek(&mut self) {
        self.pending_seek = None;
        self.queued_seek = None;
        *self.seek_progress.lock().unwrap() = SeekProgress::default();
        // A rejected seek must not leave the presentation mailbox permanently gated.
        self.frames.lock().unwrap().clear(false);
    }
    #[cfg(target_os = "windows")]
    pub fn seek_diagnostics(&self) -> serde_json::Value {
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            let status = r.status();
            return serde_json::json!({"process_seeking":status.seeking,"worker":status.seek_details});
        }

        let progress = self.seek_progress.lock().unwrap();
        serde_json::json!({
            "target":self.pending_seek.as_ref().map(|s|s.target),
            "elapsed_ms":self.pending_seek.as_ref().map(|s|s.started.elapsed().as_millis()),
            "repreroll":self.pending_seek.as_ref().is_some_and(|s|s.repreroll),
            "await_pause":self.pending_seek.as_ref().is_some_and(|s|s.await_pause),
            "recovered":self.pending_seek.as_ref().is_some_and(|s|s.recovered),
            "segment_mask":progress.segments,"buffer_mask":progress.buffers,"target_seen":progress.target_seen,
            "decoded_video_pts":progress.video_pts.map(|t|t.seconds_f64()),
            "presentation_flushing":self.frames.lock().unwrap().flushing,
            "queued_target":self.queued_seek,
        })
    }
    pub fn seeking(&self) -> bool {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            return r.status().seeking;
        }

        self.pending_seek.is_some()
    }
    pub fn set_paused(&mut self, paused: bool) -> Result<()> {
        self.recovery.cancel();
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            return r.send(process_protocol::Request::Pause(paused));
        }
        if let Some(seek) = self.pending_seek.as_mut() {
            seek.resume = !paused;
        } else {
            self.pipeline.set_state(if paused {
                gst::State::Paused
            } else {
                gst::State::Playing
            })?;
        }
        Ok(())
    }
    pub fn keep_paused(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            return r.send(process_protocol::Request::Pause(true));
        }

        if let Some(seek) = self.pending_seek.as_mut() {
            seek.resume = false;
        }
        self.pipeline.set_state(gst::State::Paused)?;
        Ok(())
    }
    fn restart_seek(&mut self, target: f64, resume: bool) -> Result<()> {
        crate::diagnostics::record(
            "seek_decoder_restart",
            serde_json::json!({"target":target,"resume":resume,"video_pts":video_position(),"decoder":self.decoder.lock().unwrap().clone()}),
        );
        #[cfg(any(target_os = "windows", test))]
        {
            self.seek_watch = None;
        }
        self.frames.lock().unwrap().clear(true);
        *self.seek_progress.lock().unwrap() = SeekProgress::default();
        // READY closes the old demux/decoder session and its reference pictures.
        // Keep URI/header configuration and the shared GPU device; no resolver,
        // CPU frame transfer or second pipeline is involved.
        if let Err(error) = self.pipeline.set_state(gst::State::Ready) {
            self.cancel_seek();
            return Err(error.into());
        }
        self.buffer_state.lock().unwrap().discard_retention();
        self.recovery.pipeline_rebuilt();
        self.pending_seek = Some(PendingSeek {
            target,
            resume,
            started: std::time::Instant::now(),
            repreroll: true,
            await_pause: false,
            recovered: true,
        });
        if let Err(error) = self.pipeline.set_state(gst::State::Paused) {
            self.cancel_seek();
            return Err(error.into());
        }
        Ok(())
    }
    pub fn poll_seek(&mut self) -> Result<bool> {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_mut().filter(|r| r.active()) {
            return Ok(r.poll_seek());
        }

        if self.pending_seek.is_none() {
            #[cfg(any(target_os = "windows", test))]
            if self.seek_watch.is_none() {
                return Ok(false);
            }
            #[cfg(not(any(target_os = "windows", test)))]
            return Ok(false);
        }
        let (state_result, current_state, pending_state) =
            self.pipeline.state(gst::ClockTime::ZERO);
        let settled = state_result.is_ok_and(|s| s != gst::StateChangeSuccess::Async)
            && pending_state == gst::State::VoidPending;
        if let Some(seek) = self.pending_seek.as_ref() {
            if seek.await_pause && settled && current_state == gst::State::Paused {
                let seek = self.pending_seek.take().unwrap();
                let target = self.queued_seek.take().unwrap_or(seek.target);
                self.dispatch_seek(target, seek.resume, seek.recovered)?;
                return Ok(false);
            }
            if seek.repreroll && settled {
                let seek = self.pending_seek.take().unwrap();
                let target = self.queued_seek.take().unwrap_or(seek.target);
                self.begin_seek(target, seek.resume, true)?;
                return Ok(false);
            }
            let required = if self.presentation.load(Ordering::Acquire) {
                3
            } else {
                1
            };
            let playing_position = (seek.resume && current_state == gst::State::Playing)
                .then(|| self.pipeline.query_position::<gst::ClockTime>())
                .flatten();
            let readiness = {
                let progress = self.seek_progress.lock().unwrap();
                if progress.ready_for_target(required) {
                    Some("target_frame")
                } else if progress.ready_at_playing_clock(
                    required,
                    playing_position,
                    seek.started.elapsed(),
                ) {
                    Some("current_segment_clock")
                } else {
                    None
                }
            };
            if !seek.repreroll && !seek.await_pause && settled && readiness.is_some() {
                let seek = self.pending_seek.take().unwrap();
                if let Some(target) = self.queued_seek.take() {
                    self.begin_seek(target, seek.resume, false)?;
                    return Ok(false);
                }
                self.pipeline.set_state(if seek.resume {
                    gst::State::Playing
                } else {
                    gst::State::Paused
                })?;
                crate::diagnostics::record(
                    "seek_completed",
                    serde_json::json!({"target":seek.target,"elapsed_ms":seek.started.elapsed().as_millis(),"recovered":seek.recovered,"readiness":readiness,"stages":self.seek_progress.lock().unwrap().stages()}),
                );
                self.seek_progress.lock().unwrap().expected = None;
                self.frames.lock().unwrap().flushing = false;
                #[cfg(any(target_os = "windows", test))]
                if self.presentation.load(Ordering::Acquire) {
                    self.seek_watch = Some(SeekWatch {
                        target: seek.target,
                        started: std::time::Instant::now(),
                        recovered: seek.recovered,
                        last_video_pts: self.seek_progress.lock().unwrap().video_pts,
                    });
                }
                return Ok(true);
            }
            // Observe both HTTP inputs. A new video SEGMENT does not mean its
            // decoder has enough data, and a slow audio request must not cause a
            // video-only timeout/restart. Keep a hard upper bound for bad media.
            let source_active = self
                .recovery
                .input_recently_active(std::time::Duration::from_secs(12));
            #[cfg(any(target_os = "windows", test))]
            if !seek.recovered && seek.started.elapsed().as_secs() >= 15 && !source_active {
                let target = self.queued_seek.take().unwrap_or(seek.target);
                let resume = seek.resume;
                self.restart_seek(target, resume)?;
                return Ok(false);
            }
            if seek.started.elapsed().as_secs() >= 90
                || (seek.started.elapsed().as_secs() >= 20 && !source_active)
            {
                self.cancel_seek();
                bail!("Seeking timed out. Try another quality or retry playback.");
            }
        }
        #[cfg(any(target_os = "windows", test))]
        if let Some(watch) = self.seek_watch.as_mut() {
            if self.pipeline.current_state() == gst::State::Paused {
                watch.started = std::time::Instant::now();
            } else if let Some(position) = self.pipeline.query_position::<gst::ClockTime>() {
                let position = position.seconds_f64();
                // Watch decoder-side output, not UI redraws: a minimized window
                // legitimately stops presenting while playback continues.
                let video_pts = if self.video_sink.find_property("last-sample").is_some() {
                    self.video_sink
                        .property::<Option<gst::Sample>>("last-sample")
                        .and_then(|sample| sample.buffer().and_then(|b| b.pts()))
                } else {
                    self.seek_progress.lock().unwrap().video_pts
                };
                if video_pts.is_some_and(|pts| watch.last_video_pts.is_none_or(|old| pts > old)) {
                    watch.last_video_pts = video_pts;
                    watch.started = std::time::Instant::now();
                }
                if video_pts.is_some_and(|pts| (pts.seconds_f64() - position).abs() < 0.75)
                    && position >= watch.target + 3.
                {
                    self.seek_watch = None;
                } else if watch.started.elapsed().as_secs() >= 5 {
                    let recovered = watch.recovered;
                    self.seek_watch = None;
                    if !recovered {
                        self.restart_seek(position, true)?;
                    } else {
                        bail!("The GPU decoder stalled after seeking. Try another quality.");
                    }
                }
            }
        }
        Ok(false)
    }
    pub fn set_volume(&self, value: f64) {
        #[cfg(target_os = "windows")]
        if let Some(r) = self.remote.as_ref().filter(|r| r.active()) {
            let _ = r.send(process_protocol::Request::Volume(value));
            return;
        }

        if let Some(gain) = &self.volume {
            gain.set_property("volume", linear_volume(value));
        }
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.release_video_process();
        let _ = self.pipeline.set_state(gst::State::Null);
        #[cfg(target_os = "windows")]
        if let Some(native) = &self.native {
            native.stop();
        }
    }
}

fn observe_decoders(
    pipeline: &gst::Pipeline,
    decoder: Arc<Mutex<String>>,
    evidence: Arc<acceleration::Evidence>,
) {
    let d = decoder.clone();
    pipeline.connect_deep_element_added(move |_, _, element| {
        if let Some(factory) = element.factory()
            && factory
                .metadata("klass")
                .is_some_and(|k| k.contains("Decoder") && k.contains("Video"))
        {
            *d.lock().unwrap() = factory.name().to_string();
            if let Some(pad) = element.static_pad("src") {
                let proof = evidence.clone();
                let name = factory.name().to_string();
                let hardware = factory.metadata("klass").is_some_and(|k| k.contains("Hardware"));
                let observed = AtomicBool::new(false);
                pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
                    if let Some(buffer) = info.buffer() {
                        if !observed.swap(true, Ordering::Relaxed) {
                            let memory = if buffer.n_memory() > 0 {
                                buffer.peek_memory(0).allocator().map(|a| a.memory_type().to_string()).unwrap_or_else(|| "unknown".into())
                            } else { "unknown".into() };
                            proof.record_decoder(&name, hardware, &memory);
                            crate::diagnostics::record("decoder_verified", serde_json::json!({"decoder":name,"hardware":hardware,"memory":memory,"caps":pad.current_caps().map(|c| c.to_string()),"evidence":"actual decoded output buffer"}));
                        }
                        proof.decoded.fetch_add(1, Ordering::Relaxed);
                    }
                    gst::PadProbeReturn::Ok
                });
            }
        }
    });
}
#[cfg(target_os = "windows")]
pub(crate) fn native_windows_enabled() -> bool {
    !(cfg!(feature = "legacy-wgpu") && std::env::var("MOCHUNE_LEGACY_WGPU").as_deref() == Ok("1"))
}
pub fn install(app: &SetupWindow) -> Result<Engine> {
    gst::init()?;
    #[cfg(target_os = "linux")]
    if crate::app::linux_renderer::software_music() {
        return install_software_music();
    }
    #[cfg(target_os = "windows")]
    if native_windows_enabled() {
        return install_native(app);
    }
    #[cfg(any(not(target_os = "windows"), feature = "legacy-wgpu"))]
    return install_texture(app);
    #[cfg(all(target_os = "windows", not(feature = "legacy-wgpu")))]
    unreachable!("Native rendering is always enabled in standard Windows builds")
}
// Audio-only software UI needs neither an OpenGL sink nor a rendering notifier.
#[cfg(target_os = "linux")]
fn install_software_music() -> Result<Engine> {
    let pipeline = gst::Pipeline::new();
    let decoder = Arc::new(Mutex::new(String::new()));
    let evidence = Arc::new(acceleration::Evidence::default());
    observe_decoders(&pipeline, decoder.clone(), evidence.clone());
    let (sender, messages) = mpsc::channel();
    pipeline.bus().unwrap().set_sync_handler(move |_, message| {
        let _ = sender.send(message.to_owned());
        gst::BusSyncReply::Drop
    });
    Ok(Engine {
        pipeline, messages, decoder, evidence,
        video_sink: gst::ElementFactory::make("fakesink").property("sync", false).build()?,
        transient: Vec::new(),
        buffer_state: Mutex::new(buffered::Snapshot::default()),
        recovery: rebuffer::Recovery::default(),
            source_hls: source_hls::Gate::default(),
        ready: Arc::new(AtomicBool::new(true)),
        setup_error: Arc::new(Mutex::new(None)),
        volume: None,
        frames: Arc::new(Mutex::new(FrameMailbox::default())),
        presentation: Arc::new(AtomicBool::new(false)),
        pending_seek: None,
        seek_progress: Arc::new(Mutex::new(SeekProgress::default())),
        #[cfg(test)]
        seek_watch: None,
        queued_seek: None,
        cache_capture: None,
    })
}
#[cfg(any(not(target_os = "windows"), feature = "legacy-wgpu"))]
fn install_texture(app: &SetupWindow) -> Result<Engine> {
    let pipeline = gst::Pipeline::new();
    let mut sink = TextureSink::new()?;
    let frames = sink.next.clone();
    let presentation = Arc::new(AtomicBool::new(false));
    let render_enabled = presentation.clone();
    let video_sink = sink.glsink.clone();
    let seek_progress = Arc::new(Mutex::new(SeekProgress::default()));
    watch_seek_output(&video_sink, seek_progress.clone(), 2);
    pipeline.add(&video_sink)?;
    let decoder = Arc::new(Mutex::new(String::new()));
    let evidence = Arc::new(acceleration::Evidence::default());
    let shown_evidence = evidence.clone();
    let backends: Vec<_> =
        gst::ElementFactory::factories_with_type(gst::ElementFactoryType::DECODER, gst::Rank::NONE)
            .into_iter()
            .filter(|f| {
                f.metadata("klass")
                    .is_some_and(|k| k.contains("Video") && k.contains("Hardware"))
            })
            .map(|f| f.name().to_string())
            .take(32)
            .collect();
    app.set_hardware_support(
        if backends.is_empty() {
            "No hardware video decoder advertised by the installed drivers/plugins".to_owned()
        } else {
            format!("Driver-advertised backends: {}", backends.join(", "))
        }
        .into(),
    );
    observe_decoders(&pipeline, decoder.clone(), evidence.clone());
    let memory_decoder = decoder.clone();
    if let Some(pad) = video_sink.static_pad("sink") {
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_ref()
                && buffer.n_memory() > 0
            {
                let mut label = memory_decoder.lock().unwrap();
                if !label.contains("input buffer:") {
                    let allocator = buffer
                        .peek_memory(0)
                        .allocator()
                        .map(|a| a.memory_type().as_str())
                        .unwrap_or("unknown");
                    label.push_str("; input buffer: ");
                    label.push_str(allocator);
                }
            }
            gst::PadProbeReturn::Ok
        });
    }
    let (sender, messages) = mpsc::channel();
    let ready = Arc::new(AtomicBool::new(false));
    let connected = ready.clone();
    let setup_error = Arc::new(Mutex::new(None));
    let render_error = setup_error.clone();
    let weak = app.as_weak();
    let pipe = pipeline.clone();
    let mut count = 0i32;
    let mut draw_start = std::time::Instant::now();
    let mut presented_pts = None;
    let mut frame_error = None;
    #[cfg(target_os="linux")]
    let interpolation_timer=slint::Timer::default();
    app.window()
        .set_rendering_notifier(move |state, api| match state {
            slint::RenderingState::RenderingSetup => {
                *render_error.lock().unwrap() = None;
                if let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                    let address = get_proc_address(c"glGetString");
                    if !address.is_null() {
                        // SAFETY: OpenGL exposes this entry point while its context is current.
                        let get: unsafe extern "system" fn(u32) -> *const std::ffi::c_char =
                            unsafe { std::mem::transmute(address) };
                        let value = unsafe { get(0x1F01) };
                        if !value.is_null()
                            && let Some(w) = weak.upgrade()
                        {
                            w.set_graphics_info(
                                unsafe { std::ffi::CStr::from_ptr(value) }
                                    .to_string_lossy()
                                    .to_string()
                                    .into(),
                            );
                        }
                    }
                }
                let weak_frame = weak.clone();
                let queued = Arc::new(AtomicBool::new(false));
                let q = queued.clone();
                sink.redraw_pending = Some(queued);
                let notify = Box::new(move || {
                    if !q.swap(true, Ordering::AcqRel) {
                        let _ = weak_frame.upgrade_in_event_loop(|w| w.window().request_redraw());
                    }
                });
                match sink.connect(api, &pipe.bus().unwrap(), sender.clone(), notify) {
                    Ok(()) => {
                        connected.store(true, Ordering::Release);
                        #[cfg(target_os = "windows")]
                        if let Some(w) = weak.upgrade() {
                            w.set_graphics_info(windows_gpu::adapter_info().into());
                            w.set_hardware_support(windows_gpu::capabilities().into());
                        }
                    }
                    Err(e) => {
                        *render_error.lock().unwrap() = Some(e.to_string());
                        if let Some(w) = weak.upgrade() {
                            w.set_player_error(
                                format!("Embedded GPU initialization failed: {e}").into(),
                            );
                            w.set_playback_status(
                                format!("Embedded GPU initialization failed: {e}").into(),
                            );
                        }
                    }
                }
            }
            slint::RenderingState::BeforeRendering => {
                RENDER_PASSES.fetch_add(1, Ordering::Relaxed);
                let started = std::time::Instant::now();
                if !render_enabled.load(Ordering::Acquire) {
                    if let Some(w) = weak.upgrade() {
                        w.set_video_visible(false);
                        w.set_video_frame(Default::default());
                    }
                    sink.clear();
                }
                if render_enabled.load(Ordering::Acquire) {
                    #[cfg(target_os="linux")]
                    let image={
                        let mode=weak.upgrade().map_or(0,|w|{
                            sink.auto_fallback=w.get_interpolation_auto_fallback();
                            if w.get_paused()||pipe.current_state()!=gst::State::Playing{0}else{w.get_interpolation_mode()}
                        });
                        sink.next_image(mode)
                    };
                    #[cfg(not(target_os="linux"))]
                    let image=sink.next_image();
                    match image {
                        Ok(Some((image, changed, pts))) => {
                            LAST_VIDEO_PTS.store(pts, Ordering::Release);
                            presented_pts = Some(pts);
                            count = count.saturating_add(1);
                            if let Some(w) = weak.upgrade() {
                                if frame_error.take().is_some() {
                                    w.set_player_error("".into());
                                }
                                if changed || !w.get_video_visible() {
                                    w.set_video_frame(image);
                                    w.set_video_visible(true);
                                }
                                #[cfg(not(target_os = "windows"))]
                                w.set_video_display_aspect(sink.display_aspect);
                                w.set_video_frames(count);
                            }
                        }
                        Err(error) => {
                            let text = format!("Video display failed: {error:#}");
                            if frame_error.as_ref() != Some(&text) {
                                gst::element_error!(pipe, gst::CoreError::Failed, ("{text}"));
                                if let Some(w) = weak.upgrade() {
                                    w.set_player_error(text.clone().into());
                                }
                                frame_error = Some(text);
                            }
                        }
                        Ok(None) => {}
                    }
                }
                #[cfg(target_os="linux")]
                {
                    if let Some(w)=weak.upgrade(){
                        if w.get_video_source_fps()!=sink.source_fps {w.set_video_source_fps(sink.source_fps);}
                        if sink.blocked.is_some() && w.get_interpolation_mode()>0 {
                            w.set_interpolation_mode(0);
                            let weak=w.as_weak();
                            let _=slint::invoke_from_event_loop(move||{if let Some(w)=weak.upgrade(){
                                if w.get_interpolation_mode()==0 {w.invoke_change_interpolation(0);}
                            }});
                        }
                        if w.get_interpolation_status().as_str()!=sink.effect_status {w.set_interpolation_status(sink.effect_status.clone().into());}
                        let pending={let mut mailbox=sink.next.lock().unwrap();mailbox.timed=sink.motion.is_some();!mailbox.timed&&mailbox.next.is_some()};
                        if pending {w.window().request_redraw();}
                        if let Some(worker)=sink.motion.as_ref(){
                            let weak=w.as_weak();interpolation_timer.start(slint::TimerMode::SingleShot,worker.delay(),move||{if let Some(w)=weak.upgrade(){w.window().request_redraw();}});
                        }else{interpolation_timer.stop();}
                    }
                }
                IMPORT_MICROS.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                draw_start = std::time::Instant::now();
            }
            slint::RenderingState::AfterRendering => {
                if let Some(pts) = presented_pts.take() {
                    record_presented_frame(pts);
                    shown_evidence.presented.fetch_add(1, Ordering::Relaxed);
                }
                DRAW_MICROS.fetch_add(draw_start.elapsed().as_micros() as u64, Ordering::Relaxed);
            }
            slint::RenderingState::RenderingTeardown => {
                connected.store(false, Ordering::Release);
                let _ = pipe.set_state(gst::State::Null);
                // Window properties cannot be changed while Winit tears its renderer down.
                sink.release();
            }
            _ => {}
        })
        .map_err(|e| anyhow::anyhow!("Cannot attach the embedded video renderer: {e}"))?;
    Ok(Engine {
        pipeline,
        messages,
        decoder,
        evidence,
        video_sink,
        transient: Vec::new(),
        buffer_state: Mutex::new(buffered::Snapshot::default()),
        recovery: rebuffer::Recovery::default(),
            source_hls: source_hls::Gate::default(),
        ready,
        setup_error,
        volume: None,
        frames,
        presentation,
        pending_seek: None,
        seek_progress,
        #[cfg(target_os = "windows")]
        native: None,
        #[cfg(target_os = "windows")]
        remote: None,
        #[cfg(any(target_os = "windows", test))]
        seek_watch: None,
        queued_seek: None,
        cache_capture: None,
    })
}

#[cfg(not(target_os = "windows"))]
struct TextureSink {
    appsink: gst_app::AppSink,
    glsink: gst::Element,
    next: Arc<Mutex<FrameMailbox>>,
    current: Option<gst_gl::GLVideoFrame<gst_gl::gl_video_frame::Readable>>,
    context: Option<gst_gl::GLContext>,
    redraw_pending: Option<Arc<AtomicBool>>,
    surface: Option<Surface>,
    display_aspect: f32,
    source_fps: f32,
    motion:Option<interpolation::worker::Worker>,
    motion_notify:Option<Arc<Mutex<Box<dyn Fn()+Send>>>>,
    current_pts:Option<u64>,
    generation:u64,
    requested:i32,
    blocked:Option<String>,
    effect_status:String,
    early_offset:i64,
    auto_fallback:bool,
}
#[cfg(not(target_os = "windows"))]
impl TextureSink {
    fn new() -> Result<Self> {
        let caps = gst_video::VideoCapsBuilder::new()
            .features([gst_gl::CAPS_FEATURE_MEMORY_GL_MEMORY])
            .format(gst_video::VideoFormat::Rgba)
            .field("texture-target", "2D")
                        .build();
        let appsink = gst_app::AppSink::builder()
            .caps(&caps)
            .enable_last_sample(false)
            .max_buffers(1)
            .drop(true)
            .async_(true)
            .build();
        let glsink = gst::ElementFactory::make("glsinkbin")
            .property("sink", &appsink)
            .build()
            .context("The GStreamer OpenGL plugin is missing")?;
        let next = Arc::new(Mutex::new(FrameMailbox::default()));
        let mailbox = next.clone();
        appsink.static_pad("sink").unwrap().add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::EVENT_FLUSH,
            move |_, info| {
                if let Some(gst::PadProbeData::Event(event)) = info.data.as_ref() {
                    match event.view() {
                        gst::EventView::FlushStart(_) => mailbox.lock().unwrap().clear(true),
                        gst::EventView::FlushStop(_) => mailbox.lock().unwrap().flushing = false,
                        _ => {}
                    }
                }
                gst::PadProbeReturn::Ok
            },
        );
        Ok(Self {
            appsink,
            glsink,
            next,
            current: None,
            context: None,
            redraw_pending: None,
            surface: None,
            display_aspect: 0.,
            source_fps: 0.,
            motion:None,motion_notify:None,current_pts:None,generation:0,requested:0,blocked:None,effect_status:"Off".into(),early_offset:0,auto_fallback:true,
        })
    }
    fn connect(
        &mut self,
        api: &slint::GraphicsAPI<'_>,
        bus: &gst::Bus,
        sender: mpsc::Sender<gst::Message>,
        notify: Box<dyn Fn() + Send>,
    ) -> Result<()> {
        let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
            bail!("An OpenGL renderer is required");
        };
        // SAFETY: loader entry points belong to the current Slint OpenGL context.
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                get_proc_address(&std::ffi::CString::new(name).unwrap())
            })
        };
        crate::diagnostics::record(
            "graphics",
            serde_json::json!({"api":"OpenGL", "vendor":unsafe {gl.get_parameter_string(glow::VENDOR)}, "renderer":unsafe {gl.get_parameter_string(glow::RENDERER)}, "version":unsafe {gl.get_parameter_string(glow::VERSION)}}),
        );
        self.surface = Some(Surface::new(gl)?);
        #[cfg(target_os = "linux")]
        let (context, display, platform) = {
            // Slint prefers GLX on X11 and EGL on Wayland. Query the current
            // context, rather than guessing from session environment variables:
            // XWayland and native X11 may both be used from a Wayland session.
            let glx_context = gst_gl::GLContext::current_gl_context(gst_gl::GLPlatform::GLX);
            if glx_context != 0 {
                let glx = glutin_glx_sys::glx::Glx::load_with(|name| {
                    get_proc_address(&std::ffi::CString::new(name).unwrap())
                });
                anyhow::ensure!(
                    glx.GetCurrentDisplay.is_loaded(),
                    "GLX display function unavailable"
                );
                // SAFETY: RenderingSetup runs on Slint's current GL thread.
                // The X display remains owned by Slint; GStreamer wraps it without closing it.
                unsafe {
                    let native_display = glx.GetCurrentDisplay();
                    anyhow::ensure!(!native_display.is_null(), "No current GLX display");
                    let display = gst_gl_x11::GLDisplayX11::with_display(native_display as usize)
                        .context("Cannot share Slint's X11 display")?;
                    let context = gst_gl::GLContext::new_wrapped(
                        &display,
                        glx_context,
                        gst_gl::GLPlatform::GLX,
                        gst_gl::GLContext::current_gl_api(gst_gl::GLPlatform::GLX).0,
                    )
                    .context("Cannot wrap Slint's GLX context")?;
                    (context, display.upcast::<gst_gl::GLDisplay>(), "GLX")
                }
            } else {
                let egl = glutin_egl_sys::egl::Egl::load_with(|name| {
                    get_proc_address(&std::ffi::CString::new(name).unwrap())
                });
                anyhow::ensure!(
                    egl.GetCurrentDisplay.is_loaded() && egl.GetCurrentContext.is_loaded(),
                    "No current GLX context or available EGL context functions"
                );
                // SAFETY: Slint invokes RenderingSetup with its EGL context current.
                unsafe {
                    let native_display = egl.GetCurrentDisplay();
                    let native_context = egl.GetCurrentContext();
                    anyhow::ensure!(
                        !native_display.is_null() && !native_context.is_null(),
                        "No current EGL display/context"
                    );
                    let display =
                        gst_gl_egl::GLDisplayEGL::with_egl_display(native_display as usize)
                            .context("Cannot share Slint's EGL display")?;
                    let context = gst_gl::GLContext::new_wrapped(
                        &display,
                        native_context as usize,
                        gst_gl::GLPlatform::EGL,
                        gst_gl::GLContext::current_gl_api(gst_gl::GLPlatform::EGL).0,
                    )
                    .context("Cannot wrap Slint's EGL context")?;
                    (context, display.upcast::<gst_gl::GLDisplay>(), "EGL")
                }
            }
        };
        context.activate(true)?;
        context.fill_info()?;
        gst::info!(
            gst::CAT_DEFAULT,
            "Sharing Slint {platform} context with GStreamer"
        );
        self.context = Some(context.clone());
        bus.set_sync_handler(move |_, message| {
            if let gst::MessageView::NeedContext(need) = message.view() {
                if let Some(element) = message.src().and_then(|s| s.downcast_ref::<gst::Element>())
                {
                    let kind = need.context_type();
                    if kind == *gst_gl::GL_DISPLAY_CONTEXT_TYPE {
                        let ctx = gst::Context::new(kind, true);
                        ctx.set_gl_display(&display);
                        element.set_context(&ctx);
                    } else if kind == "gst.gl.app_context" {
                        let mut ctx = gst::Context::new(kind, true);
                        ctx.get_mut()
                            .unwrap()
                            .structure_mut()
                            .set("context", &context);
                        element.set_context(&ctx);
                    }
                }
            } else {
                let _ = sender.send(message.to_owned());
            }
            gst::BusSyncReply::Drop
        });
        let notify = Arc::new(Mutex::new(notify));
        self.motion_notify=Some(notify.clone());
        let next = self.next.clone();
        let preroll_next = next.clone();
        let preroll_notify = notify.clone();
        self.appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_preroll(move |sink| receive_frame(sink, true, &preroll_next, &preroll_notify))
                .new_sample(move |sink| receive_frame(sink, false, &next, &notify))
                .build(),
        );
        Ok(())
    }
    fn reset_motion(&mut self) {
        self.next.lock().unwrap().motion=None;
        self.motion=None;
        if self.early_offset != 0 {
            self.appsink.set_property("ts-offset", 0i64);
            self.early_offset = 0;
        }
        interpolation::inactive();
    }
    fn motion_failed(&mut self, error: anyhow::Error) {
        let reason = error.to_string();
        crate::diagnostics::record(
            "interpolation_fallback",
            serde_json::json!({"reason":reason}),
        );
        self.effect_status = reason.clone();
        self.blocked = Some(reason);
        self.reset_motion();
    }
    fn current_image(&mut self) -> Result<Option<(slint::Image, bool, u64)>> {
        let Some(frame) = self.current.as_ref() else {
            return Ok(None);
        };
        let surface = self.surface.as_mut().context("Missing video surface")?;
        let changed = surface.copy(frame.texture_id(0)?, frame.width(), frame.height())?;
        Ok(Some((
            surface.image.clone(),
            changed,
            self.current_pts.unwrap_or(0),
        )))
    }
    fn prepared_image(&mut self)->Result<Option<(slint::Image,bool,u64)>> {
        let worker=self.motion.as_ref().context("Missing interpolation worker")?;
        let surface=self.surface.as_mut().context("Missing video surface")?;
        let Some(frame)=worker.take_ready(&surface.gl)? else {return Ok(None);};
        let pts=frame.pts;
        let synthetic=frame.synthetic;
        let result=surface.copy(frame.texture,self.current.as_ref().map_or(surface.size.0,|f|f.width()),self.current.as_ref().map_or(surface.size.1,|f|f.height()));
        worker.recycle(&surface.gl,frame)?;
        let changed=result?;
        if synthetic {interpolation::presented();}
        self.current=None;self.current_pts=Some(pts);
        Ok(Some((surface.image.clone(),changed,pts)))
    }
    fn next_image(&mut self,mode:i32)->Result<Option<(slint::Image,bool,u64)>> {
        if let Some(q)=&self.redraw_pending {q.store(false,Ordering::Release);}
        let (epoch,flushing)={let m=self.next.lock().unwrap();(m.epoch,m.flushing)};
        if self.generation!=epoch||self.requested!=mode {
            self.reset_motion(); self.blocked=None;
            if self.generation!=epoch {self.current=None;self.current_pts=None;}
            self.generation=epoch; self.requested=mode;
        }
        if flushing {return Ok(None);}
        if let Some(worker)=self.motion.as_ref() {
            worker.shared.auto_fallback.store(self.auto_fallback,Ordering::Relaxed);
            if let Some(error)=worker.error() {
                self.motion_failed(anyhow::anyhow!(error));
                return Ok(None);
            }
            return match self.prepared_image() {
                Ok(image)=>Ok(image),
                Err(error)=>{self.motion_failed(error);Ok(None)}
            };
        }
        let incoming={let mut m=self.next.lock().unwrap();let due=m.due.take().unwrap_or_else(std::time::Instant::now);m.next.take().map(|(info,buffer)|(info,buffer,due))};
        let Some((info,buffer,due))=incoming else {return Ok(None);};
        let pts=buffer.pts().map(|t|t.nseconds()).unwrap_or_else(||LAST_VIDEO_PTS.load(Ordering::Acquire));
        let rate=info.fps();
        self.source_fps=if rate.denom()>0 {rate.numer() as f32/rate.denom() as f32}else{0.};
        let par=info.par();
        self.display_aspect=display_aspect(info.width(),info.height(),par.numer(),par.denom());
        if mode>0 && self.blocked.is_none() {
            if let Some(period)=interpolation::policy::interval(info.width(),info.height(),rate.numer(),rate.denom()) {
                let worker=interpolation::worker::Worker::new(self.context.as_ref().context("Missing display GL context")?,interpolation::worker::Input{info:info.clone(),buffer:buffer.clone(),due},mode,self.auto_fallback,self.motion_notify.as_ref().context("Missing redraw callback")?.clone());
                match worker {
                    Ok(worker)=>{
                        self.next.lock().unwrap().motion=Some(Arc::downgrade(&worker.shared));
                        self.motion=Some(worker);
                        let offset=-((period*interpolation::policy::LOOKAHEAD_SOURCE_FRAMES+interpolation::policy::PREPARE_LEAD).as_nanos() as i64);
                        self.appsink.set_property("ts-offset",offset);self.early_offset=offset;
                        self.effect_status=format!("GPU prepare-ahead · {:.2} → {:.2} fps",self.source_fps,if mode==2{60.}else{self.source_fps*2.});
                    }
                    Err(error)=>self.motion_failed(error),
                }
            } else {self.effect_status="Native frames · trial supports ≤720p, 12–30 fps".into();}
        } else if mode==0 {self.effect_status="Off · native frames".into();}
        buffer.meta::<gst_gl::GLSyncMeta>().context("Missing GPU frame synchronization")?.wait(self.context.as_ref().context("Missing display GL context")?);
        self.current=Some(gst_gl::GLVideoFrame::from_buffer_readable(buffer,&info).map_err(|_|anyhow::anyhow!("Cannot import the decoded OpenGL frame"))?);
        self.current_pts=Some(pts);
        self.current_image()
    }
    fn clear(&mut self) {
        self.source_fps=0.;
        self.reset_motion();self.current_pts=None;self.effect_status="Waiting for video".into();
        self.current = None;
        self.next.lock().unwrap().next.take();
        if let Some(surface) = self.surface.as_mut() {
            surface.clear_image();
        }
    }
    fn release(&mut self) {
        self.source_fps=0.;
        self.reset_motion();self.current_pts=None;
        self.surface = None;
        self.current = None;
        self.next.lock().unwrap().next.take();
        if let Some(context) = self.context.take() {
            let _ = context.activate(false);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn receive_frame(
    sink: &gst_app::AppSink,
    preroll: bool,
    mailbox: &Mutex<FrameMailbox>,
    notify: &Mutex<Box<dyn Fn() + Send>>,
) -> std::result::Result<gst::FlowSuccess, gst::FlowError> {
    let epoch = mailbox.lock().unwrap().epoch;
    let sample = if preroll {
        sink.pull_preroll()
    } else {
        sink.pull_sample()
    }
    .map_err(|_| gst::FlowError::Flushing)?;
    let mut buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
    if buffer.n_memory() == 0 {
        return Err(gst::FlowError::Error);
    }
    let context = buffer
        .peek_memory(0)
        .downcast_memory_ref::<gst_gl::GLBaseMemory>()
        .map(|m| m.context().clone())
        .ok_or(gst::FlowError::NotNegotiated)?;
    // The producer owns an existing sync point. In particular, appsink may
    // deliver the same buffer as both preroll and a sample: resetting its fence
    // would delete an object concurrently used by the UI/interpolation context.
    if buffer.meta::<gst_gl::GLSyncMeta>().is_none() {
        gst_gl::GLSyncMeta::add(buffer.make_mut(), &context).set_sync_point(&context);
    }
    let info = sample
        .caps()
        .and_then(|c| gst_video::VideoInfo::from_caps(c).ok())
        .ok_or(gst::FlowError::NotNegotiated)?;
    let now=std::time::Instant::now();
    let due=(|| {
        let running=sample.segment()?.downcast_ref::<gst::ClockTime>()?.to_running_time(buffer.pts()?)?;
        let target=sink.base_time()?.nseconds().saturating_add(running.nseconds()).saturating_add(gst_base::prelude::BaseSinkExt::latency(sink.upcast_ref::<gst::Element>().downcast_ref::<gst_base::BaseSink>()?).nseconds());
        let clock=sink.clock()?.time()?.nseconds();
        Some(if target>=clock {now+std::time::Duration::from_nanos(target-clock)}else{now.checked_sub(std::time::Duration::from_nanos(clock-target)).unwrap_or(now)})
    })().unwrap_or(now);
    let should_notify;
    {
        let mut frames = mailbox.lock().unwrap();
        if frames.flushing || frames.epoch != epoch {
            return Ok(gst::FlowSuccess::Ok);
        }
        if let Some(worker)=frames.motion.as_ref().and_then(|w|w.upgrade()) {
            worker.submit(interpolation::worker::Input{info,buffer,due});
            return Ok(gst::FlowSuccess::Ok);
        }
        frames.received=Some(now);frames.due=Some(due);
        frames.next = Some((info, buffer));
        should_notify=!frames.timed;
    }
    if should_notify {notify.lock().unwrap()();}
    Ok(gst::FlowSuccess::Ok)
}

// Keep one Slint image/texture for each video size. Replacing the UI Image every
// frame invalidates Slint's scene and schedules duplicate draws. The copy below
// is a GPU framebuffer blit; it never reads decoded pixels into host memory.
#[cfg(not(target_os = "windows"))]
struct Surface {
    gl: glow::Context,
    read: glow::NativeFramebuffer,
    draw: glow::NativeFramebuffer,
    texture: Option<glow::NativeTexture>,
    size: (u32, u32),
    image: slint::Image,
}
#[cfg(not(target_os = "windows"))]
impl Surface {
    fn new(gl: glow::Context) -> Result<Self> {
        let read = unsafe { gl.create_framebuffer() }.map_err(|e| anyhow::anyhow!(e))?;
        let draw = unsafe { gl.create_framebuffer() }.map_err(|e| anyhow::anyhow!(e))?;
        Ok(Self {
            gl,
            read,
            draw,
            texture: None,
            size: (0, 0),
            image: slint::Image::default(),
        })
    }
    fn clear_image(&mut self) {
        self.image = Default::default();
        self.size = (0, 0);
        // Detach both FBO attachments before deleting the texture: otherwise GL
        // can retain their storage even after the decoded frame is dropped.
        // This runs exclusively in BeforeRendering on the owning current context.
        if let Some(texture) = self.texture.take() {
            unsafe {
                let gl = &self.gl;
                let old_read = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING) as u32;
                let old_draw = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) as u32;
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.read));
                gl.framebuffer_texture_2d(
                    glow::READ_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    None,
                    0,
                );
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.draw));
                gl.framebuffer_texture_2d(
                    glow::DRAW_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    None,
                    0,
                );
                gl.bind_framebuffer(
                    glow::READ_FRAMEBUFFER,
                    std::num::NonZeroU32::new(old_read).map(glow::NativeFramebuffer),
                );
                gl.bind_framebuffer(
                    glow::DRAW_FRAMEBUFFER,
                    std::num::NonZeroU32::new(old_draw).map(glow::NativeFramebuffer),
                );
                gl.delete_texture(texture);
            }
        }
    }
    fn copy(&mut self, source: u32, width: u32, height: u32) -> Result<bool> {
        let source = std::num::NonZeroU32::new(source).context("Invalid video texture")?;
        let gl = &self.gl;
        let changed = self.size != (width, height);
        // SAFETY: rendering callback runs on the owning GL thread. Bindings are
        // restored so GStreamer interop does not alter Slint's renderer state.
        unsafe {
            let old_read = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING) as u32;
            let old_draw = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) as u32;
            if changed {
                let old_texture = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32;
                if let Some(texture) = self.texture.take() {
                    gl.delete_texture(texture);
                }
                let texture = gl.create_texture().map_err(|e| anyhow::anyhow!(e))?;
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_S,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    width as i32,
                    height as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                gl.bind_texture(
                    glow::TEXTURE_2D,
                    std::num::NonZeroU32::new(old_texture).map(glow::NativeTexture),
                );
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.draw));
                gl.framebuffer_texture_2d(
                    glow::DRAW_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(texture),
                    0,
                );
                self.image = slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                    texture.0,
                    [width, height].into(),
                )
                .build();
                self.texture = Some(texture);
                self.size = (width, height);
            }
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.read));
            gl.framebuffer_texture_2d(
                glow::READ_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(glow::NativeTexture(source)),
                0,
            );
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.draw));
            let scissor = gl.is_enabled(glow::SCISSOR_TEST);
            gl.disable(glow::SCISSOR_TEST);
            gl.blit_framebuffer(
                0,
                0,
                width as i32,
                height as i32,
                0,
                0,
                width as i32,
                height as i32,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            if scissor {
                gl.enable(glow::SCISSOR_TEST);
            }
            // Do not retain a producer-owned texture in the UI's read FBO across
            // quality changes or worker destruction. The queued blit retains
            // its own GPU references until the consumer fence completes.
            gl.framebuffer_texture_2d(
                glow::READ_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                None,
                0,
            );
            gl.bind_framebuffer(
                glow::READ_FRAMEBUFFER,
                std::num::NonZeroU32::new(old_read).map(glow::NativeFramebuffer),
            );
            gl.bind_framebuffer(
                glow::DRAW_FRAMEBUFFER,
                std::num::NonZeroU32::new(old_draw).map(glow::NativeFramebuffer),
            );
        }
        Ok(changed)
    }
}
#[cfg(not(target_os = "windows"))]
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if let Some(texture) = self.texture.take() {
                self.gl.delete_texture(texture);
            }
            self.gl.delete_framebuffer(self.read);
            self.gl.delete_framebuffer(self.draw);
        }
    }
}

// Windows video must stay in D3D12 memory, including during codec retries.
#[cfg(any(target_os = "windows", test))]
fn windows_video_caps(audio: bool) -> &'static str {
    if audio {
        "video/x-raw(memory:D3D12Memory);audio/x-raw(ANY)"
    } else {
        "video/x-raw(memory:D3D12Memory)"
    }
}

fn software_video_decoder(klass: &str) -> bool {
    let has = |category| klass.split('/').any(|part| part == category);
    has("Decoder") && has("Video") && !has("Hardware")
}

// Per-decodebin selection policy, not a global plugin blacklist: audio decoders,
// parsers and demuxers retain their normal behavior on both platforms.
fn hardware_video_only(source: &gst::Element) {
    source.connect("autoplug-select", false, |args| {
        let skip = args[3]
            .get::<gst::ElementFactory>()
            .ok()
            .and_then(|f| f.metadata("klass").map(|k| software_video_decoder(&k)))
            .unwrap_or(true);
        let class = gst::glib::EnumClass::with_type(
            gst::glib::Type::from_name("GstAutoplugSelectResult")
                .expect("decodebin registered its autoplug-select enum"),
        )
        .expect("autoplug-select returns an enum");
        class.to_value_by_nick(if skip { "skip" } else { "try" })
    });
}

fn decoded_caps(video: bool, audio: bool) -> &'static str {
    #[cfg(target_os = "windows")]
    if video {
        return windows_video_caps(audio);
    }
    match (video, audio) {
        (true, true) => "video/x-raw(ANY);audio/x-raw(ANY)",
        (true, false) => "video/x-raw(ANY)",
        _ => "audio/x-raw(ANY)",
    }
}

// Preserve anamorphic display geometry without rescaling/copying decoded pixels.
#[cfg(any(not(target_os = "windows"), test))]
fn display_aspect(width: u32, height: u32, numerator: i32, denominator: i32) -> f32 {
    let par = if numerator > 0 && denominator > 0 { numerator as f64 / denominator as f64 } else { 1. };
    if width == 0 || height == 0 { return 0.; }
    (width as f64 * par / height as f64).clamp(0.05, 20.) as f32
}
fn linear_volume(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0., 100.) / 100.
    } else {
        0.7
    }
}
#[cfg(test)]
mod volume_tests {
    #[test]
    fn anamorphic_source_geometry_is_preserved() {
        assert!((super::display_aspect(1280,534,801,800) - 2.4).abs()<0.001);
        assert!((super::display_aspect(720,576,16,15) - 4./3.).abs()<0.001);
        assert_eq!(super::display_aspect(1280,720,0,0),1280./720.);
    }
    use super::*;
    #[test]
    fn windows_video_rejects_cpu_frames_without_restricting_audio() {
        gst::init().unwrap();
        for audio in [false, true] {
            let native: gst::Caps = windows_video_caps(audio).parse().unwrap();
            for format in ["I420", "I420_10LE", "NV12", "P010_10LE"] {
                let cpu: gst::Caps =
                    format!("video/x-raw,format={format},width=3840,height=2160,framerate=60/1")
                        .parse()
                        .unwrap();
                assert!(!cpu.can_intersect(&native));
                let gpu: gst::Caps = format!(
                    "video/x-raw(memory:D3D12Memory),format={format},width=3840,height=2160,framerate=60/1"
                ).parse().unwrap();
                assert!(gpu.is_subset(&native));
            }
            let sound: gst::Caps = "audio/x-raw,format=F32LE".parse().unwrap();
            assert_eq!(sound.can_intersect(&native), audio);
        }
    }
    #[test]
    fn decoder_policy_skips_only_software_video() {
        for klass in ["Codec/Decoder/Video", "Codec/Decoder/Video/Image"] {
            assert!(software_video_decoder(klass));
        }
        for klass in [
            "Codec/Decoder/Video/Hardware",
            "Codec/Decoder/Audio",
            "Codec/Parser/Video",
            "Codec/Demuxer",
            "Filter/Converter/Video",
        ] {
            assert!(!software_video_decoder(klass));
        }
        gst::init().unwrap();
        let source = gst::ElementFactory::make("uridecodebin").build().unwrap();
        hardware_video_only(&source);
        let pad = gst::Pad::builder(gst::PadDirection::Sink).build();
        let caps: gst::Caps = "video/x-h264".parse().unwrap();
        for (name, expected) in [
            ("avdec_h264", "skip"),
            ("opusdec", "try"),
            ("h264parse", "try"),
        ] {
            let factory = gst::ElementFactory::find(name).expect("test codec installed");
            let result = source
                .emit_by_name_with_values(
                    "autoplug-select",
                    &[pad.to_value(), caps.to_value(), factory.to_value()],
                )
                .unwrap();
            assert_eq!(
                gst::glib::EnumValue::from_value(&result).unwrap().1.nick(),
                expected
            );
        }
    }
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn decoded_d3d11_frames_are_terminal_caps_not_a_missing_codec() {
        gst::init().unwrap();
        let hardware: gst::Caps =
            "video/x-raw(memory:D3D11Memory),format=NV12,width=1280,height=720"
                .parse()
                .unwrap();
        let software: gst::Caps = "video/x-raw,format=NV12,width=1280,height=720"
            .parse()
            .unwrap();
        for audio in [false, true] {
            let accepted: gst::Caps = super::decoded_caps(true, audio).parse().unwrap();
            assert!(hardware.is_subset(&accepted));
            assert!(software.is_subset(&accepted));
        }
        let audio_only: gst::Caps = super::decoded_caps(false, true).parse().unwrap();
        assert!(!hardware.can_intersect(&audio_only));
    }

    #[test]
    fn stalled_seek_restarts_once_and_reseeks_after_fresh_preroll() {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(
            "videotestsrc ! video/x-raw,width=32,height=32 ! fakesink name=video sync=true \
             audiotestsrc ! fakesink name=audio sync=true",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let video = pipeline.by_name("video").unwrap();
        let audio = pipeline.by_name("audio").unwrap();
        let progress = Arc::new(Mutex::new(super::SeekProgress::default()));
        super::watch_seek_output(&video, progress.clone(), 2);
        super::watch_seek_output(&audio, progress.clone(), 1);
        let (old_events, messages) = mpsc::channel();
        let mut engine = super::Engine {
            pipeline,
            messages,
            decoder: Arc::new(Mutex::new(String::new())),
            evidence: Arc::new(super::acceleration::Evidence::default()),
            video_sink: video,
            transient: Vec::new(),
            buffer_state: Mutex::new(buffered::Snapshot::default()),
            recovery: rebuffer::Recovery::default(),
            source_hls: source_hls::Gate::default(),
            ready: Arc::new(AtomicBool::new(true)),
            setup_error: Arc::new(Mutex::new(None)),
            volume: None,
            frames: Arc::new(Mutex::new(super::FrameMailbox::default())),
            presentation: Arc::new(AtomicBool::new(true)),
            pending_seek: None,
            seek_progress: progress,
            #[cfg(target_os = "windows")]
            native: None,
            #[cfg(target_os = "windows")]
            remote: None,
            seek_watch: None,
            queued_seek: None,
            cache_capture: None,
        };
        engine.pipeline.set_state(gst::State::Playing).unwrap();
        engine
            .pipeline
            .state(gst::ClockTime::from_seconds(2))
            .0
            .unwrap();
        // A quality reload may report Paused during preroll. Explicit play intent
        // must survive the resume seek rather than inheriting that transient state.
        for (position, paused) in [(0., false), (0., true), (1.25, false), (1.25, true)] {
            engine.pipeline.set_state(gst::State::Paused).unwrap();
            engine
                .pipeline
                .state(gst::ClockTime::from_seconds(2))
                .0
                .unwrap();
            let mut start = super::playback_state::Start::new(position, paused);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                engine.poll_seek().unwrap();
                if engine.poll_start(&mut start).unwrap() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "startup resume did not complete"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(
                engine
                    .pipeline
                    .query_position::<gst::ClockTime>()
                    .unwrap()
                    .seconds_f64()
                    >= position - 0.1
            );
            engine
                .pipeline
                .state(gst::ClockTime::from_seconds(2))
                .0
                .unwrap();
            assert_eq!(
                engine.pipeline.current_state(),
                if paused {
                    gst::State::Paused
                } else {
                    gst::State::Playing
                }
            );
        }
        engine.set_paused(false).unwrap();
        for target in [6., 1., 4., 0.5] {
            engine.seek_with_state(target, false).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !engine.poll_seek().unwrap() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "Backward/forward seek stalled"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let sample = engine
                .video_sink
                .property::<Option<gst::Sample>>("last-sample")
                .unwrap();
            let pts = sample.buffer().unwrap().pts().unwrap().seconds_f64();
            assert!(
                (pts - target).abs() < 0.3,
                "Video stayed at {pts} after seeking to {target}"
            );
            let pos = engine
                .pipeline
                .query_position::<gst::ClockTime>()
                .unwrap()
                .seconds_f64();
            assert!(
                (pos - target).abs() < 0.3,
                "Audio clock did not follow the video seek"
            );
        }
        // Simulate a seek that was accepted but never produced its new segment.
        engine
            .seek_progress
            .lock()
            .unwrap()
            .begin(gst::Seqnum::next(), 0.);
        engine.pending_seek = Some(super::PendingSeek {
            target: 1.,
            resume: true,
            started: std::time::Instant::now() - std::time::Duration::from_secs(16),
            repreroll: false,
            await_pause: false,
            recovered: false,
        });
        assert!(!engine.poll_seek().unwrap());
        assert!(engine.pending_seek.as_ref().unwrap().repreroll);
        engine.seek_with_state(2., true).unwrap(); // Newest request wins, preserving pause.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !engine.poll_seek().unwrap() {
            assert!(
                std::time::Instant::now() < deadline,
                "recovery did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(engine.pipeline.current_state(), gst::State::Paused);
        assert!(
            (engine
                .pipeline
                .query_position::<gst::ClockTime>()
                .unwrap()
                .seconds_f64()
                - 2.)
                .abs()
                < 0.1
        );
        assert!(engine.queued_seek.is_none());
        engine.begin_seek(1., true, true).unwrap();
        engine.pending_seek.as_mut().unwrap().started -= std::time::Duration::from_secs(21);
        engine
            .seek_progress
            .lock()
            .unwrap()
            .begin(gst::Seqnum::next(), 0.);
        assert!(engine.poll_seek().is_err()); // No unbounded restart loop.
        assert!(!engine.seeking());
        assert!(!engine.frames.lock().unwrap().flushing);
        old_events.send(gst::message::Eos::new()).unwrap();
        engine.stop().unwrap();
        assert!(
            engine.messages.try_recv().is_err(),
            "Old EOS crossed the stopped session boundary"
        );
        engine.seek_with_state(20., false).unwrap();
        assert!(!engine.seeking(), "Queued seek revived a stopped pipeline");
        assert_eq!(engine.pipeline.current_state(), gst::State::Null);
    }

    #[test]
    fn seek_completion_requires_current_segments_and_both_outputs() {
        gst::init().unwrap();
        let old = gst::Seqnum::next();
        let current = gst::Seqnum::next();
        let mut progress = super::SeekProgress::default();
        progress.begin(current, 0.);
        progress.buffer(1);
        progress.segment(old, 2);
        progress.buffer(2);
        assert!(!progress.ready(3));
        progress.segment(current, 1);
        progress.buffer(1);
        assert!(progress.ready(1)); // Audio-only seeks do not wait for video.
        assert!(!progress.ready(3));
        progress.segment(current, 2);
        assert!(!progress.ready(3)); // A segment alone is not preroll.
        progress.buffer(2);
        assert!(progress.ready(3));
        progress.begin(gst::Seqnum::next(), 0.);
        assert!(!progress.ready(3)); // A queued seek cannot inherit old completion.
    }
    #[test]
    fn old_video_timestamp_cannot_complete_a_backward_seek() {
        gst::init().unwrap();
        let seq = gst::Seqnum::next();
        let mut p = super::SeekProgress::default();
        p.begin(seq, 10.);
        p.segment(seq, 1);
        p.segment(seq, 2);
        p.buffer(1);
        p.buffer(2);
        p.video(Some(gst::ClockTime::from_seconds(30)), None);
        assert!(!p.ready_for_target(3));
        p.video(Some(gst::ClockTime::from_seconds(10)), None);
        assert!(p.ready_for_target(3));
        assert!(p.ready_for_target(1));
        p.begin(seq, 10.9);
        p.segment(seq, 1);
        p.segment(seq, 2);
        p.buffer(1);
        p.buffer(2);
        p.video(
            Some(gst::ClockTime::from_seconds(10)),
            Some(gst::ClockTime::from_seconds(1)),
        );
        assert!(p.ready_for_target(3)); // One-fps videos are still valid seek outputs.
    }
    #[test]
    fn playing_seek_completes_on_current_segment_clock_when_preroll_was_dropped() {
        gst::init().unwrap();
        let seq = gst::Seqnum::next();
        let mut p = super::SeekProgress::default();
        p.begin(seq, 2.021783);
        p.segment(seq, 1);
        p.segment(seq, 2);
        p.buffer(1);
        p.buffer(2);
        p.video(Some(gst::ClockTime::from_mseconds(4216)), None);
        assert!(!p.ready_for_target(3));
        assert!(p.ready_at_playing_clock(
            3,
            Some(gst::ClockTime::from_mseconds(4221)),
            std::time::Duration::from_millis(2219)
        ));
        assert!(!p.ready_at_playing_clock(
            3,
            Some(gst::ClockTime::from_mseconds(2022)),
            std::time::Duration::from_millis(2219)
        ));
        assert!(!p.ready_at_playing_clock(
            3,
            Some(gst::ClockTime::from_mseconds(4221)),
            std::time::Duration::from_millis(10)
        ));
        p.video(Some(gst::ClockTime::from_seconds(30)), None);
        assert!(!p.ready_at_playing_clock(
            3,
            Some(gst::ClockTime::from_seconds(30)),
            std::time::Duration::from_secs(2)
        ));
        p.segment(gst::Seqnum::next(), 2);
        assert!(!p.ready_at_playing_clock(
            3,
            Some(gst::ClockTime::from_seconds(30)),
            std::time::Duration::from_secs(30)
        ));
    }
    #[test]
    fn completed_seek_survives_delayed_poll_and_invalidates_on_flush() {
        gst::init().unwrap();
        let seq = gst::Seqnum::next();
        let mut p = super::SeekProgress::default();
        p.begin(seq, 0.);
        p.segment(seq, 1);
        p.segment(seq, 2);
        p.buffer(1);
        p.buffer(2);
        p.video(Some(gst::ClockTime::from_mseconds(16)), None);
        // Replay the owner's log: no poll until playback is already at 3.98 s.
        p.video(Some(gst::ClockTime::from_mseconds(3983)), None);
        assert!(p.ready_for_target(3));
        assert_eq!(p.video_pts, Some(gst::ClockTime::from_mseconds(3983)));
        p.segment(gst::Seqnum::next(), 2);
        assert!(!p.ready_for_target(3));
        p.begin(gst::Seqnum::next(), 25.);
        assert!(!p.ready_for_target(3));
    }
    #[test]
    fn stale_segment_invalidates_its_output() {
        gst::init().unwrap();
        let current = gst::Seqnum::next();
        let mut progress = super::SeekProgress::default();
        progress.begin(current, 0.);
        progress.segment(current, 1);
        progress.buffer(1);
        assert!(progress.ready(1));
        progress.segment(gst::Seqnum::next(), 1);
        progress.buffer(1);
        assert!(!progress.ready(1));
    }

    #[test]
    fn gain_never_amplifies() {
        for value in [-100., 0., 70., 100., 200., f64::INFINITY, f64::NAN] {
            let gain = super::linear_volume(value);
            assert!((0.0..=1.0).contains(&gain));
        }
        assert_eq!(super::linear_volume(100.), 1.);
    }
}

#[cfg(feature = "native-overlay-test")]
pub(crate) fn probe_native_overlay() -> Result<serde_json::Value> {
    native_overlay::probe()
}

#[cfg(target_os = "windows")]
fn native_pipeline(
    decoder: Arc<Mutex<String>>,
    evidence: Arc<acceleration::Evidence>,
) -> (gst::Pipeline, mpsc::Receiver<gst::Message>) {
    let pipeline = gst::Pipeline::new();
    observe_decoders(&pipeline, decoder, evidence);
    let (sender, messages) = mpsc::channel();
    pipeline.bus().unwrap().set_sync_handler(move |_, message| {
        if !matches!(message.view(), gst::MessageView::NeedContext(_)) {
            let _ = sender.send(message.to_owned());
        }
        gst::BusSyncReply::Drop
    });
    (pipeline, messages)
}
#[cfg(target_os = "windows")]
fn install_native(_app: &SetupWindow) -> Result<Engine> {
    install_native_engine(true)
}

#[cfg(target_os = "windows")]
fn install_native_worker(hwnd: isize) -> Result<Engine> {
    let engine = install_native_engine(false)?;
    engine.native.as_ref().unwrap().bind_process_window(hwnd);
    engine.ready.store(true, Ordering::Release);
    Ok(engine)
}
#[cfg(target_os = "windows")]
fn install_native_engine(separate_process: bool) -> Result<Engine> {
    let decoder = Arc::new(Mutex::new(String::new()));
    let evidence = Arc::new(acceleration::Evidence::default());
    let native = windows_native::Controller::new(evidence.clone());
    let (pipeline, messages) = native_pipeline(decoder.clone(), evidence.clone());
    Ok(Engine {
        pipeline,
        messages,
        decoder,
        evidence,
        remote: separate_process.then(|| windows_process::Client::new(native.clone())),
        native: Some(native),
        video_sink: gst::ElementFactory::make("fakesink").build()?,
        transient: Vec::new(),
        buffer_state: Mutex::new(buffered::Snapshot::default()),
        recovery: rebuffer::Recovery::default(),
            source_hls: source_hls::Gate::default(),
        ready: Arc::new(AtomicBool::new(false)),
        setup_error: Arc::new(Mutex::new(None)),
        volume: None,
        frames: Arc::new(Mutex::new(FrameMailbox::default())),
        presentation: Arc::new(AtomicBool::new(false)),
        pending_seek: None,
        seek_progress: Arc::new(Mutex::new(SeekProgress::default())),
        seek_watch: None,
        queued_seek: None,
        cache_capture: None,
    })
}

#[cfg(target_os = "windows")]
pub(crate) fn run_video_worker(parent: isize) -> Result<()> {
    windows_process::run_worker(parent)
}

#[cfg(feature = "native-overlay-test")]
pub(crate) fn probe_main_layout() -> Result<serde_json::Value> {
    native_overlay::probe_main_layout()
}

#[cfg(feature = "native-overlay-test")]
pub(crate) fn probe_music_skins() -> Result<serde_json::Value> {
    native_overlay::probe_music_skins()
}
