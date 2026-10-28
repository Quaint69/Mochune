//! Sparse post-start starvation recovery. No speculative requests or raw frames.
use super::*;
use std::time::{Duration, Instant};
const GRACE: Duration = Duration::from_secs(3);
const STALE: Duration = Duration::from_millis(500);
const COOLDOWN: Duration = Duration::from_secs(10);
#[derive(Default)]
pub(super) struct Recovery {
    pub(super) observe_only: bool,
    plans: Vec<(gst::Element, buffering::Plan)>,
    grown: bool,
    delivery: Vec<(gst::Element, Arc<Mutex<delivery::Meter>>)>,
    resume_at: Option<f64>,
    pub resync: Option<f64>,
    eligible: Option<Instant>,
    changed: Option<Instant>,
    pts: Option<f64>,
    checked: Option<Instant>,
    last_incident: Option<Instant>,
    refill: Option<Instant>,
    queues: Vec<(gst::Element, u32, u32)>,
    pub last_event: Option<serde_json::Value>,
    sequence: u64,
    #[cfg(any(target_os = "windows", test))]
    drop_sample: Option<(Instant, u64, u64)>,
    #[cfg(any(target_os = "windows", test))]
    last_drop_log: Option<Instant>,
}
impl Recovery {
    pub fn track(&mut self, source: gst::Element, plan: buffering::Plan) {
        let meter = Arc::new(Mutex::new(delivery::Meter::default()));
        let input = meter.clone();
        source.connect("source-setup", false, move |args| {
            if let Ok(source) = args[1].get::<gst::Element>()
                && let Some(pad) = source.static_pad("src")
            {
                let input = input.clone();
                pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    if let Some(buffer) = info.buffer() {
                        input.lock().unwrap().arrived(Instant::now(), buffer.size());
                    }
                    gst::PadProbeReturn::Ok
                });
            }
            None
        });
        self.delivery.push((source.clone(), meter));
        self.plans.push((source, plan));
    }
    pub fn input_recently_active(&self, grace: Duration) -> bool {
        self.delivery
            .iter()
            .any(|(_, m)| m.lock().unwrap().recent(Instant::now(), grace))
    }
    fn refill_seconds(&self, queue: &gst::Element, required_bps: f64) -> f64 {
        let measured = self
            .delivery
            .iter()
            .find(|(s, _)| queue.has_as_ancestor(s))
            .and_then(|(_, m)| m.lock().unwrap().rate());
        delivery::refill_seconds(required_bps, measured)
    }
    fn slow_start(&self, sources: &[gst::Element]) -> bool {
        sources.iter().any(|source| {
            let measured = self
                .delivery
                .iter()
                .find(|(s, _)| s == source)
                .and_then(|(_, m)| m.lock().unwrap().rate());
            let Some(measured) = measured else {
                return false;
            };
            let Some(bin) = source.downcast_ref::<gst::Bin>() else {
                return false;
            };
            bin.iterate_recurse().into_iter().flatten().any(|q| {
                q.factory().is_some_and(|f| f.name() == "queue2")
                    && q.property::<u64>("ring-buffer-max-size") > 0
                    && measured < self.rate(&q) * 1.2
                    && (q.property::<u32>("current-level-bytes") as f64) < self.rate(&q) * 0.5
            })
        })
    }
    pub fn expand_after_picture(&mut self) {
        if !self.grown && self.plans.iter().all(|(source, plan)| plan.grow(source)) {
            self.grown = true;
            self.record("compressed_buffer_expanded", self.budgets());
            let sources = self.plans.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>();
            self.record("compressed_buffer_capacity", self.queue_health(&sources));
        }
    }
    /// READY tears down uridecodebin's internal queue2 instances. A new queue
    /// starts small again, so re-arm expansion without changing the ring budget.
    pub fn pipeline_rebuilt(&mut self) {
        self.cancel();
        self.grown = false;
        for (_, meter) in &self.delivery {
            *meter.lock().unwrap() = delivery::Meter::default();
        }
        self.record("compressed_buffer_rearmed", self.budgets());
    }
    pub fn budgets(&self) -> serde_json::Value {
        serde_json::json!(self.plans.iter().map(|(_, p)| p).collect::<Vec<_>>())
    }

    fn record(&mut self, event: &str, data: serde_json::Value) {
        self.sequence = self.sequence.wrapping_add(1);
        crate::diagnostics::record(event, data.clone());
        self.last_event =
            Some(serde_json::json!({"sequence":self.sequence,"event":event,"data":data}));
    }
    #[cfg(any(target_os = "windows", test))]
    pub fn observe_drops(&mut self, rendered: u64, dropped: u64) {
        let now = Instant::now();
        if self.active() || self.eligible.is_none_or(|t| now.duration_since(t) < GRACE) {
            self.drop_sample = None;
            return;
        }
        let Some((at, old_rendered, old_dropped)) = self.drop_sample else {
            self.drop_sample = Some((now, rendered, dropped));
            return;
        };
        let elapsed = now.duration_since(at);
        if elapsed < Duration::from_secs(2) {
            return;
        }
        self.drop_sample = Some((now, rendered, dropped));
        let r = rendered.saturating_sub(old_rendered);
        let d = dropped.saturating_sub(old_dropped);
        let total = r.saturating_add(d);
        if d >= 5
            && total >= 20
            && d as f64 / total as f64 >= 0.1
            && self
                .last_drop_log
                .is_none_or(|t| now.duration_since(t) >= COOLDOWN)
        {
            self.last_drop_log = Some(now);
            self.record("playback_frame_drop_burst",serde_json::json!({"window_ms":elapsed.as_millis(),"rendered":r,"dropped":d,"drop_fraction":d as f64/total as f64,"presented_fps":r as f64/elapsed.as_secs_f64()}));
        }
    }
    fn rate(&self, q: &gst::Element) -> f64 {
        let bitrate = q.property::<u64>("bitrate");
        if bitrate > 0 {
            return bitrate as f64 / 8.;
        }
        self.plans
            .iter()
            .find(|(source, _)| q.has_as_ancestor(source))
            .map(|(_, p)| p.bytes_per_second)
            .unwrap_or(64_000.)
    }
    fn queue_health(&self, sources: &[gst::Element]) -> serde_json::Value {
        let mut rows = Vec::new();
        for source in sources {
            if let Some(bin) = source.downcast_ref::<gst::Bin>() {
                for q in bin.iterate_recurse().into_iter().flatten() {
                    if q.factory().is_some_and(|f| f.name() == "queue2")
                        && q.property::<u64>("ring-buffer-max-size") > 0
                    {
                        let bytes = q.property::<u32>("current-level-bytes");
                        let rate = self.rate(&q);
                        rows.push(serde_json::json!({"source":source.name().to_string(),"bytes":bytes,"capacity":q.property::<u32>("max-size-bytes"),"seconds_estimate":bytes as f64/rate,"bitrate_bps":rate*8.}));
                    }
                }
            }
        }
        serde_json::json!(rows)
    }
    pub fn active(&self) -> bool {
        self.refill.is_some()
    }
    pub fn cancel(&mut self) {
        for (q, old, _) in self.queues.drain(..) {
            q.set_property("max-size-bytes", old);
        }
        self.refill = None;
        self.resume_at = None;
        self.resync = None;
        self.eligible = None;
        self.changed = None;
        self.pts = None;
        self.checked = None;
        #[cfg(any(target_os = "windows", test))]
        {
            self.drop_sample = None;
        }
    }
    pub fn poll(
        &mut self,
        pipeline: &gst::Pipeline,
        sources: &[gst::Element],
        active: bool,
        paused: bool,
        seeking: bool,
        pts: f64,
        position: Option<f64>,
        duration: Option<f64>,
    ) -> Result<bool> {
        if !active || paused || seeking {
            self.cancel();
            return Ok(false);
        }
        let now = Instant::now();
        if self
            .checked
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(150))
        {
            return Ok(self.active());
        }
        self.checked = Some(now);
        if let Some(start) = self.refill {
            let filled = self
                .queues
                .iter()
                .all(|(q, _, target)| q.property::<u32>("current-level-bytes") >= *target);
            let elapsed = now.duration_since(start);
            if (filled && elapsed >= Duration::from_millis(150))
                || elapsed >= Duration::from_secs(4)
            {
                for (q, old, _) in self.queues.drain(..) {
                    q.set_property("max-size-bytes", old);
                }
                self.refill = None;
                self.resync = self.resume_at.take();
                self.changed = Some(now);
                self.eligible = Some(now);
                if self.resync.is_none() {
                    pipeline.set_state(gst::State::Playing)?;
                }
                self.record(
                    "playback_refill_finished",
                    serde_json::json!({"wait_ms":elapsed.as_millis(),"filled":filled,"queues":self.queue_health(sources)}),
                );
            }
            return Ok(self.active());
        }
        if self.pts.is_none_or(|old| (old - pts).abs() > 0.0001) && pts.is_finite() && pts > 0. {
            self.pts = Some(pts);
            self.changed = Some(now);
            self.eligible.get_or_insert(now);
        }
        let eligible = self
            .eligible
            .is_some_and(|t| now.duration_since(t) >= GRACE);
        let stale = self.changed.is_some_and(|t| now.duration_since(t) >= STALE);
        let lag = position
            .filter(|p| p.is_finite())
            .map(|p| p - pts)
            .unwrap_or(0.);
        // Once delivery is measurable, cushion a slow startup before the
        // first starvation, rather than allowing the audio clock to run away.
        let slow_start = !eligible && self.eligible.is_some() && self.slow_start(sources);
        if (!eligible && !slow_start)
            || (!stale && lag < 0.8 && !slow_start)
            || self
                .last_incident
                .is_some_and(|t| now.duration_since(t) < COOLDOWN)
        {
            return Ok(false);
        }
        if position.zip(duration).is_some_and(|(p, d)| d - p < 1.5) {
            return Ok(false);
        }
        let mut queues = Vec::new();
        for source in sources {
            let Some(bin) = source.downcast_ref::<gst::Bin>() else {
                continue;
            };
            for q in bin.iterate_recurse().into_iter().flatten() {
                if q.factory().is_none_or(|f| f.name() != "queue2")
                    || q.property::<u64>("ring-buffer-max-size") == 0
                {
                    continue;
                }
                queues.push(q);
            }
        }
        let empty = !queues.is_empty()
            && queues.iter().any(|q| {
                let rate = self.rate(q);
                f64::from(q.property::<u32>("current-level-bytes")) < (rate * 0.3).max(4096.)
            });
        self.last_incident = Some(now);
        self.record(
            "playback_stall",
            serde_json::json!({"position":position,"video_pts":pts,"av_lag_seconds":lag,"no_frame_ms":self.changed.map(|t|now.duration_since(t).as_millis()),"input_starved":empty,"queues":self.queue_health(sources),"action":if empty&&(stale||slow_start)&&!self.observe_only {"brief_refill"}else{"observe_decoder_or_presentation"}}),
        );
        // A slow decoder/GPU with healthy input must not trigger more network reads.
        if self.observe_only || !empty || (!stale && !slow_start) {
            return Ok(false);
        }
        for q in queues {
            let old = q.property::<u32>("max-size-bytes");
            let bitrate = q.property::<u64>("bitrate");
            let rate = if bitrate > 0 {
                bitrate as f64 / 8.
            } else {
                self.rate(&q)
            };
            // Refill a short playable interval, not the full ahead budget.
            // A full/EOF queue may be smaller than one second of bitrate.
            let capacity = old.min(q.property::<u64>("ring-buffer-max-size") as u32);
            let target = (rate * self.refill_seconds(&q, rate))
                .clamp(4096., capacity.max(4096) as f64) as u32;
            q.set_property("max-size-bytes", old.max(target));
            self.queues.push((q, old, target));
        }
        self.refill = Some(now);
        self.resume_at = position.filter(|p| p.is_finite() && *p >= 0. && lag > 0.25);
        self.record("playback_refill_started",serde_json::json!({"resume_position":self.resume_at,"max_wait_ms":4000,"queues":self.queue_health(sources)}));
        pipeline.set_state(gst::State::Paused)?;
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rebuilt_queues_expand_again_without_growing_the_budget() {
        gst::init().unwrap();
        let source = gst::ElementFactory::make("uridecodebin").build().unwrap()
            .downcast::<gst::Bin>().unwrap();
        let plan = buffering::Plan::new(4800., 1_400_000., true);
        let make_queue = || gst::ElementFactory::make("queue2")
            .property("ring-buffer-max-size", plan.ring)
            .property("max-size-bytes", plan.initial(true) as u32).build().unwrap();
        let mut recovery = Recovery::default();
        recovery.track(source.clone().upcast(), plan);
        let old = make_queue();
        source.add(&old).unwrap();
        recovery.expand_after_picture();
        assert!(recovery.grown);
        assert_eq!(old.property::<u32>("max-size-bytes"), plan.forward);
        // An ordinary seek must preserve expanded queues and their contents.
        recovery.cancel();
        assert!(recovery.grown);
        assert_eq!(old.property::<u32>("max-size-bytes"), plan.forward);
        // Simulate READY disposing/replacing the internal queue, as on Windows.
        source.remove(&old).unwrap();
        let replacement = make_queue();
        source.add(&replacement).unwrap();
        recovery.pipeline_rebuilt();
        assert!(!recovery.grown);
        assert_eq!(replacement.property::<u32>("max-size-bytes"), 256 * 1024);
        recovery.expand_after_picture();
        assert_eq!(replacement.property::<u32>("max-size-bytes"), plan.forward);
        assert_eq!(replacement.property::<u64>("ring-buffer-max-size"), plan.ring);
    }
    #[test]
    fn observation_only_never_pauses_or_resizes_queues() {
        gst::init().unwrap();
        let pipeline = gst::Pipeline::new();
        pipeline.set_state(gst::State::Playing).unwrap();
        let source = gst::ElementFactory::make("uridecodebin")
            .build()
            .unwrap()
            .downcast::<gst::Bin>()
            .unwrap();
        let queue = gst::ElementFactory::make("queue2")
            .property("ring-buffer-max-size", 8u64 * 1024 * 1024)
            .property("max-size-bytes", 65536u32)
            .build()
            .unwrap();
        source.add(&queue).unwrap();
        let now = Instant::now();
        let mut r = Recovery {
            observe_only: true,
            eligible: Some(now - Duration::from_secs(4)),
            changed: Some(now - Duration::from_secs(2)),
            pts: Some(1.),
            ..Default::default()
        };
        assert!(
            !r.poll(
                &pipeline,
                &[source.upcast()],
                true,
                false,
                false,
                1.,
                Some(10.),
                Some(60.)
            )
            .unwrap()
        );
        assert_eq!(pipeline.current_state(), gst::State::Playing);
        assert_eq!(queue.property::<u32>("max-size-bytes"), 65536);
        assert!(!r.active());
        assert_eq!(
            r.last_event.as_ref().unwrap()["data"]["action"],
            "observe_decoder_or_presentation"
        );
        pipeline.set_state(gst::State::Null).unwrap();
    }
    #[test]
    fn frame_drop_logs_require_a_burst_and_are_rate_limited() {
        let now = Instant::now();
        let mut r = Recovery::default();
        r.eligible = Some(now - Duration::from_secs(4));
        r.drop_sample = Some((now - Duration::from_secs(2), 0, 0));
        r.observe_drops(100, 20);
        assert_eq!(r.sequence, 1);
        r.drop_sample = Some((now - Duration::from_secs(2), 100, 20));
        r.observe_drops(200, 40);
        assert_eq!(r.sequence, 1);
        r.cancel();
        assert!(r.drop_sample.is_none());
    }
    #[test]
    fn refill_has_startup_grace_timeout_cooldown_and_respects_user_pause() {
        gst::init().unwrap();
        let pipeline = gst::Pipeline::new();
        let source = gst::ElementFactory::make("uridecodebin")
            .build()
            .unwrap()
            .downcast::<gst::Bin>()
            .unwrap();
        let q = gst::ElementFactory::make("queue2")
            .property("ring-buffer-max-size", 8u64 * 1024 * 1024)
            .property("max-size-bytes", 65536u32)
            .build()
            .unwrap();
        source.add(&q).unwrap();
        let sources = vec![source.upcast()];
        let mut r = Recovery::default();
        assert!(
            !r.poll(
                &pipeline,
                &sources,
                true,
                false,
                false,
                9.,
                Some(10.),
                Some(60.)
            )
            .unwrap()
        );
        let now = Instant::now();
        r.eligible = Some(now - Duration::from_secs(4));
        r.changed = Some(now - Duration::from_secs(1));
        r.checked = None;
        assert!(
            r.poll(
                &pipeline,
                &sources,
                true,
                false,
                false,
                9.,
                Some(10.),
                Some(60.)
            )
            .unwrap()
        );
        r.refill = Some(Instant::now() - Duration::from_secs(5));
        r.checked = None;
        assert!(
            !r.poll(
                &pipeline,
                &sources,
                true,
                false,
                false,
                9.,
                Some(10.),
                Some(60.)
            )
            .unwrap()
        );
        assert_eq!(q.property::<u32>("max-size-bytes"), 65536);
        r.eligible = Some(now - Duration::from_secs(4));
        r.changed = Some(now - Duration::from_secs(1));
        r.checked = None;
        assert!(
            !r.poll(
                &pipeline,
                &sources,
                true,
                false,
                false,
                9.,
                Some(10.),
                Some(60.)
            )
            .unwrap()
        );
        pipeline.set_state(gst::State::Paused).unwrap();
        r.refill = Some(Instant::now());
        assert!(
            !r.poll(
                &pipeline,
                &sources,
                true,
                true,
                false,
                9.,
                Some(10.),
                Some(60.)
            )
            .unwrap()
        );
        assert_eq!(pipeline.current_state(), gst::State::Paused);
        pipeline.set_state(gst::State::Null).unwrap();
    }
}
