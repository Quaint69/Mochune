//! Opt-in D3D12 frame generation inside the disposable video worker.
//! No CPU video mapping, system-memory caps or disk cache. The normal sink path
//! does not construct this element, its converter, shaders or GPU allocations.
use super::interpolation_policy as policy;
use anyhow::{Result, bail};
use gst::{glib, prelude::*, subclass::prelude::*};
use std::{
    ffi::{CStr, c_char, c_void},
    sync::{Arc, Mutex, OnceLock},
};
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub mode: i32,
    pub source_fps: f32,
    pub generated: u64,
    pub gpu_ms: f64,
    pub texture_bytes: u64,
    pub blocked: bool,
    pub status: String,
}
pub type Shared = Arc<Mutex<Report>>;
unsafe extern "C" {
    fn mochune_fps_new(
        buffer: *mut gst::ffi::GstBuffer,
        w: u32,
        h: u32,
        error: *mut c_char,
        cap: usize,
    ) -> *mut c_void;
    fn mochune_fps_render(
        ctx: *mut c_void,
        a: *mut gst::ffi::GstBuffer,
        b: *mut gst::ffi::GstBuffer,
        fractions: *const f32,
        count: i32,
        out: *mut *mut gst::ffi::GstBuffer,
        ns: *mut u64,
        error: *mut c_char,
        cap: usize,
    ) -> i32;
    fn mochune_fps_free(ctx: *mut c_void);
    fn mochune_fps_shader_check(error: *mut c_char, cap: usize) -> i32;
}
fn message(error: &[c_char]) -> String {
    unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
pub fn shader_check() -> Result<()> {
    let mut error = [0; 4096];
    if unsafe { mochune_fps_shader_check(error.as_mut_ptr(), error.len()) } == 0 {
        bail!("{}", message(&error));
    }
    Ok(())
}
struct Gpu(*mut c_void);
// The element's streaming/state mutex serializes all accesses to the native
// queue. It never calls Slint or touches the main application's GPU resources.
unsafe impl Send for Gpu {}
impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe { mochune_fps_free(self.0) }
    }
}
impl Gpu {
    fn new(buffer: &gst::BufferRef, info: &gst_video::VideoInfo) -> Result<Self> {
        let mut error = [0; 4096];
        let ptr = unsafe {
            mochune_fps_new(
                buffer.as_ptr() as *mut _,
                info.width(),
                info.height(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if ptr.is_null() {
            bail!("{}", message(&error));
        }
        Ok(Self(ptr))
    }
    fn render(
        &mut self,
        a: &gst::BufferRef,
        b: &gst::BufferRef,
        fractions: &[f32],
    ) -> Result<(Vec<gst::Buffer>, u64)> {
        use glib::translate::from_glib_full;
        let mut outputs = [std::ptr::null_mut(); 8];
        let mut ns = 0;
        let mut error = [0; 4096];
        let ok = unsafe {
            mochune_fps_render(
                self.0,
                a.as_ptr() as *mut _,
                b.as_ptr() as *mut _,
                fractions.as_ptr(),
                fractions.len() as i32,
                outputs.as_mut_ptr(),
                &mut ns,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if ok < 0 {
            bail!("GPU commands did not complete safely: {}", message(&error));
        }
        if ok == 0 {
            bail!("{}", message(&error));
        }
        Ok((
            outputs[..fractions.len()]
                .iter()
                .map(|p| unsafe { from_glib_full(*p) })
                .collect(),
            ns,
        ))
    }
}
#[derive(Default)]
struct State {
    info: Option<gst_video::VideoInfo>,
    previous: Option<gst::Buffer>,
    gpu: Option<Gpu>,
    slow: u8,
    disabled: bool,
}
impl State {
    fn reset_pair(&mut self) {
        self.previous = None;
        self.slow = 0;
    }
    fn clear(&mut self) {
        self.previous = None;
        self.gpu = None;
        self.slow = 0;
    }
}
struct Config {
    mode: i32,
    auto: bool,
    report: Shared,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            mode: 0,
            auto: true,
            report: Default::default(),
        }
    }
}
mod imp {
    use super::*;
    pub struct Filter {
        sink: gst::Pad,
        src: gst::Pad,
        state: Mutex<State>,
        config: Mutex<Config>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Filter {
        const NAME: &'static str = "MochuneD3D12Interpolation";
        type Type = super::Filter;
        type ParentType = gst::Element;
        fn with_class(class: &Self::Class) -> Self {
            let sink = gst::Pad::builder_from_template(&class.pad_template("sink").unwrap())
                .chain_function(|_, parent, buffer| {
                    Self::catch_panic_pad_function(
                        parent,
                        || Err(gst::FlowError::Error),
                        |imp| imp.chain(buffer),
                    )
                })
                .event_function(|pad, parent, event| {
                    Self::catch_panic_pad_function(parent, || false, |imp| imp.event(pad, event))
                })
                .query_function(|pad, parent, query| gst::Pad::query_default(pad, parent, query))
                .build();
            let src = gst::Pad::builder_from_template(&class.pad_template("src").unwrap())
                .event_function(|pad, parent, event| gst::Pad::event_default(pad, parent, event))
                .query_function(|pad, parent, query| gst::Pad::query_default(pad, parent, query))
                .build();
            Self {
                sink,
                src,
                state: Default::default(),
                config: Default::default(),
            }
        }
    }
    impl ObjectImpl for Filter {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().add_pad(&self.sink).unwrap();
            self.obj().add_pad(&self.src).unwrap();
        }
    }
    impl GstObjectImpl for Filter {}
    impl ElementImpl for Filter {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static M: OnceLock<gst::subclass::ElementMetadata> = OnceLock::new();
            Some(M.get_or_init(|| {
                gst::subclass::ElementMetadata::new(
                    "D3D12 motion interpolation",
                    "Filter/Effect/Video/Hardware",
                    "GPU-only 2x/60 fps presentation",
                    "Mochune contributors",
                )
            }))
        }
        fn pad_templates() -> &'static [gst::PadTemplate] {
            static P: OnceLock<Vec<gst::PadTemplate>> = OnceLock::new();
            P.get_or_init(|| {
                let caps = "video/x-raw(memory:D3D12Memory),format=(string)RGBA"
                    .parse::<gst::Caps>()
                    .unwrap();
                ["sink", "src"]
                    .into_iter()
                    .map(|name| {
                        gst::PadTemplate::new(
                            name,
                            if name == "sink" {
                                gst::PadDirection::Sink
                            } else {
                                gst::PadDirection::Src
                            },
                            gst::PadPresence::Always,
                            &caps,
                        )
                        .unwrap()
                    })
                    .collect()
            })
        }
        fn change_state(
            &self,
            t: gst::StateChange,
        ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
            let result = self.parent_change_state(t)?;
            if matches!(
                t,
                gst::StateChange::PausedToReady | gst::StateChange::ReadyToNull
            ) {
                self.state.lock().unwrap().clear();
            }
            Ok(result)
        }
    }
    impl Filter {
        pub fn configure(&self, mode: i32, auto: bool, report: Shared) {
            *self.config.lock().unwrap() = Config { mode, auto, report };
        }
        fn event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
            match event.view() {
                gst::EventView::Caps(e) => {
                    let Ok(info) = gst_video::VideoInfo::from_caps(e.caps()) else {
                        return false;
                    };
                    let mut state = self.state.lock().unwrap();
                    state.clear();
                    state.info = Some(info);
                    state.disabled = false;
                    // Variable presentation cadence: timestamps carry the exact 2x or
                    // 60-Hz grid, while the decoder retains its original frame rate.
                    let mut caps = e.caps().to_owned();
                    caps.make_mut()
                        .structure_mut(0)
                        .unwrap()
                        .set("framerate", gst::Fraction::new(0, 1));
                    drop(state);
                    return self.src.push_event(gst::event::Caps::new(&caps));
                }
                gst::EventView::FlushStart(_)
                | gst::EventView::FlushStop(_)
                | gst::EventView::Segment(_) => {
                    self.state.lock().unwrap().reset_pair();
                }
                gst::EventView::Eos(_) => {
                    let tail = {
                        let mut state = self.state.lock().unwrap();
                        let tail = state.previous.take();
                        // The tail is an original source frame. Drain our read/copy
                        // before handing it to a different GPU presentation queue.
                        state.gpu = None;
                        tail
                    };
                    if let Some(tail) = tail {
                        let _ = self.src.push(tail);
                    }
                }
                _ => {}
            }
            gst::Pad::event_default(pad, Some(&*self.obj()), event)
        }
        fn block(config: &Config, state: &mut State, error: String) {
            state.clear();
            state.disabled = true;
            let mut r = config.report.lock().unwrap();
            r.mode = 0;
            r.blocked = true;
            r.texture_bytes = 0;
            r.status = error.clone();
            crate::diagnostics::record(
                "interpolation_fallback",
                serde_json::json!({"backend":"D3D12","error":error}),
            );
        }
        fn chain(&self, buffer: gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
            let config = self.config.lock().unwrap();
            let mut s = self.state.lock().unwrap();
            if s.disabled || config.mode == 0 {
                drop(s);
                drop(config);
                return self.src.push(buffer);
            }
            let Some(info) = s.info.clone() else {
                drop(s);
                drop(config);
                return self.src.push(buffer);
            };
            let fps = info.fps();
            config.report.lock().unwrap().source_fps = if fps.denom() > 0 {
                fps.numer() as f32 / fps.denom() as f32
            } else {
                0.
            };
            let Some(period) =
                policy::interval(info.width(), info.height(), fps.numer(), fps.denom())
            else {
                Self::block(&config,&mut s,"Interpolation needs a 12–30 fps source at 720p or lower; native playback restored".into());
                drop(s);
                drop(config);
                return self.src.push(buffer);
            };
            if buffer.flags().contains(gst::BufferFlags::DISCONT) {
                s.reset_pair();
            }
            let Some(previous) = s.previous.replace(buffer.clone()) else {
                return Ok(gst::FlowSuccess::Ok);
            };
            let pts = previous.pts().zip(buffer.pts());
            let Some((a, b)) =
                pts.filter(|(a, b)| policy::continuous(a.nseconds(), b.nseconds(), period))
            else {
                s.gpu = None;
                drop(s);
                drop(config);
                return self.src.push(previous);
            };
            let plan = policy::plan(a.nseconds(), b.nseconds(), config.mode);
            let fractions: Vec<f32> = plan.ticks[..plan.len].iter().map(|t| t.fraction).collect();
            let generated = (|| -> Result<(Vec<gst::Buffer>, u64)> {
                if s.gpu.is_none() {
                    s.gpu = Some(Gpu::new(&previous, &info)?);
                }
                s.gpu
                    .as_mut()
                    .unwrap()
                    .render(&previous, &buffer, &fractions)
            })();
            let (frames, ns) = match generated {
                Ok(v) => v,
                Err(e)
                    if e.to_string()
                        .starts_with("GPU commands did not complete safely") =>
                {
                    gst::element_imp_error!(self, gst::ResourceError::Failed, ("{e:#}"));
                    s.clear();
                    return Err(gst::FlowError::Error);
                }
                Err(e) => {
                    Self::block(&config, &mut s, format!("Interpolation off: {e:#}"));
                    drop(s);
                    drop(config);
                    self.src.push(previous)?;
                    return self.src.push(buffer);
                }
            };
            s.slow = if ns > 0 && u128::from(ns) > period.as_nanos() * 85 / 100 {
                s.slow.saturating_add(1)
            } else {
                0
            };
            let mut outputs = Vec::with_capacity(plan.len);
            let mut frames = frames.into_iter();
            for (i, t) in plan.ticks[..plan.len].iter().enumerate() {
                // Every presentation frame has private GPU output storage.
                // Never update a fence on GStreamer's shared source memory.
                let mut output = frames.next().unwrap();
                let end = plan
                    .ticks
                    .get(i + 1)
                    .filter(|_| i + 1 < plan.len)
                    .map_or(b.nseconds(), |v| v.pts);
                let out = output.make_mut();
                if i == 0 && previous.flags().contains(gst::BufferFlags::DISCONT) {
                    out.set_flags(gst::BufferFlags::DISCONT);
                }
                out.set_pts(gst::ClockTime::from_nseconds(t.pts));
                out.set_dts(None);
                out.set_duration(gst::ClockTime::from_nseconds(end - t.pts));
                outputs.push(output);
            }
            {
                let mut r = config.report.lock().unwrap();
                r.generated += fractions.iter().filter(|f| **f > 0.).count() as u64;
                r.gpu_ms = ns as f64 / 1e6;
                r.texture_bytes = u64::from(info.width()) * u64::from(info.height()) * 24;
                r.status = format!(
                    "D3D12 interpolation · {} generated frames · {:.2} ms GPU",
                    r.generated, r.gpu_ms
                );
            }
            if config.auto && s.slow >= 3 {
                Self::block(
                    &config,
                    &mut s,
                    "GPU cannot sustain interpolation; native playback restored".into(),
                );
            }
            drop(s);
            drop(config);
            for output in outputs {
                self.src.push(output)?;
            }
            Ok(gst::FlowSuccess::Ok)
        }
    }
}
glib::wrapper! {pub struct Filter(ObjectSubclass<imp::Filter>) @extends gst::Element,gst::Object;}
pub fn make(mode: i32, auto: bool) -> (gst::Element, Shared) {
    let report = Arc::new(Mutex::new(Report {
        mode,
        ..Default::default()
    }));
    let filter: Filter = glib::Object::builder().build();
    filter.imp().configure(mode, auto, report.clone());
    (filter.upcast(), report)
}

