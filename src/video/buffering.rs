//! Compressed source budgeting; decoded/GPU frame queues remain small.
use gst::prelude::*;
use serde::{Deserialize, Serialize};
const MIB: u64 = 1024 * 1024;
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Hints {
    pub duration: f64,
    pub primary_bps: f64,
    pub audio_bps: f64,
}
impl Hints {
    pub fn from_metadata(
        info: &serde_json::Value,
        primary: &serde_json::Value,
        audio: Option<&serde_json::Value>,
    ) -> Self {
        let duration = info["duration"].as_f64().unwrap_or(0.);
        let rate = |format: &serde_json::Value| {
            format["tbr"]
                .as_f64()
                .filter(|r| r.is_finite() && *r > 0.)
                .map(|r| r * 1000.)
                .or_else(|| {
                    format["filesize"]
                        .as_f64()
                        .or(format["filesize_approx"].as_f64())
                        .filter(|_| duration > 0.)
                        .map(|n| n * 8. / duration)
                })
                .unwrap_or(0.)
        };
        Self {
            duration,
            primary_bps: rate(primary),
            audio_bps: audio.map(rate).unwrap_or(0.),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct Plan {
    pub ring: u64,
    pub forward: u32,
    pub bytes_per_second: f64,
    pub requested_ahead_seconds: f64,
    pub retained_behind_seconds: f64,
}
impl Plan {
    pub fn new(duration: f64, bps: f64, video: bool) -> Self {
        let duration = if duration.is_finite() && duration > 0. {
            duration
        } else {
            0.
        };
        let ahead = if duration == 0. {
            30.
        } else if duration <= 600. {
            duration
        } else if duration <= 1200. {
            duration * 0.25
        } else {
            (duration * 0.1).clamp(180., 600.)
        };
        let rate = if bps.is_finite() && bps > 0. {
            bps / 8.
        } else if video {
            // No format metadata (e.g. a direct test/local HTTP URL): reserve
            // for ordinary high-bitrate 1080p rather than a low-bitrate song.
            1_000_000.
        } else {
            20_000.
        };
        let cap = if video { 96 * MIB } else { 8 * MIB };
        let minimum = if video { 2 * MIB } else { 512 * 1024 };
        // Include a keyframe margin beyond four 5-second rewinds. When a high
        // bitrate exceeds the budget, retain history before spending on ahead.
        let behind = (rate * 25.).ceil().min(cap as f64 * 0.8) as u64;
        let forward = (rate * ahead)
            .ceil()
            .max(64. * 1024.)
            .min((cap - behind) as f64) as u64;
        let ring = (behind + forward).clamp(minimum, cap);
        Self {
            ring,
            forward: forward.min(ring - behind) as u32,
            bytes_per_second: rate,
            requested_ahead_seconds: ahead,
            retained_behind_seconds: behind as f64 / rate,
        }
    }
    pub fn initial(self, video: bool) -> i32 {
        self.forward.min(if video { 256 * 1024 } else { 64 * 1024 }) as i32
    }
    pub fn grow(self, source: &gst::Element) -> bool {
        let mut applied = false;
        if let Some(bin) = source.downcast_ref::<gst::Bin>() {
            for q in bin.iterate_recurse().into_iter().flatten() {
                if q.factory().is_some_and(|f| f.name() == "queue2")
                    && q.property::<u64>("ring-buffer-max-size") > 0
                {
                    q.set_property("max-size-bytes", self.forward);
                    applied = true;
                }
            }
        }
        applied || source.current_state() >= gst::State::Paused
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_scales_with_duration_and_preserves_rewind_before_ahead() {
        let song = Plan::new(240., 2_000_000., true);
        assert_eq!(song.requested_ahead_seconds, 240.);
        assert!(song.retained_behind_seconds >= 25.);
        assert_eq!(song.initial(true), 256 * 1024);
        assert_eq!(
            Plan::new(900., 2_000_000., true).requested_ahead_seconds,
            225.
        );
        assert_eq!(
            Plan::new(3600., 2_000_000., true).requested_ahead_seconds,
            360.
        );
        let high = Plan::new(3600., 30_000_000., true);
        assert!(high.ring <= 96 * MIB && high.retained_behind_seconds >= 20.);
        assert!(Plan::new(240., 128_000., false).ring < 5 * MIB);
        for (d, b) in [(f64::NAN, f64::INFINITY), (-1., -1.), (1e30, 1e30)] {
            let p = Plan::new(d, b, true);
            assert!(p.ring <= 96 * MIB && p.forward > 0 && u64::from(p.forward) < p.ring);
        }
    }
    #[test]
    fn estimates_only_selected_quality_and_independent_audio() {
        let h = Hints::from_metadata(
            &serde_json::json!({"duration":100}),
            &serde_json::json!({"filesize":10000000}),
            Some(&serde_json::json!({"tbr":128})),
        );
        assert_eq!(h.primary_bps, 800000.);
        assert_eq!(h.audio_bps, 128000.);
    }
}
