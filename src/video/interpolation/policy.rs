//! Pure scheduling limits for the opt-in Linux GPU interpolation experiment.
use std::time::Duration;
// Requested decoder lead; the fixed GPU slot pool still bounds retained outputs.
pub const LOOKAHEAD_SOURCE_FRAMES: u32 = 3;
pub const PREPARE_LEAD: Duration = Duration::from_millis(21);
pub const DISPLAY_LEAD: Duration = Duration::from_nanos(16_666_667);
pub const MAX_PIXELS: u64 = 1280 * 720;
pub fn interval(width: u32, height: u32, n: i32, d: i32) -> Option<Duration> {
    if width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > MAX_PIXELS
        || width.max(height) > 1280
        || n <= 0
        || d <= 0
    {
        return None;
    }
    let fps = n as f64 / d as f64;
    if !(12.0..=30.1).contains(&fps) {
        return None;
    }
    Some(Duration::from_secs_f64(1.0 / fps))
}
pub fn continuous(previous: u64, next: u64, period: Duration) -> bool {
    next.checked_sub(previous).is_some_and(|delta| {
        delta > period.as_nanos() as u64 * 3 / 4 && delta < period.as_nanos() as u64 * 5 / 4
    })
}
pub fn slow(gpu_ns: u64, period: Duration) -> bool {
    gpu_ns > period.as_nanos() as u64 * 35 / 100
}
/// Estimation is shared by every generated output in the source interval.
/// Leave room for warp/presentation while allowing the opt-in quality work.
pub fn slow_estimate(gpu_ns: u64, source_period: Duration) -> bool {
    u128::from(gpu_ns) > source_period.as_nanos() * 60 / 100
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_preserve_native_playback_for_large_high_rate_or_unknown_streams() {
        assert!(interval(1280, 720, 24000, 1001).is_some());
        assert!(interval(720, 1280, 24, 1).is_some());
        assert!(interval(1920, 1080, 24, 1).is_none());
        assert!(interval(1280, 720, 60, 1).is_none());
        assert!(interval(0, 0, 24, 1).is_none());
        assert!(interval(1280, 720, 0, 1).is_none());
    }
    #[test]
    fn seek_and_discontinuities_cannot_form_a_frame_pair() {
        let p = Duration::from_millis(40);
        assert!(continuous(0, 40_000_000, p));
        assert!(!continuous(0, 0, p));
        assert!(!continuous(80_000_000, 40_000_000, p));
        assert!(!continuous(0, 5_000_000_000, p));
        assert!(!continuous(0, 80_000_000, p));
    }
    #[test]
    fn gpu_budget_is_a_fraction_of_source_frame_interval() {
        let p = Duration::from_millis(40);
        assert!(!slow(10_000_000, p));
        assert!(slow(15_000_000, p));
        assert!(!slow_estimate(19_000_000, p));
        assert!(slow_estimate(25_000_000, p));
        assert!(slow(8_000_000, Duration::from_secs_f64(1.0 / 60.0)));
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Tick {
    pub pts: u64,
    pub fraction: f32,
}
pub struct Plan {
    pub ticks: [Tick; 8],
    pub len: usize,
}
/// Fixed-size output grid. Fixed 60 uses media time, so 24→60 alternates
/// three/two samples per pair instead of drifting at 48 or 72 fps.
pub fn plan(previous: u64, next: u64, mode: i32) -> Plan {
    let mut out = Plan {
        ticks: [Tick::default(); 8],
        len: 0,
    };
    let Some(delta) = next.checked_sub(previous).filter(|d| *d > 0) else {
        return out;
    };
    if mode == 1 {
        out.ticks[0] = Tick {
            pts: previous,
            fraction: 0.,
        };
        out.ticks[1] = Tick {
            pts: previous + delta / 2,
            fraction: 0.5,
        };
        out.len = 2;
    } else if mode == 2 {
        let mut index = (u128::from(previous) * 60).div_ceil(1_000_000_000);
        while out.len < out.ticks.len() {
            let Ok(pts) = u64::try_from(index * 1_000_000_000 / 60) else {
                break;
            };
            if pts >= next {
                break;
            }
            if pts >= previous {
                out.ticks[out.len] = Tick {
                    pts,
                    fraction: (pts - previous) as f32 / delta as f32,
                };
                out.len += 1;
            }
            index += 1;
        }
    }
    out
}
pub fn output_period(mode: i32, source: Duration) -> Duration {
    if mode == 2 {
        Duration::from_secs_f64(1. / 60.)
    } else {
        source / 2
    }
}
#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn fixed_sixty_is_exact_across_24_25_and_30_fps() {
        for fps in [24u64, 25, 30] {
            let total: usize = (0..fps)
                .map(|i| plan(i * 1_000_000_000 / fps, (i + 1) * 1_000_000_000 / fps, 2).len)
                .sum();
            assert_eq!(total, 60);
        }
    }
    #[test]
    fn doubled_cadence_remains_doubled() {
        let total: usize = (0..24)
            .map(|i| plan(i * 1_000_000_000 / 24, (i + 1) * 1_000_000_000 / 24, 1).len)
            .sum();
        assert_eq!(total, 48);
    }
    #[test]
    fn fixed_sixty_handles_fractional_rate_and_seek_offset() {
        let start = 57_123_456_789;
        let end = start + 10_010_000_000;
        let mut all = Vec::new();
        for i in 0..240 {
            let p = plan(
                start + i * 1_001_000_000 / 24,
                start + (i + 1) * 1_001_000_000 / 24,
                2,
            );
            all.extend(p.ticks[..p.len].iter().map(|t| t.pts));
        }
        assert_eq!(all.len(), 601);
        assert!(all.iter().all(|t| *t >= start && *t < end));
        assert!(
            all.windows(2)
                .all(|p| matches!(p[1] - p[0], 16_666_666 | 16_666_667))
        );
    }
    #[test]
    fn invalid_spans_and_off_have_no_output() {
        assert_eq!(plan(10, 0, 2).len, 0);
        assert_eq!(plan(0, 0, 2).len, 0);
        assert_eq!(plan(0, 1_000_000_000, 0).len, 0);
        assert_eq!(plan(0, u64::MAX, 2).len, 8);
    }
}
