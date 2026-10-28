//! Bounded buffering aggregation and a single, stable loading state.
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
// A paused pipeline can briefly answer zero while preroll/state queries settle.
// Explicit seeks reset the checkpoint first, so seeking to zero still wins.
pub(crate) fn position(queried: Option<f64>, checkpoint: f64, paused: bool) -> Option<f64> {
    let queried = queried.filter(|p| p.is_finite() && *p >= 0.);
    if paused && checkpoint.is_finite() && checkpoint > 0. && queried == Some(0.) {
        Some(checkpoint)
    } else {
        queried
    }
}
#[cfg(test)]
#[test]
fn paused_position_ignores_transient_zero_but_honors_explicit_rewind() {
    assert_eq!(position(Some(0.), 25., true), Some(25.));
    assert_eq!(position(Some(0.), 0., true), Some(0.));
    assert_eq!(position(Some(0.), 25., false), Some(0.));
    assert_eq!(position(Some(26.), 25., true), Some(26.));
    assert_eq!(position(None, 25., true), None);
}

// Never seek to exact EOF: no target video sample exists there, so preroll
// cannot complete. Unknown/live durations still accept finite nonnegative times.
pub(crate) fn seek_target(requested: f64, duration: Option<f64>) -> Option<f64> {
    if !requested.is_finite() {
        return None;
    }
    let end = duration
        .filter(|d| d.is_finite() && *d > 0.)
        .map(|d| (d - 0.25).max(0.))
        .unwrap_or(f64::MAX);
    Some(requested.clamp(0., end))
}
pub(crate) fn seek_request(
    requested: f64,
    duration: Option<f64>,
    has_media: bool,
    ended: bool,
) -> Option<f64> {
    if !has_media || (ended && duration.is_some_and(|d| requested >= d - 0.25)) {
        return None;
    }
    seek_target(requested, duration)
}
#[cfg(test)]
#[test]
fn stopped_and_ended_seek_requests_are_not_accidental_replays() {
    assert_eq!(seek_request(100., Some(100.), true, true), None);
    assert_eq!(seek_request(105., Some(100.), true, true), None);
    assert_eq!(seek_request(95., Some(100.), true, true), Some(95.));
    assert_eq!(seek_request(5., None, false, false), None);
    assert_eq!(seek_request(f64::NAN, Some(100.), true, false), None);
    assert_eq!(seek_request(105., Some(100.), true, false), Some(99.75));
}
#[cfg(test)]
#[test]
fn seek_targets_reject_invalid_input_and_leave_a_decodable_eof_margin() {
    assert_eq!(seek_target(f64::NAN, Some(100.)), None);
    assert_eq!(seek_target(100., Some(100.)), Some(99.75));
    assert_eq!(seek_target(200., Some(100.)), Some(99.75));
    assert_eq!(seek_target(-5., Some(100.)), Some(0.));
    assert_eq!(seek_target(20., None), Some(20.));
    assert_eq!(seek_target(95., Some(100.)), Some(95.));
}

#[derive(Default)]
pub(crate) struct Buffering {
    sources: BTreeSet<usize>,
    last_log: Option<Instant>,
    suppressed: u64,
}
impl Buffering {
    pub fn update(&mut self, source: usize, percent: i32) {
        if percent >= 100 {
            self.sources.remove(&source);
        } else if self.sources.len() < 32 {
            self.sources.insert(source);
        }
    }
    pub fn diagnostic_transition(&mut self) -> Option<u64> {
        let now = Instant::now();
        if self
            .last_log
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
        {
            self.suppressed += 1;
            return None;
        }
        self.last_log = Some(now);
        let suppressed = std::mem::take(&mut self.suppressed);
        Some(suppressed)
    }
    pub fn waiting(&self) -> bool {
        !self.sources.is_empty()
    }
    pub fn clear(&mut self) {
        self.sources.clear();
    }
}
/// Recent media timestamps override stale/flapping BUFFERING percentages.
#[derive(Default)]
pub(crate) struct Progress {
    timestamp: Option<f64>,
    changed: Option<Instant>,
}
impl Progress {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn observe(&mut self, timestamp: Option<f64>, now: Instant) -> bool {
        if let Some(t) = timestamp.filter(|t| t.is_finite() && *t >= 0.) {
            if self
                .timestamp
                .is_none_or(|previous| (previous - t).abs() > 0.0001)
            {
                self.timestamp = Some(t);
                self.changed = Some(now);
            }
        }
        self.changed
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(800))
    }
}
#[derive(Default)]
pub(crate) struct Loading {
    seek_wait: Option<Instant>,
}
impl Loading {
    pub fn update(&mut self, waiting: bool, seeking_only: bool, now: Instant) -> bool {
        if !waiting || !seeking_only {
            self.seek_wait = None;
            return waiting;
        }
        let started = *self.seek_wait.get_or_insert(now);
        now.duration_since(started) >= Duration::from_millis(150)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seek_spinner_waits_briefly_but_startup_and_rebuffer_do_not() {
        let mut l = Loading::default();
        let t = Instant::now();
        assert!(!l.update(true, true, t));
        assert!(!l.update(true, true, t + Duration::from_millis(149)));
        assert!(l.update(true, true, t + Duration::from_millis(150)));
        assert!(!l.update(false, true, t + Duration::from_millis(151)));
        assert!(!l.update(true, true, t + Duration::from_millis(152)));
        assert!(l.update(true, false, t + Duration::from_millis(153)));
        assert!(!l.update(false, false, t + Duration::from_millis(154)));
    }
    #[test]
    fn one_ready_source_cannot_clear_another_sources_buffering() {
        let mut b = Buffering::default();
        b.update(1, 20);
        b.update(2, 30);
        b.update(1, 100);
        assert!(b.waiting());
        b.update(2, 100);
        assert!(!b.waiting());
        b.update(1, 0);
        b.clear();
        assert!(!b.waiting());
    }
    #[test]
    fn loader_clears_immediately_and_percentages_cannot_hide_progress() {
        let t = Instant::now();
        let mut l = Loading::default();
        let mut progress = Progress::default();
        assert!(l.update(true, false, t));
        assert!(!l.update(false, false, t));
        assert!(progress.observe(Some(1.), t));
        assert!(progress.observe(Some(1.1), t + Duration::from_millis(100)));
        assert!(progress.observe(Some(1.1), t + Duration::from_millis(700)));
        assert!(!progress.observe(Some(1.1), t + Duration::from_millis(950)));
        assert!(progress.observe(Some(0.5), t + Duration::from_millis(1000)));
        progress.clear();
        assert!(!progress.observe(None, t + Duration::from_millis(1001)));
    }
}

/// A load is not ready until its initial seek and requested state have settled.
#[cfg(any(target_os = "windows", test))]
pub(crate) struct Start {
    pub position: f64,
    pub paused: bool,
    pub seek_sent: bool,
}
#[cfg(any(target_os = "windows", test))]
impl Start {
    pub fn new(position: f64, paused: bool) -> Self {
        Self {
            position: if position.is_finite() {
                position.max(0.)
            } else {
                0.
            },
            paused,
            seek_sent: false,
        }
    }
}
