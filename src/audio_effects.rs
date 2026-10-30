//! Bounded, optional audio DSP. Steady-state processing reuses preallocated
//! buffers; file reads and FFT planning happen before streaming. Disabled effects
//! never enter the audio pipeline.
use anyhow::{Context, Result, ensure};
use gst::{glib, prelude::*};
use gst_base::{
    prelude::*,
    subclass::{base_transform::BaseTransformMode, prelude::*},
};
use rustfft::{Fft, FftPlanner, num_complex::Complex32 as C};
use serde::{Deserialize, Serialize};
use std::{
    io::{Cursor, Read},
    sync::{Arc, Mutex, OnceLock},
};

const RATE: usize = 48_000;
const BLOCK: usize = 512;
const FFT: usize = BLOCK * 2;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SECONDS: usize = 5;
const OXY: &[u8] = include_bytes!("../assets/audio/Oxy.irs");

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Config {
    pub delay: bool,
    pub channel: i32,
    pub delay_ms: f32,
    pub convolver: bool,
    pub source: i32,
    /// Empty selects the built-in impulse; external files are never copied.
    pub impulse: String,
    pub gain_db: f32,
}
// Read earlier two-slider preferences without retaining a common delay on
// both channels. New preferences always store one channel and one amount.
impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Saved {
            delay: bool,
            channel: i32,
            delay_ms: Option<f32>,
            left_ms: f32,
            right_ms: f32,
            convolver: bool,
            source: Option<i32>,
            impulse: String,
            gain_db: f32,
        }
        let saved = Saved::deserialize(deserializer)?;
        let (channel, amount) = if let Some(amount) = saved.delay_ms {
            (saved.channel, amount)
        } else {
            let difference =
                finite_clamp(saved.left_ms, 0., 20.) - finite_clamp(saved.right_ms, 0., 20.);
            (if difference < 0. { 1 } else { 0 }, difference.abs())
        };
        let mut config = Self {
            delay: saved.delay,
            channel,
            delay_ms: amount,
            convolver: saved.convolver,
            source: saved
                .source
                .unwrap_or(if saved.impulse.is_empty() { 0 } else { 1 }),
            impulse: saved.impulse,
            gain_db: saved.gain_db,
        };
        config.sanitize();
        Ok(config)
    }
}
fn finite_clamp(v: f32, low: f32, high: f32) -> f32 {
    if v.is_finite() {
        v.clamp(low, high)
    } else {
        0.
    }
}
impl Config {
    pub fn sanitize(&mut self) {
        self.source = self.source.clamp(0, 2);
        self.channel = self.channel.clamp(0, 1);
        self.delay_ms = finite_clamp(self.delay_ms, 0., 20.);
        self.gain_db = finite_clamp(self.gain_db, -24., 0.);
    }
    pub fn read(w: &crate::SetupWindow) -> Self {
        let mut s = Self {
            delay: w.get_audio_delay_enabled(),
            channel: w.get_audio_delay_channel(),
            delay_ms: w.get_audio_delay_ms(),
            convolver: w.get_audio_convolver_enabled(),
            source: w.get_audio_impulse_source(),
            impulse: w.get_audio_impulse_path().to_string(),
            gain_db: w.get_audio_convolver_gain(),
        };
        s.sanitize();
        s
    }
    fn selected_path(&self) -> Result<&str> {
        if self.source == 0 {
            return Ok("");
        }
        ensure!(!self.impulse.is_empty(), "Choose an impulse file first");
        Ok(&self.impulse)
    }
    pub fn validate(&self) -> Result<String> {
        validate(self.selected_path()?)
    }
    pub fn show(&self, w: &crate::SetupWindow) {
        w.set_audio_delay_enabled(self.delay);
        let mut delay = self.clone();
        delay.sanitize();
        w.set_audio_delay_channel(delay.channel);
        w.set_audio_delay_ms(delay.delay_ms);
        w.set_audio_impulse_status(
            if self.source == 0 {
                "Bundled Oxy · stereo · 48 kHz"
            } else {
                "External impulse — checked when applied"
            }
            .into(),
        );
        w.set_audio_impulse_source(self.source);
        w.set_audio_convolver_enabled(self.convolver);
        w.set_audio_impulse_path(self.impulse.clone().into());
        w.set_audio_convolver_gain(finite_clamp(self.gain_db, -24., 0.));
    }
}
struct Impulse {
    channels: Vec<Vec<f32>>,
    rate: usize,
}
fn load(path: &str) -> Result<Impulse> {
    let bytes = if path.is_empty() {
        OXY.to_vec()
    } else {
        let metadata = std::fs::metadata(path).context("Impulse file is missing or unreadable")?;
        ensure!(metadata.is_file(), "Select a regular impulse file");
        ensure!(metadata.len() <= MAX_BYTES, "Impulse file exceeds 8 MiB");
        let file = std::fs::File::open(path).context("Impulse file is missing or unreadable")?;
        ensure!(
            file.metadata()?.len() <= MAX_BYTES,
            "Impulse file exceeds 8 MiB"
        );
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "Impulse file exceeds 8 MiB"
        );
        bytes
    };
    ensure!(
        bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE"),
        "Select a WAV-format .irs or .wav impulse"
    );
    let mut reader =
        hound::WavReader::new(Cursor::new(bytes)).context("Invalid impulse WAV header")?;
    let spec = reader.spec();
    let rate = spec.sample_rate as usize;
    ensure!(
        (1..=2).contains(&spec.channels),
        "Impulse must be mono or stereo"
    );
    ensure!(
        (8_000..=192_000).contains(&rate),
        "Impulse sample rate must be 8–192 kHz"
    );
    let samples = reader.len() as usize;
    let channels = spec.channels as usize;
    ensure!(
        samples > 0 && samples.is_multiple_of(channels),
        "Impulse contains incomplete or empty audio"
    );
    ensure!(
        samples / channels <= rate * MAX_SECONDS,
        "Impulse must be at most 5 seconds"
    );
    let values: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => {
            ensure!(
                spec.bits_per_sample == 32,
                "Floating-point impulses must use 32-bit samples"
            );
            reader
                .samples::<f32>()
                .collect::<std::result::Result<_, _>>()?
        }
        hound::SampleFormat::Int => {
            ensure!(
                [8, 16, 24, 32].contains(&spec.bits_per_sample),
                "Unsupported PCM bit depth"
            );
            let scale = 2f64.powi(i32::from(spec.bits_per_sample) - 1);
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| (v as f64 / scale) as f32))
                .collect::<std::result::Result<_, _>>()?
        }
    };
    ensure!(values.len() == samples, "Impulse audio data is truncated");
    ensure!(
        values.iter().all(|v| v.is_finite() && v.abs() <= 64.),
        "Impulse contains invalid or excessive sample values"
    );
    ensure!(values.iter().any(|v| v.abs() > 1e-12), "Impulse is silent");
    let mut split = vec![Vec::with_capacity(samples / channels); channels];
    for frame in values.chunks_exact(channels) {
        for (channel, value) in frame.iter().enumerate() {
            split[channel].push(*value);
        }
    }
    Ok(Impulse {
        channels: split,
        rate,
    })
}
pub fn validate(path: &str) -> Result<String> {
    let ir = load(path)?;
    Ok(format!(
        "{} · {} · {} Hz · {:.0} ms",
        if path.is_empty() {
            "Bundled Oxy"
        } else {
            "Valid impulse"
        },
        if ir.channels.len() == 1 {
            "mono"
        } else {
            "stereo"
        },
        ir.rate,
        ir.channels[0].len() as f64 * 1000. / ir.rate as f64
    ))
}
// Offline, bounded windowed-sinc resampling. The phase table avoids per-sample
// trigonometry, and the reduced cutoff prevents aliasing when downsampling.
fn resample(input: &[f32], rate: usize) -> Vec<f32> {
    if rate == RATE {
        return input.to_vec();
    }
    const TAPS: usize = 64;
    const PHASES: usize = 1024;
    let cutoff = (RATE as f64 / rate as f64).min(1.) * 0.95;
    let mut table = vec![[0f32; TAPS]; PHASES];
    for (phase, weights) in table.iter_mut().enumerate() {
        let mut sum = 0.;
        for (tap, weight) in weights.iter_mut().enumerate() {
            let x = tap as f64 - (TAPS / 2 - 1) as f64 - phase as f64 / PHASES as f64;
            let a = std::f64::consts::PI * x * cutoff;
            let sinc = if a.abs() < 1e-9 { 1. } else { a.sin() / a };
            let window = 0.42
                + 0.5 * (std::f64::consts::PI * x / 32.).cos()
                + 0.08 * (std::f64::consts::PI * x / 16.).cos();
            *weight = (sinc * cutoff * window) as f32;
            sum += *weight;
        }
        for w in weights {
            *w /= sum;
        }
    }
    let length = (input.len() * RATE).div_ceil(rate);
    (0..length)
        .map(|i| {
            let position = i as f64 * rate as f64 / RATE as f64;
            let center = position.floor() as isize;
            let phase = ((position.fract() * PHASES as f64) as usize).min(PHASES - 1);
            table[phase]
                .iter()
                .enumerate()
                .map(|(tap, w)| {
                    let index = center + tap as isize - (TAPS / 2 - 1) as isize;
                    if index >= 0 {
                        input.get(index as usize).copied().unwrap_or(0.) * w
                    } else {
                        0.
                    }
                })
                .sum()
        })
        .collect()
}
struct Channel {
    // Flat allocations avoid hundreds of small retained heap blocks when a
    // long impulse is disabled, and keep each partition contiguous for the FFT.
    kernel: Vec<C>,
    history: Vec<C>,
    partitions: usize,
    work: Vec<C>,
    scratch: Vec<C>,
    overlap: Vec<f32>,
    output: Vec<f32>,
    input: Vec<f32>,
    head: usize,
}
struct Convolver {
    channels: [Channel; 2],
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    cursor: usize,
    taps: usize,
    resample_delay: usize,
}
impl Convolver {
    fn new(ir: Impulse) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(FFT);
        let inverse = planner.plan_fft_inverse(FFT);
        let scratch = forward
            .get_inplace_scratch_len()
            .max(inverse.get_inplace_scratch_len());
        // An impulse is a discrete filter, not an ordinary audio recording.
        // Preserve its integral (DC gain) when changing sample rates. Retain
        // the sinc's full ringing rather than cutting its negative-time half;
        // the added causal delay is compensated in output PTS/latency below.
        let resample_delay = if ir.rate == RATE {
            0
        } else {
            (32 * RATE + ir.rate / 2) / ir.rate
        };
        let kernels: Vec<_> = ir
            .channels
            .iter()
            .map(|v| {
                if ir.rate == RATE {
                    return v.clone();
                }
                let mut padded = vec![0.; v.len() + 64];
                padded[32..32 + v.len()].copy_from_slice(v);
                let mut kernel = resample(&padded, ir.rate);
                for value in &mut kernel {
                    *value *= ir.rate as f32 / RATE as f32;
                }
                kernel
            })
            .collect();
        let taps = kernels[0].len();
        let channels = std::array::from_fn(|c| {
            let mut temp = vec![C::default(); scratch];
            let coefficients = &kernels[c.min(kernels.len() - 1)];
            let count = coefficients.len().div_ceil(BLOCK);
            let mut kernel = vec![C::default(); count * FFT];
            for (spectrum, part) in kernel.chunks_exact_mut(FFT).zip(coefficients.chunks(BLOCK)) {
                for (v, sample) in spectrum.iter_mut().zip(part) {
                    v.re = *sample;
                }
                forward.process_with_scratch(spectrum, &mut temp);
            }
            Channel {
                kernel,
                history: vec![C::default(); count * FFT],
                partitions: count,
                work: vec![C::default(); FFT],
                scratch: vec![C::default(); scratch],
                overlap: vec![0.; BLOCK],
                output: vec![0.; BLOCK],
                input: vec![0.; BLOCK],
                head: 0,
            }
        });
        Self {
            channels,
            forward,
            inverse,
            cursor: 0,
            taps,
            resample_delay,
        }
    }
    fn sample(&mut self, input: [f32; 2]) -> [f32; 2] {
        let output = std::array::from_fn(|c| self.channels[c].output[self.cursor]);
        for (c, value) in input.into_iter().enumerate() {
            self.channels[c].input[self.cursor] = value;
        }
        self.cursor += 1;
        if self.cursor == BLOCK {
            self.cursor = 0;
            for c in &mut self.channels {
                let spectrum = &mut c.history[c.head * FFT..(c.head + 1) * FFT];
                spectrum.fill(C::default());
                for (v, sample) in spectrum.iter_mut().zip(&c.input) {
                    v.re = *sample;
                }
                self.forward.process_with_scratch(spectrum, &mut c.scratch);
                c.work.fill(C::default());
                for (p, kernel) in c.kernel.chunks_exact(FFT).enumerate() {
                    let start = ((c.head + c.partitions - p) % c.partitions) * FFT;
                    let source = &c.history[start..start + FFT];
                    for ((sum, a), b) in c.work.iter_mut().zip(source).zip(kernel) {
                        *sum += *a * *b;
                    }
                }
                self.inverse
                    .process_with_scratch(&mut c.work, &mut c.scratch);
                for i in 0..BLOCK {
                    c.output[i] = c.work[i].re / FFT as f32 + c.overlap[i];
                    c.overlap[i] = c.work[i + BLOCK].re / FFT as f32;
                }
                c.head = (c.head + 1) % c.partitions;
            }
        }
        output
    }
    fn reset(&mut self) {
        self.cursor = 0;
        for c in &mut self.channels {
            c.history.fill(C::default());
            c.output.fill(0.);
            c.overlap.fill(0.);
            c.input.fill(0.);
            c.head = 0;
        }
    }
}
struct Delay {
    values: Vec<f32>,
    cursor: usize,
}
impl Delay {
    fn sample(&mut self, value: f32) -> f32 {
        if self.values.is_empty() {
            return value;
        }
        let old = self.values[self.cursor];
        self.values[self.cursor] = value;
        self.cursor = (self.cursor + 1) % self.values.len();
        old
    }
}
struct Processor {
    convolution: Option<Convolver>,
    delay: [Delay; 2],
    gain: f32,
    priming: usize,
    last_end: Option<gst::ClockTime>,
    received: bool,
}
impl Processor {
    fn latency(&self) -> usize {
        self.convolution
            .as_ref()
            .map_or(0, |c| BLOCK + c.resample_delay)
    }
    fn tail(&self) -> usize {
        // resample_delay is already present in the padded kernel: do not add it
        // a second time to the physical tail when compensating output PTS.
        self.convolution.as_ref().map_or(0, |c| BLOCK + c.taps - 1)
            + self.delay.iter().map(|d| d.values.len()).max().unwrap_or(0)
    }
    fn reset(&mut self) {
        if let Some(c) = &mut self.convolution {
            c.reset();
        }
        for d in &mut self.delay {
            d.values.fill(0.);
            d.cursor = 0;
        }
        self.priming = self.latency();
        self.last_end = None;
        self.received = false;
    }
    fn sample(&mut self, input: [f32; 2]) -> [f32; 2] {
        let v = self.convolution.as_mut().map_or(input, |c| c.sample(input));
        std::array::from_fn(|c| {
            let v = self.delay[c].sample(v[c]) * self.gain;
            if v.is_finite() { v } else { 0. }
        })
    }
    fn process(&mut self, buffer: &mut gst::BufferRef) -> Result<gst::FlowSuccess, gst::FlowError> {
        let frames = buffer.size() / 8;
        if buffer.size() % 8 != 0 {
            return Err(gst::FlowError::Error);
        }
        let pts = buffer.pts();
        self.last_end = pts.map(|p| p + time(frames));
        self.received |= frames > 0;
        {
            let mut map = buffer.map_writable().map_err(|_| gst::FlowError::Error)?;
            for f in map.as_mut_slice().chunks_exact_mut(8) {
                let sample = [
                    f32::from_le_bytes(f[..4].try_into().unwrap()),
                    f32::from_le_bytes(f[4..].try_into().unwrap()),
                ];
                let out = self.sample(sample);
                f[..4].copy_from_slice(&out[0].to_le_bytes());
                f[4..].copy_from_slice(&out[1].to_le_bytes());
            }
        }
        let skip = self.priming.min(frames);
        self.priming -= skip;
        if skip == frames {
            return Ok(gst_base::BASE_TRANSFORM_FLOW_DROPPED);
        }
        if skip > 0 {
            buffer
                .map_writable()
                .map_err(|_| gst::FlowError::Error)?
                .as_mut_slice()
                .copy_within(skip * 8.., 0);
            buffer.set_size((frames - skip) * 8);
            // Interleaved audio is described by the negotiated caps. A copied
            // AudioMeta would still describe the original, larger buffer.
            buffer.foreach_meta_mut(|meta| {
                std::ops::ControlFlow::Continue(if meta.api().name() == "GstAudioMetaAPI" {
                    gst::buffer::BufferMetaForeachAction::Remove
                } else {
                    gst::buffer::BufferMetaForeachAction::Keep
                })
            });
        }
        buffer.set_pts(pts.map(|p| (p + time(skip)).saturating_sub(time(self.latency()))));
        buffer.set_dts(None);
        buffer.set_duration(time(frames - skip));
        buffer.set_offset(u64::MAX);
        buffer.set_offset_end(u64::MAX);
        Ok(gst::FlowSuccess::Ok)
    }
}
fn time(frames: usize) -> gst::ClockTime {
    gst::ClockTime::from_nseconds((frames as u64 * 1_000_000_000) / RATE as u64)
}
fn prepare(config: &Config) -> (Option<Processor>, Option<String>) {
    let mut config = config.clone();
    config.sanitize();
    let (convolution, error) = if config.convolver {
        match config.selected_path().and_then(load) {
            Ok(ir) => (Some(Convolver::new(ir)), None),
            Err(e) => (None, Some(format!("Convolution off: {e:#}"))),
        }
    } else {
        (None, None)
    };
    let delay = std::array::from_fn(|c| Delay {
        values: vec![
            0.;
            if config.delay && c == config.channel as usize {
                (config.delay_ms * RATE as f32 / 1000.).round() as usize
            } else {
                0
            }
        ],
        cursor: 0,
    });
    if convolution.is_none() && delay.iter().all(|d| d.values.is_empty()) {
        return (None, error);
    }
    let gain = if convolution.is_some() {
        10f32.powf(config.gain_db / 20.)
    } else {
        1.
    };
    let mut p = Processor {
        convolution,
        delay,
        gain,
        priming: 0,
        last_end: None,
        received: false,
    };
    p.reset();
    (Some(p), error)
}
mod imp {
    use super::*;
    #[derive(Default)]
    pub struct Effects {
        pub(super) processor: Mutex<Option<Processor>>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Effects {
        const NAME: &'static str = "MochuneAudioEffects";
        type Type = super::Effects;
        type ParentType = gst_base::BaseTransform;
    }
    impl ObjectImpl for Effects {}
    impl GstObjectImpl for Effects {}
    impl ElementImpl for Effects {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: OnceLock<gst::subclass::ElementMetadata> = OnceLock::new();
            Some(META.get_or_init(|| {
                gst::subclass::ElementMetadata::new(
                    "Mochune audio effects",
                    "Filter/Effect/Audio",
                    "Independent channel delay and partitioned convolution",
                    "Mochune",
                )
            }))
        }
        fn pad_templates() -> &'static [gst::PadTemplate] {
            static TEMPLATES: OnceLock<Vec<gst::PadTemplate>> = OnceLock::new();
            TEMPLATES.get_or_init(|| {
                let caps = gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("rate", RATE as i32)
                    .field("channels", 2i32)
                    .field("layout", "interleaved")
                    .build();
                [gst::PadDirection::Sink, gst::PadDirection::Src]
                    .into_iter()
                    .map(|d| {
                        gst::PadTemplate::new(
                            if d == gst::PadDirection::Sink {
                                "sink"
                            } else {
                                "src"
                            },
                            d,
                            gst::PadPresence::Always,
                            &caps,
                        )
                        .unwrap()
                    })
                    .collect()
            })
        }
    }
    impl BaseTransformImpl for Effects {
        const MODE: BaseTransformMode = BaseTransformMode::AlwaysInPlace;
        const PASSTHROUGH_ON_SAME_CAPS: bool = false;
        const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;
        fn transform_ip(
            &self,
            buffer: &mut gst::BufferRef,
        ) -> Result<gst::FlowSuccess, gst::FlowError> {
            let mut lock = self.processor.lock().unwrap();
            let p = lock.as_mut().ok_or(gst::FlowError::Error)?;
            if buffer.flags().contains(gst::BufferFlags::DISCONT) {
                p.reset();
            }
            p.process(buffer)
        }
        fn query(&self, direction: gst::PadDirection, query: &mut gst::QueryRef) -> bool {
            let result = BaseTransformImplExt::parent_query(self, direction, query);
            if result
                && direction == gst::PadDirection::Src
                && let gst::QueryViewMut::Latency(q) = query.view_mut()
            {
                let extra = time(
                    self.processor
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map_or(0, Processor::latency),
                );
                let (live, min, max) = q.result();
                q.set(live, min + extra, max.map(|v| v + extra));
            }
            result
        }
        fn sink_event(&self, event: gst::Event) -> bool {
            if matches!(
                event.view(),
                gst::EventView::FlushStop(_) | gst::EventView::Segment(_)
            ) {
                if let Some(p) = self.processor.lock().unwrap().as_mut() {
                    p.reset();
                }
            }
            if matches!(event.view(), gst::EventView::Eos(_)) {
                let (mut remaining, mut pts) = {
                    let lock = self.processor.lock().unwrap();
                    lock.as_ref()
                        .filter(|p| p.received)
                        .map_or((0, None), |p| (p.tail(), p.last_end))
                };
                // Bounded chunks, respecting downstream backpressure, preserve
                // the last block and reverb/delay tails even for short tracks.
                while remaining > 0 {
                    let count = remaining.min(2048);
                    remaining -= count;
                    let mut buffer = gst::Buffer::with_size(count * 8).unwrap();
                    let b = buffer.get_mut().unwrap();
                    b.set_pts(pts);
                    b.map_writable().unwrap().as_mut_slice().fill(0);
                    let result = self.processor.lock().unwrap().as_mut().unwrap().process(b);
                    pts = pts.map(|p| p + time(count));
                    match result {
                        Ok(s) if s == gst_base::BASE_TRANSFORM_FLOW_DROPPED => {}
                        Ok(_) => {
                            if self.obj().src_pad().push(buffer).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
            self.parent_sink_event(event)
        }
    }
}
glib::wrapper! { pub struct Effects(ObjectSubclass<imp::Effects>) @extends gst_base::BaseTransform, gst::Element, gst::Object; }
pub fn element(config: &Config) -> Option<gst::Element> {
    let (processor, error) = prepare(config);
    if let Some(error) = error {
        crate::diagnostics::record(
            "audio_effects_fallback",
            serde_json::json!({"reason":error}),
        );
    }
    processor.map(|p| {
        let e: Effects = glib::Object::new();
        *e.imp().processor.lock().unwrap() = Some(p);
        e.upcast()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };
    struct Temp(PathBuf);
    impl Temp {
        fn new(bytes: &[u8]) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "mochune-impulse-{}-{}.irs",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }
        fn path(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn wav(channels: u16, rate: u32, data: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = hound::WavWriter::new(
                Cursor::new(&mut bytes),
                hound::WavSpec {
                    channels,
                    sample_rate: rate,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                },
            )
            .unwrap();
            for x in data {
                writer.write_sample(*x).unwrap();
            }
            writer.finalize().unwrap();
        }
        bytes
    }
    #[test]
    fn off_is_unallocated_and_delay_is_independent_and_bounded() {
        assert!(prepare(&Config::default()).0.is_none());
        let config = Config {
            delay: true,
            channel: 1,
            delay_ms: 200.,
            ..Default::default()
        };
        let mut p = prepare(&config).0.unwrap();
        assert!(p.convolution.is_none());
        assert_eq!(p.delay[1].values.len(), 960);
        assert_eq!(p.sample([0.25, 0.5]), [0.25, 0.]);
        for _ in 1..960 {
            assert_eq!(p.sample([0., 0.]), [0., 0.]);
        }
        assert_eq!(p.sample([0., 0.]), [0., 0.5]);
    }
    #[test]
    fn delay_can_only_affect_one_channel_and_old_settings_migrate() {
        for channel in [-100, 0, 1, 100] {
            let p = prepare(&Config {
                delay: true,
                channel,
                delay_ms: 20.,
                ..Default::default()
            })
            .0
            .unwrap();
            assert_eq!(p.delay.iter().filter(|d| !d.values.is_empty()).count(), 1);
        }
        let config: Config =
            serde_json::from_value(serde_json::json!({"delay":true,"left_ms":20.,"right_ms":7.}))
                .unwrap();
        assert_eq!((config.channel, config.delay_ms), (0, 13.));
        let value = serde_json::to_value(config).unwrap();
        assert!(value.get("left_ms").is_none() && value.get("right_ms").is_none());
        let config: Config = serde_json::from_value(serde_json::json!({"delay":true,"channel":1,"delay_ms":5.,"left_ms":20.,"right_ms":20.})).unwrap();
        let p = prepare(&config).0.unwrap();
        assert!(p.delay[0].values.is_empty());
        assert_eq!(p.delay[1].values.len(), 240);
    }
    #[test]
    fn partitioned_stereo_convolution_matches_direct_reference() {
        let n = 3200;
        let mut kernels = vec![vec![0.; 1100]; 2];
        kernels[0][0] = 0.6;
        kernels[0][511] = 0.2;
        kernels[0][1099] = -0.3;
        kernels[1][1] = 0.4;
        kernels[1][512] = -0.15;
        kernels[1][1050] = 0.1;
        let mut c = Convolver::new(Impulse {
            channels: kernels.clone(),
            rate: RATE,
        });
        let input: Vec<[f32; 2]> = (0..n)
            .map(|i| {
                [
                    (i as f32 * 0.13).sin() * 0.1,
                    (i as f32 * 0.07).cos() * 0.05,
                ]
            })
            .collect();
        for i in 0..n + 1100 + BLOCK {
            let out = c.sample(input.get(i).copied().unwrap_or([0.; 2]));
            for channel in 0..2 {
                let expected: f32 = if i < BLOCK {
                    0.
                } else {
                    kernels[channel]
                        .iter()
                        .enumerate()
                        .filter_map(|(k, h)| {
                            (i - BLOCK)
                                .checked_sub(k)
                                .and_then(|index| input.get(index))
                                .map(|v| v[channel] * h)
                        })
                        .sum()
                };
                assert!(
                    (out[channel] - expected).abs() < 2e-5,
                    "sample {i} channel {channel}: {} != {expected}",
                    out[channel]
                );
            }
        }
    }
    #[test]
    fn validation_rejects_corrupt_silent_nonfinite_long_and_missing_files() {
        assert!(validate("").unwrap().contains("48000"));
        assert!(validate("/missing/mochune-impulse.irs").is_err());
        for bytes in [
            b"invalid".to_vec(),
            wav(1, 48_000, &[0.; 32]),
            wav(1, 48_000, &[f32::NAN, 1.]),
            wav(3, 48_000, &[1., 1., 1.]),
            wav(1, 8_000, &vec![1.; 8_000 * 5 + 1]),
        ] {
            let file = Temp::new(&bytes);
            assert!(validate(&file.path()).is_err());
        }
        let file = Temp::new(&[]);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file.0)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(validate(&file.path()).is_err());
    }
    #[test]
    fn impulse_resampling_preserves_dc_and_rejects_aliasing() {
        for rate in [8000, 44100, 96000, 192000] {
            let output = resample(&vec![0.25; rate / 10], rate);
            assert_eq!(output.len(), 4800);
            assert!(
                output[256..output.len() - 256]
                    .iter()
                    .all(|v| (*v - 0.25).abs() < 1e-5)
            );
        }
        let output = resample(
            &(0..9600)
                .map(|i| if i % 2 == 0 { 1. } else { -1. })
                .collect::<Vec<_>>(),
            96000,
        );
        assert!(
            output[128..output.len() - 128]
                .iter()
                .all(|v| v.abs() < 0.02)
        );
    }
    #[test]
    fn resampled_impulses_preserve_gain_and_declare_extra_latency() {
        for rate in [8000, 44100, 48000, 96000, 192000] {
            let c = Convolver::new(Impulse {
                channels: vec![vec![1.]],
                rate,
            });
            let dc: f32 = c.channels[0]
                .kernel
                .chunks_exact(FFT)
                .map(|part| part[0].re)
                .sum();
            assert!(
                (dc - 1.).abs() < 0.003,
                "rate {rate} changed impulse DC gain to {dc}"
            );
            assert_eq!(
                c.resample_delay,
                if rate == RATE {
                    0
                } else {
                    (32 * RATE + rate / 2) / rate
                }
            );
        }
    }
    #[test]
    fn reset_removes_old_audio_and_missing_convolver_keeps_delay() {
        let (mut p, error) = prepare(&Config {
            delay: true,
            channel: 0,
            delay_ms: 1.,
            convolver: true,
            source: 1,
            impulse: "/missing/ir.irs".into(),
            ..Default::default()
        });
        assert!(error.is_some());
        assert!(p.take().unwrap().convolution.is_none());
        let mut p = prepare(&Config {
            delay: true,
            channel: 1,
            delay_ms: 20.,
            convolver: true,
            ..Default::default()
        })
        .0
        .unwrap();
        for _ in 0..1900 {
            p.sample([0.2, 0.3]);
        }
        p.reset();
        for _ in 0..9000 {
            let v = p.sample([0.; 2]);
            assert!(v[0].abs() < 1e-7 && v[1].abs() < 1e-7);
        }
    }
    #[test]
    fn real_gstreamer_preserves_short_buffers_timestamps_and_eos_tail() {
        gst::init().unwrap();
        let mut ir = vec![0.; 1400];
        ir[0] = 1.;
        ir[1] = 1.;
        ir[1200] = 0.1;
        ir[1201] = 0.2;
        let file = Temp::new(&wav(2, 48_000, &ir));
        let effect = element(&Config {
            convolver: true,
            source: 1,
            impulse: file.path(),
            delay: true,
            channel: 1,
            delay_ms: 7. / 48.,
            ..Default::default()
        })
        .unwrap();
        let src = gst_app::AppSrc::builder()
            .caps(
                &gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("rate", 48000i32)
                    .field("channels", 2i32)
                    .field("layout", "interleaved")
                    .build(),
            )
            .format(gst::Format::Time)
            .build();
        let sink = gst_app::AppSink::builder().sync(false).build();
        let pipe = gst::Pipeline::new();
        pipe.add_many([src.upcast_ref(), &effect, sink.upcast_ref()])
            .unwrap();
        gst::Element::link_many([src.upcast_ref(), &effect, sink.upcast_ref()]).unwrap();
        pipe.set_state(gst::State::Playing).unwrap();
        let mut position = 0;
        for frames in [170, 440, 101, 789] {
            let mut bytes = vec![0; frames * 8];
            if position == 0 {
                bytes[..4].copy_from_slice(&0.5f32.to_le_bytes());
                bytes[4..8].copy_from_slice(&0.25f32.to_le_bytes());
            }
            let mut buffer = gst::Buffer::from_mut_slice(bytes);
            let b = buffer.get_mut().unwrap();
            b.set_pts(time(position));
            b.set_duration(time(frames));
            src.push_buffer(buffer).unwrap();
            position += frames;
        }
        src.end_of_stream().unwrap();
        let mut output = Vec::<[f32; 2]>::new();
        let mut expected_end = 0u64;
        while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(3)) {
            let b = sample.buffer().unwrap();
            let pts = b.pts().unwrap().nseconds();
            assert!(
                pts.abs_diff(expected_end) <= 2,
                "noncontiguous timestamps: {pts} vs {expected_end}"
            );
            let map = b.map_readable().unwrap();
            for f in map.as_slice().chunks_exact(8) {
                output.push([
                    f32::from_le_bytes(f[..4].try_into().unwrap()),
                    f32::from_le_bytes(f[4..].try_into().unwrap()),
                ]);
            }
            expected_end = pts + b.duration().unwrap().nseconds();
        }
        let eos = sink.is_eos();
        let _ = pipe.set_state(gst::State::Null);
        assert!(eos, "effects pipeline stalled");
        assert_eq!(output.len(), 1500 + 699 + 7);
        for (i, v) in output.iter().enumerate() {
            let a = if i == 0 {
                0.5
            } else if i == 600 {
                0.05
            } else {
                0.
            };
            let b = if i == 7 {
                0.25
            } else if i == 607 {
                0.05
            } else {
                0.
            };
            assert!(
                (v[0] - a).abs() < 2e-5 && (v[1] - b).abs() < 2e-5,
                "incorrect sample {i}: {v:?}"
            );
        }
    }
}
