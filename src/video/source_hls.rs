//! Only Mochune's nonce-protected Anime/Movie HLS relay uses this policy.
//! YouTube, Music and ordinary HTTP playback never enter it.
use base64::Engine as _;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
#[derive(Default)]
pub(super) struct Gate {
    pub enabled: bool,
    pub sink_defaults: Option<(Option<i64>, Option<bool>)>,
    levels: BTreeMap<usize, (i32, Instant)>,
    pts: Option<f64>,
    changed: Option<Instant>,
    waiting: Option<Instant>,
    ready: Option<Instant>,
    cooldown: Option<Instant>,
}
#[derive(Debug, PartialEq)]
pub(super) enum Action {
    Pause,
    Resume,
    None,
}
pub(super) fn relay_uri(uri: &str) -> bool {
    let Ok(u) = url::Url::parse(uri) else {
        return false;
    };
    if u.scheme() != "http" || u.host_str() != Some("127.0.0.1") || u.port().is_none() {
        return false;
    }
    let mut path = u.path_segments().into_iter().flatten();
    let nonce = path.next().unwrap_or("");
    nonce.len() == 22
        && base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(nonce)
            .is_ok_and(|v| v.len() == 16)
        && path.next().is_some_and(|p| !p.is_empty())
}
impl Gate {
    pub fn reset(&mut self, enabled: bool) {
        *self = Self {
            enabled,
            ..Default::default()
        };
    }
    pub fn buffering(&mut self, id: usize, percent: i32) {
        if self.enabled && (self.levels.len() < 16 || self.levels.contains_key(&id)) {
            self.levels
                .insert(id, (percent.clamp(0, 100), Instant::now()));
        }
    }
    pub fn waiting(&self) -> bool {
        self.waiting.is_some()
    }
    pub fn step(
        &mut self,
        active: bool,
        paused: bool,
        seeking: bool,
        pts: f64,
        position: Option<f64>,
        duration: Option<f64>,
        now: Instant,
    ) -> Action {
        if !self.enabled || !active || paused || seeking {
            self.waiting = None;
            self.ready = None;
            self.pts = None;
            self.changed = None;
            self.cooldown = None;
            return Action::None;
        }
        let level = self
            .levels
            .values()
            .filter(|(_, at)| now.duration_since(*at) < Duration::from_secs(12))
            .map(|(v, _)| *v)
            .min()
            .unwrap_or(100);
        if let Some(start) = self.waiting {
            if level >= 75 {
                self.ready.get_or_insert(now);
            } else {
                self.ready = None;
            }
            let stable = self
                .ready
                .is_some_and(|t| now.duration_since(t) >= Duration::from_millis(300));
            let eof = position.zip(duration).is_some_and(|(p, d)| d - p < 1.);
            if (now.duration_since(start) >= Duration::from_millis(600) && (stable || eof))
                || now.duration_since(start) >= Duration::from_secs(4)
            {
                self.waiting = None;
                self.ready = None;
                self.changed = Some(now);
                self.cooldown = Some(now);
                return Action::Resume;
            }
            return Action::None;
        }
        if pts.is_finite() && pts > 0. && self.pts.is_none_or(|p| (p - pts).abs() > 0.0001) {
            self.pts = Some(pts);
            self.changed = Some(now);
        }
        let stalled = self
            .changed
            .is_some_and(|t| now.duration_since(t) >= Duration::from_millis(500));
        let lag = position
            .filter(|p| p.is_finite())
            .map(|p| p - pts)
            .unwrap_or(0.);
        let cooling = self
            .cooldown
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(2));
        if self.pts.is_some() && level < 75 && ((stalled && lag > 0.15) || lag > 0.8) && !cooling {
            self.waiting = Some(now);
            self.ready = None;
            return Action::Pause;
        }
        Action::None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolation_is_explicit() {
        assert!(!relay_uri("https://r1.googlevideo.com/videoplayback"));
        assert!(!relay_uri("mochunehttps://r1.googlevideo.com/file"));
        assert!(!relay_uri("http://127.0.0.1:1234/music"));
        assert!(relay_uri(
            "http://127.0.0.1:1234/AAAAAAAAAAAAAAAAAAAAAA/c2VnbWVudA"
        ));
    }
    #[test]
    fn advancing_frames_ignore_percentage_flapping() {
        let mut g = Gate::default();
        g.reset(true);
        let t = Instant::now();
        for i in 0..30 {
            g.buffering(1, if i % 2 == 0 { 0 } else { 100 });
            assert_eq!(
                g.step(
                    true,
                    false,
                    false,
                    1. + i as f64 * 0.1,
                    Some(1. + i as f64 * 0.1),
                    None,
                    t + Duration::from_millis(i * 100)
                ),
                Action::None
            );
        }
        assert!(!g.waiting());
    }
    #[test]
    fn true_stall_holds_until_a_stable_cushion() {
        let mut g = Gate::default();
        g.reset(true);
        let t = Instant::now();
        g.buffering(1, 0);
        g.step(true, false, false, 2., Some(2.), None, t);
        assert_eq!(
            g.step(
                true,
                false,
                false,
                2.,
                Some(2.6),
                None,
                t + Duration::from_millis(600)
            ),
            Action::Pause
        );
        g.buffering(1, 100);
        assert_eq!(
            g.step(
                true,
                false,
                false,
                2.,
                Some(2.6),
                None,
                t + Duration::from_millis(650)
            ),
            Action::None
        );
        g.buffering(1, 0);
        assert_eq!(
            g.step(
                true,
                false,
                false,
                2.,
                Some(2.6),
                None,
                t + Duration::from_millis(700)
            ),
            Action::None
        );
        g.buffering(1, 100);
        g.step(
            true,
            false,
            false,
            2.,
            Some(2.6),
            None,
            t + Duration::from_millis(1300),
        );
        assert_eq!(
            g.step(
                true,
                false,
                false,
                2.,
                Some(2.6),
                None,
                t + Duration::from_millis(1650)
            ),
            Action::Resume
        );
    }
    #[test]
    fn disabled_and_user_pause_do_not_resume() {
        let mut g = Gate::default();
        let t = Instant::now();
        g.buffering(1, 0);
        assert_eq!(
            g.step(true, false, false, 2., Some(7.), None, t),
            Action::None
        );
        g.reset(true);
        g.buffering(1, 0);
        assert_eq!(
            g.step(true, false, false, 2., Some(7.), None, t),
            Action::Pause
        );
        assert_eq!(
            g.step(
                true,
                true,
                false,
                2.,
                Some(7.),
                None,
                t + Duration::from_secs(10)
            ),
            Action::None
        );
        assert!(!g.waiting());
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    #[test]
    fn a_clock_already_stopped_with_video_gets_no_extra_pause() {
        let mut g = Gate::default();
        g.reset(true);
        let t = Instant::now();
        g.buffering(1, 0);
        g.step(true, false, false, 2., Some(2.03), None, t);
        assert_eq!(
            g.step(
                true,
                false,
                false,
                2.,
                Some(2.03),
                None,
                t + Duration::from_secs(1)
            ),
            Action::None
        );
        assert!(!g.waiting());
    }
}
