//! Bounded, cancellable music retries; no sleeping worker or extra process.
use super::*;
const DELAY: Duration = Duration::from_secs(2);
#[derive(Default)]
pub(super) struct MusicRetry {
    epoch: Option<u64>,
    attempts: u8,
    due: Option<Instant>,
}
impl MusicRetry {
    pub fn begin(&mut self, epoch: u64) {
        if self.epoch != Some(epoch) {
            *self = Self {
                epoch: Some(epoch),
                ..Self::default()
            };
        }
        self.due = None;
    }
    pub fn cancel(&mut self) {
        self.due = None;
    }
    fn arm(
        &mut self,
        epoch: u64,
        current: u64,
        offline: bool,
        error: &str,
        now: Instant,
    ) -> Option<u8> {
        if self.epoch != Some(epoch)
            || epoch != current
            || offline
            || self.attempts >= 2
            || availability::failure(error).is_some()
        {
            return None;
        }
        let text = error.to_lowercase();
        if [
            "cancelled",
            "canceled",
            "superseded",
            "requested format is not available",
            "missing a plugin",
            "no such file",
            "access is denied",
            "cannot start yt-dlp",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            return None;
        }
        self.attempts += 1;
        self.due = Some(now + DELAY);
        Some(self.attempts)
    }
    pub fn take_due(&mut self, current: u64, now: Instant) -> bool {
        if self.epoch != Some(current) {
            self.cancel();
            return false;
        }
        if self.due.is_some_and(|due| now >= due) {
            self.due = None;
            return true;
        }
        false
    }
    pub fn schedule(
        &mut self,
        request: &Option<(Video, Settings, u64)>,
        generation: &Arc<AtomicU64>,
        paths: &Paths,
        cache: &Arc<Mutex<stream_cache::StreamCache>>,
        weak: &slint::Weak<SetupWindow>,
        error: &str,
    ) -> bool {
        let Some((video, _, epoch)) = request.as_ref().filter(|(_, s, _)| s.music) else {
            return false;
        };
        if self.epoch == Some(*epoch)
            && *epoch == generation.load(Ordering::Acquire)
            && self.due.is_some()
        {
            return true;
        }
        let Some(attempt) = self.arm(
            *epoch,
            generation.load(Ordering::Acquire),
            paths.network.offline(),
            error,
            Instant::now(),
        ) else {
            return false;
        };
        cache.lock().unwrap().invalidate(&video.id);
        crate::diagnostics::record(
            "music_retry_scheduled",
            serde_json::json!({"video_id":video.id,"attempt":attempt,"limit":2,"delay_ms":2000,"error":error}),
        );
        ui_generation(weak, generation, *epoch, move |w| {
            w.set_resolving(true);
            w.set_playback_active(false);
            w.set_player_error("".into());
            w.set_playback_status(format!("Retrying audio ({attempt}/2)…").into());
        });
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn two_retries_wait_two_seconds_and_new_selection_cancels() {
        let now = Instant::now();
        let mut r = MusicRetry::default();
        r.begin(1);
        assert_eq!(r.arm(1, 1, false, "YouTube request failed", now), Some(1));
        assert!(!r.take_due(1, now + Duration::from_millis(1999)));
        assert!(r.take_due(1, now + DELAY));
        r.begin(1);
        assert_eq!(
            r.arm(1, 1, false, "Internal data stream error", now),
            Some(2)
        );
        assert!(r.take_due(1, now + DELAY));
        r.begin(1);
        assert_eq!(r.arm(1, 1, false, "Forbidden", now), None);
        r.begin(2);
        assert_eq!(r.arm(2, 2, false, "YouTube request failed", now), Some(1));
        assert!(!r.take_due(3, now + DELAY));
        assert!(r.due.is_none());
    }
    #[test]
    fn permanent_offline_and_cancelled_failures_do_not_retry() {
        let mut r = MusicRetry::default();
        r.begin(1);
        for error in [
            "Private video",
            "Requires age verification: age restricted",
            "cancelled",
            "Your installation is missing a plugin",
            "Requested format is not available",
        ] {
            assert_eq!(r.arm(1, 1, false, error, Instant::now()), None, "{error}");
        }
        assert_eq!(r.arm(1, 1, true, "Connection lost", Instant::now()), None);
        assert_eq!(r.attempts, 0);
    }
}