static CURRENT: Mutex<Option<Shared>> = Mutex::new(None);
static REMOTE: Mutex<serde_json::Value> = Mutex::new(serde_json::Value::Null);
pub fn publish(report: Shared) {
    *CURRENT.lock().unwrap() = Some(report);
}
pub fn inactive() {
    CURRENT.lock().unwrap().take();
    *REMOTE.lock().unwrap() = serde_json::Value::Null;
}
pub fn remote_report(report: &serde_json::Value) {
    *REMOTE.lock().unwrap() = report.clone();
}
pub fn latest_stats() -> serde_json::Value {
    if let Some(report) = CURRENT.lock().unwrap().as_ref() {
        let r = report.lock().unwrap();
        let mut v = serde_json::to_value(&*r).unwrap_or_default();
        v["active"] = (r.mode > 0 && !r.blocked && r.generated > 0).into();
        v["backend"] = "D3D12".into();
        v["cpu_video_readback"] = false.into();
        v["extra_texture_bytes"] = r.texture_bytes.into();
        return v;
    }
    let remote = REMOTE.lock().unwrap();
    if !remote.is_null() {
        remote.clone()
    } else {
        serde_json::json!({"mode":0,"active":false,"backend":"D3D12"})
    }
}

// Headless GPU validation for the Windows SSH test machine. The source generates
// its own textures. Only frame counts, PTS and GPU timings return to the CPU.
pub fn probe() -> Result<serde_json::Value> {
    gst::init()?;
    let mut results = Vec::new();
    for (mode, fps) in [(1, 24), (2, 24), (2, 25), (2, 30)] {
        let pipeline = gst::Pipeline::new();
        let source = gst::ElementFactory::make("d3d12testsrc")
            .property("num-buffers", fps + 1)
            .build()?;
        if let Some(spec) = source.find_property("pattern") {
            if let Some(class) = glib::EnumClass::with_type(spec.value_type()) {
                if class.values().iter().any(|v| v.nick() == "ball") {
                    source.set_property_from_str("pattern", "ball");
                }
            }
        }
        let caps=gst::ElementFactory::make("capsfilter").property("caps",format!("video/x-raw(memory:D3D12Memory),format=RGBA,width=320,height=180,framerate={fps}/1").parse::<gst::Caps>()?).build()?;
        let (effect, report) = make(mode, false);
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("enable-last-sample", false)
            .build()?;
        let points = Arc::new(Mutex::new(Vec::new()));
        let frames = points.clone();
        sink.static_pad("sink")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, p| {
                if let Some(b) = p.buffer() {
                    let mut pts = frames.lock().unwrap();
                    if pts.len() < 256 {
                        pts.push(b.pts().map(|p| p.nseconds()));
                    }
                }
                gst::PadProbeReturn::Ok
            });
        pipeline.add_many([&source, &caps, &effect, &sink])?;
        gst::Element::link_many([&source, &caps, &effect, &sink])?;
        pipeline.set_state(gst::State::Playing)?;
        let message = pipeline.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(90),
            &[gst::MessageType::Error, gst::MessageType::Eos],
        );
        let detail = report.lock().unwrap().clone();
        let pts = points.lock().unwrap().clone();
        pipeline.set_state(gst::State::Null)?;
        if let Some(m) = message {
            if let gst::MessageView::Error(e) = m.view() {
                bail!("GPU interpolation test: {} ({:?})", e.error(), e.debug());
            }
        } else {
            bail!("GPU interpolation test timed out");
        }
        let expected = if mode == 1 { 2 * fps + 1 } else { 61 };
        let monotonic = pts
            .windows(2)
            .all(|p| p[0].zip(p[1]).is_some_and(|(a, b)| b > a));
        let ok =
            detail.generated > 0 && !detail.blocked && pts.len() == expected as usize && monotonic;
        results.push(serde_json::json!({"ok":ok,"mode":mode,"source_fps":fps,"frames":pts.len(),"expected_frames":expected,"monotonic":monotonic,"report":detail}));
    }
    Ok(serde_json::json!({"ok":results.iter().all(|r|r["ok"]==true),"cases":results}))
}
