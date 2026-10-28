//! Constant-space input observations, shared by source and seek/rebuffer policy.
//! Sampling ignores long gaps (a full ring intentionally stops its HTTP reader).
use std::time::{Duration, Instant};
#[derive(Default)]
pub(super) struct Meter {
    last: Option<Instant>,
    window: Option<Instant>,
    bytes: u64,
    samples: u32,
    rate: Option<f64>,
}
impl Meter {
    pub fn arrived(&mut self, now: Instant, bytes: usize) {
        if self
            .last
            .is_some_and(|at| now.duration_since(at) > Duration::from_secs(2))
        {
            self.window = None;
            self.bytes = 0;
            self.samples = 0;
        }
        self.last = Some(now);
        let Some(start) = self.window else {
            self.window = Some(now);
            return;
        };
        self.bytes = self.bytes.saturating_add(bytes as u64);
        self.samples = self.samples.saturating_add(1);
        let elapsed = now.duration_since(start).as_secs_f64();
        if elapsed >= 0.25 && self.samples >= 1 {
            let sample = self.bytes as f64 / elapsed;
            self.rate = Some(self.rate.map_or(sample, |old| old * 0.65 + sample * 0.35));
            self.window = Some(now);
            self.bytes = 0;
            self.samples = 0;
        }
    }
    pub fn rate(&self) -> Option<f64> {
        self.rate
    }
    pub fn recent(&self, now: Instant, grace: Duration) -> bool {
        self.last.is_some_and(|at| now.duration_since(at) < grace)
    }
}
/// Fast input refills briefly; borderline delivery gets a modest cushion.
/// No speculative new requests or increase to the compressed ring budget.
pub(super) fn refill_seconds(required: f64, measured: Option<f64>) -> f64 {
    match measured.filter(|r| r.is_finite() && *r > 0.) {
        Some(rate) if rate >= required * 2. => 0.5,
        Some(rate) if rate >= required * 1.2 => 1.,
        Some(_) => 2.,
        None => 1.,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adapts_to_delivery_without_unbounded_buffer_growth() {
        assert_eq!(refill_seconds(100., Some(300.)), 0.5);
        assert_eq!(refill_seconds(100., Some(150.)), 1.);
        assert_eq!(refill_seconds(100., Some(80.)), 2.);
        assert_eq!(refill_seconds(100., None), 1.);
        assert_eq!(refill_seconds(100., Some(f64::NAN)), 1.);
    }
    #[test]
    fn activity_and_rate_do_not_confuse_ring_backpressure_with_failure() {
        let now = Instant::now();
        let mut meter = Meter::default();
        meter.arrived(now, 1000);
        meter.arrived(now + Duration::from_millis(500), 1000);
        assert_eq!(meter.rate(), Some(2000.));
        meter.arrived(now + Duration::from_secs(20), 1000);
        assert_eq!(meter.rate(), Some(2000.));
        assert!(meter.recent(now + Duration::from_secs(21), Duration::from_secs(2)));
        assert!(!meter.recent(now + Duration::from_secs(23), Duration::from_secs(2)));
    }
}
