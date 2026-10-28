//! Retained compressed ranges, queried from the current native source buffers.
//! Timeline ranges come from observed packet timestamps intersected with native
//! byte retention. No synchronous container indexing or global byte percentages.
use gst::prelude::*;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub const MAX_RANGES: usize = 16;
#[derive(Default)]
pub struct Snapshot {
    checked: Option<Instant>,
    indices: HashMap<String, Arc<Mutex<TimeIndex>>>,
    pub ranges: Vec<(f64, f64)>,
    pub estimated: bool,
    pub confirmed: Vec<(f64, f64)>,
    native_bytes: HashMap<String, Vec<(u64, u64)>>,
    valid_bytes: HashMap<String, Vec<(u64, u64)>>,
    tails: HashMap<String, TailEstimate>,
    complete: HashSet<String>,
    pub diagnostics: Vec<serde_json::Value>,
}
#[derive(Debug, serde::Serialize)]
pub(super) struct SeekAssessment {
    pub retained: Option<bool>,
    pub unsafe_native_claim: bool,
    pub reason: &'static str,
    pub keyframe_time: Option<f64>,
}
impl Default for SeekAssessment {
    fn default() -> Self {
        Self {
            retained: None,
            unsafe_native_claim: false,
            reason: "unobserved",
            keyframe_time: None,
        }
    }
}

fn union_bytes(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.retain(|(a, b)| b > a);
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (a, b) in ranges {
        if let Some(last) = merged.last_mut()
            && a <= last.1
        {
            last.1 = last.1.max(b);
        } else {
            merged.push((a, b));
        }
    }
    merged
}
#[derive(Default)]
struct TailEstimate {
    bytes: Option<(u64, u64)>,
    end: f64,
}
impl TailEstimate {
    fn update(&mut self, range: (u64, u64), end: f64) -> f64 {
        // Appending bytes cannot remove already resident future media. Don't
        // redraw its endpoint backward just because local VBR rate changed.
        // A shrinking/disjoint byte range really can evict that data.
        let keep = self
            .bytes
            .is_some_and(|(a, b)| range.1 >= b && range.0 <= b && a <= range.1);
        self.end = if keep { self.end.max(end) } else { end };
        self.bytes = Some(range);
        self.end
    }
}
fn normalized(mut ranges: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    ranges.retain(|(a, b)| a.is_finite() && b.is_finite() && *a >= 0. && *b > *a && *b <= 1.);
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in ranges {
        if let Some(last) = merged.last_mut()
            && a <= last.1 + 1e-8
        {
            last.1 = last.1.max(b);
            continue;
        }
        if merged.len() == MAX_RANGES {
            break;
        }
        merged.push((a, b));
    }
    merged
}
fn intersection(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    normalized(
        a.iter()
            .flat_map(|(a, z)| {
                b.iter().filter_map(move |(c, d)| {
                    let lo = a.max(*c);
                    let hi = z.min(*d);
                    (hi > lo).then_some((lo, hi))
                })
            })
            .collect(),
    )
}
// Packet timestamps paired with exact pull-read offsets. Never retain packets,
// frames, URLs or full container indexes. Adjacent packets share 250ms buckets.
#[derive(Clone, Copy)]
struct Span {
    first: u64,
    last: u64,
    start: f64,
    end: f64,
}
#[derive(Default)]
struct TimeIndex {
    spans: VecDeque<Span>,
    keyframes: VecDeque<Span>,
    writes: WriteWindow,
}
// Forward bytes may not have reached the demuxer yet. Estimate only that
// bounded unread tail, using a current byte/time anchor and measured local rate.
// Never derive the whole timeline from file percentages, or mark evicted bytes.
fn ahead(bytes: &[(u64, u64)], anchor: (u64, f64), rate: f64, duration: f64) -> Option<(f64, f64)> {
    if !rate.is_finite() || rate <= 0. || !anchor.1.is_finite() || anchor.1 < 0. || duration <= 0. {
        return None;
    }
    let end = bytes
        .iter()
        .find(|(a, b)| *a <= anchor.0 && *b > anchor.0)?
        .1;
    // This is an estimate from bytes actually resident in queue2. Larger
    // read-ahead budgets must not be drawn as an arbitrary two-second tail.
    let seconds = ((end - anchor.0) as f64 / rate).min((duration - anchor.1).max(0.));
    (seconds > 0.).then_some((
        anchor.1 / duration,
        ((anchor.1 + seconds) / duration).min(1.),
    ))
}
// Conservatively intersect native claims with bytes received in the most
// recent physical ring capacity. Native queries can keep stale tiny ranges
// (e.g. the first MP4 keyframe) after another range overwrites their slots.
#[derive(Default)]
struct WriteWindow {
    spans: VecDeque<(u64, u64)>,
    size: u64,
    next: Option<u64>,
    truncated: bool,
    last_input: Option<Instant>,
}
impl WriteWindow {
    fn add(&mut self, start: u64, end: u64, capacity: u64) {
        self.last_input = Some(Instant::now());
        if end <= start || capacity == 0 {
            return;
        }
        if let Some(last) = self.spans.back_mut().filter(|s| s.1 == start) {
            last.1 = end;
        } else {
            self.spans.push_back((start, end));
        }
        self.size = self.size.saturating_add(end - start);
        while self.size > capacity || self.spans.len() > 1024 {
            let excess = self.size.saturating_sub(capacity);
            self.truncated |= excess > 0;
            let first = self.spans.front_mut().unwrap();
            let trim = if excess == 0 {
                first.1 - first.0
            } else {
                excess.min(first.1 - first.0)
            };
            first.0 += trim;
            self.size -= trim;
            if first.0 == first.1 {
                self.spans.pop_front();
            }
        }
    }
    fn retained(&self, native: &[(u64, u64)]) -> Vec<(u64, u64)> {
        union_bytes(
            native
                .iter()
                .flat_map(|(a, b)| {
                    self.spans.iter().filter_map(move |(c, d)| {
                        let lo = (*a).max(*c);
                        let hi = (*b).min(*d);
                        (hi > lo).then_some((lo, hi))
                    })
                })
                .collect(),
        )
    }
}
impl TimeIndex {
    fn add(&mut self, span: Span) {
        if !span.start.is_finite()
            || span.start < 0.
            || span.end <= span.start
            || span.last <= span.first
        {
            return;
        }
        if let Some(p) = self.spans.back_mut()
            && span.first >= p.last
            && span.first - p.last < 64
            && span.start <= p.end + 0.08
            && span.end - p.start <= 0.25
        {
            p.last = span.last;
            p.start = p.start.min(span.start);
            p.end = p.end.max(span.end);
            return;
        }
        if self.spans.len() == 4096 {
            self.spans.pop_front();
        }
        self.spans.push_back(span);
    }
    // Map each physically continuous retained byte window to its observed time
    // bounds. Packet observations are sparse/reordered: holes between observed
    // timestamps are not holes in a continuous native byte buffer. Never join
    // separate byte windows (a distant seek can leave a real missing interval).
    fn coverage(&self, bytes: &[(u64, u64)], duration: f64) -> (Vec<(f64, f64)>, Vec<(f64, f64)>) {
        if !duration.is_finite() || duration <= 0. {
            return (Vec::new(), Vec::new());
        }
        let bytes = union_bytes(bytes.to_vec());
        let mut retained = vec![None::<(f64, f64)>; bytes.len()];
        let mut confirmed = retained.clone();
        let mut keys: Vec<_> = self.keyframes.iter().collect();
        keys.sort_by(|a, b| a.start.total_cmp(&b.start));
        let extend = |slot: &mut Option<(f64, f64)>, span: &Span| {
            *slot = Some(slot.map_or((span.start, span.end), |(a, b)| {
                (a.min(span.start), b.max(span.end))
            }));
        };
        for span in &self.spans {
            let i = bytes.partition_point(|(_, end)| *end <= span.first);
            let Some(&(a, b)) = bytes.get(i) else {
                continue;
            };
            if a > span.first || b < span.last {
                continue;
            }
            extend(&mut retained[i], span);
            let key = keys
                .partition_point(|k| k.start <= span.start)
                .checked_sub(1)
                .and_then(|i| keys.get(i));
            if key.is_some_and(|k| a <= k.first && b >= k.last) {
                extend(&mut confirmed[i], span);
            }
        }
        let normalize = |bounds: Vec<Option<(f64, f64)>>| {
            normalized(
                bounds
                    .into_iter()
                    .flatten()
                    .map(|(a, b)| (a / duration, (b / duration).min(1.)))
                    .collect(),
            )
        };
        (normalize(retained), normalize(confirmed))
    }
    #[cfg(test)]
    fn seekable(&self, bytes: &[(u64, u64)], duration: f64) -> Vec<(f64, f64)> {
        self.coverage(bytes, duration).1
    }
    fn current_anchor(&self) -> Option<(u64, f64)> {
        self.spans.back().map(|s| (s.last, s.end))
    }
    fn recent_rate(&self) -> Option<f64> {
        let last = self.spans.back()?;
        self.spans.iter().rev().take(256).find_map(|s| {
            let dt = last.end - s.start;
            (dt >= 1. && dt <= 3. && last.last > s.first).then(|| (last.last - s.first) as f64 / dt)
        })
    }
    #[cfg(test)]
    fn retained(&self, bytes: &[(u64, u64)], duration: f64) -> Vec<(f64, f64)> {
        self.coverage(bytes, duration).0
    }
}
impl Snapshot {
    #[cfg(test)]
    pub fn source_recently_active(&self, grace: Duration) -> bool {
        // Only video-index sources are indexed here. Separate audio read-ahead
        // must not indefinitely defer a genuinely stalled video seek.
        self.indices.values().any(|i| {
            i.lock()
                .unwrap()
                .writes
                .last_input
                .is_some_and(|t| t.elapsed() < grace)
        })
    }

    pub fn discard_retention(&mut self) {
        for index in self.indices.values() {
            *index.lock().unwrap() = TimeIndex::default();
        }
        self.valid_bytes.clear();
        self.native_bytes.clear();
        self.confirmed.clear();
        self.complete.clear();
        self.tails.clear();
        self.diagnostics.clear();
        self.ranges.clear();
        self.estimated = false;
        self.checked = None;
    }
    pub fn prepare_seek(&mut self) {
        self.checked = None;
    }
    #[cfg(test)]
    pub fn seek_retained(&self, target: f64) -> Option<bool> {
        self.assess_seek(target).retained
    }
    pub fn assess_seek(&self, target: f64) -> SeekAssessment {
        let mut result = SeekAssessment::default();
        for (name, index) in &self.indices {
            if self.complete.contains(name) {
                result.retained = Some(true);
                result.reason = "complete_video";
                continue;
            }
            let Some(bytes) = self.valid_bytes.get(name) else {
                return SeekAssessment::default();
            };
            let index = index.lock().unwrap();
            let Some(key) = index
                .keyframes
                .iter()
                .filter(|s| s.start <= target)
                .max_by(|a, b| a.start.total_cmp(&b.start))
            else {
                return SeekAssessment {
                    reason: "keyframe_not_observed",
                    ..Default::default()
                };
            };
            let key_retained = bytes.iter().any(|(a, b)| *a <= key.first && *b >= key.last);
            let target_retained = index.spans.iter().any(|span| {
                span.start <= target + 0.1
                    && span.end >= target - 0.1
                    && bytes.iter().any(|(a, b)| {
                        *a <= key.first.min(span.first) && *b >= span.last.max(key.last)
                    })
            });
            let unsafe_claim = !key_retained
                && index.writes.truncated
                && self.native_bytes.get(name).is_some_and(|ranges| {
                    ranges
                        .iter()
                        .any(|(a, b)| *a <= key.first && *b >= key.last)
                });
            result = SeekAssessment {
                retained: Some(key_retained && target_retained),
                unsafe_native_claim: unsafe_claim,
                reason: if unsafe_claim {
                    "native_claims_overwritten_keyframe"
                } else if !key_retained {
                    "fetch_missing_keyframe"
                } else if !target_retained {
                    "fetch_target_data"
                } else {
                    "retained_video_and_keyframe"
                },
                keyframe_time: Some(key.start),
            };
            if result.retained != Some(true) {
                return result;
            }
        }
        result
    }
    pub fn track(&mut self, source: &gst::Element) {
        let index = Arc::new(Mutex::new(TimeIndex::default()));
        self.indices
            .insert(source.name().to_string(), index.clone());
        let Some(bin) = source.downcast_ref::<gst::Bin>() else {
            return;
        };
        bin.connect_deep_element_added(move |_, _, element| {
            if element.factory().is_some_and(|f| f.name() == "queue2") {
                let capacity = element.property::<u64>("ring-buffer-max-size");
                if capacity > 0 {
                    let index = index.clone();
                    element.static_pad("sink").unwrap().add_probe(
                        gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
                        move |_, info| {
                            let mut index = index.lock().unwrap();
                            if let Some(event) = info.event() {
                                if let gst::EventView::Segment(segment) = event.view() {
                                    index.writes.next = segment
                                        .segment()
                                        .downcast_ref::<gst::format::Bytes>()
                                        .and_then(|s| s.start())
                                        .map(u64::from);
                                }
                            } else if let Some(buffer) = info.buffer() {
                                let start = if buffer.offset() != u64::MAX {
                                    Some(buffer.offset())
                                } else {
                                    index.writes.next
                                };
                                if let Some(start) = start {
                                    let end = start.saturating_add(buffer.size() as u64);
                                    index.writes.add(start, end, capacity);
                                    index.writes.next = Some(end);
                                }
                            }
                            gst::PadProbeReturn::Ok
                        },
                    );
                }
                return;
            }
            if element
                .factory()
                .is_none_or(|f| !matches!(f.name().as_str(), "qtdemux" | "matroskademux"))
            {
                return;
            }
            let read = Arc::new(Mutex::new(None::<(u64, u64)>));
            if let Some(pad) = element.static_pad("sink") {
                let read = read.clone();
                pad.add_probe(
                    gst::PadProbeType::BUFFER | gst::PadProbeType::PULL,
                    move |_, info| {
                        if let Some(b) = info.buffer() {
                            let off = if info.offset != u64::MAX {
                                info.offset
                            } else {
                                b.offset()
                            };
                            *read.lock().unwrap() = (off != u64::MAX)
                                .then_some((off, off.saturating_add(b.size() as u64)));
                        }
                        gst::PadProbeReturn::Ok
                    },
                );
            }
            let index = index.clone();
            element.connect_pad_added(move |_, pad| {
                // The seek track represents video retention. Interleaved audio
                // packets must not move its byte/time anchor backwards.
                let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
                if !caps
                    .structure(0)
                    .is_some_and(|s| s.name().starts_with("video/"))
                {
                    return;
                }
                let read = read.clone();
                let index = index.clone();
                pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    if let Some(b) = info.buffer()
                        && let Some(pts) = b.pts()
                        && let Some((first, last)) = *read.lock().unwrap()
                        && last - first >= b.size() as u64
                        && last - first <= b.size() as u64 + 64
                    {
                        let start = pts.seconds_f64();
                        let end = start + b.duration().map(|d| d.seconds_f64()).unwrap_or(0.);
                        let span = Span {
                            first,
                            last,
                            start,
                            end,
                        };
                        let mut index = index.lock().unwrap();
                        if !b.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                            if index.keyframes.len() == 1024 {
                                index.keyframes.pop_front();
                            }
                            index.keyframes.push_back(span);
                        }
                        index.add(span);
                    }
                    gst::PadProbeReturn::Ok
                });
            });
        });
    }

    pub fn refresh(&mut self, sources: &[gst::Element], duration: Option<gst::ClockTime>) {
        if self
            .checked
            .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.checked = Some(Instant::now());
        let Some(duration) = duration
            .filter(|t| !t.is_zero())
            .map(|t| t.nseconds() as f64)
        else {
            self.ranges.clear();
            self.confirmed.clear();
            return;
        };
        let mut combined: Option<Vec<(f64, f64)>> = None;
        let mut confirmed_combined: Option<Vec<(f64, f64)>> = None;
        self.valid_bytes.clear();
        self.native_bytes.clear();
        self.estimated = false;
        self.diagnostics.clear();
        self.complete.clear();
        for source in sources
            .iter()
            .filter(|s| s.factory().is_some_and(|f| f.name() == "uridecodebin"))
        {
            // This is the video seek track. A separate audio URI (often push-
            // mode WebM, with no byte/time index) must not erase known video
            // retention. It does not claim that seeking needs zero audio I/O.
            if !self.indices.is_empty() && !self.indices.contains_key(source.name().as_str()) {
                continue;
            }
            let mut ranges = None;
            let mut confirmed = Vec::new();
            // Observation only: asking demuxers to convert arbitrary retained
            // byte endpoints can force lazy sample-table parsing on a live
            // streaming task. Use queue2 byte retention and already-observed
            // packet timestamps; the UI must not drive container indexing.
            if let Some(bin) = source.downcast_ref::<gst::Bin>() {
                for element in bin.iterate_recurse().into_iter().flatten() {
                    if element.factory().is_none_or(|f| f.name() != "queue2")
                        || element.property::<u64>("ring-buffer-max-size") == 0
                    {
                        continue;
                    }
                    let Some(pad) = element.static_pad("src") else {
                        continue;
                    };
                    let mut query = gst::query::Buffering::new(gst::Format::Bytes);
                    if pad.query(&mut query) {
                        let bytes: Vec<_> = query
                            .ranges()
                            .into_iter()
                            .filter_map(|(a, b)| {
                                let (a, b) = (a.value(), b.value());
                                (a >= 0 && b > a).then(|| (a as u64, b as u64))
                            })
                            .collect();
                        self.native_bytes
                            .insert(source.name().to_string(), bytes.clone());
                        // Retain raw claims for detecting stale queue2 ranges,
                        // but never trust more than the physical ring capacity.
                        let capacity = element.property::<u64>("ring-buffer-max-size");
                        let bytes: Vec<_> = bytes
                            .into_iter()
                            .map(|(a, b)| (a.max(b.saturating_sub(capacity)), b))
                            .collect();
                        let bytes = self
                            .indices
                            .get(source.name().as_str())
                            .map(|index| index.lock().unwrap().writes.retained(&bytes))
                            .unwrap_or(bytes);
                        self.valid_bytes
                            .insert(source.name().to_string(), bytes.clone());
                        let mut timed = Vec::new();
                        let total = pad
                            .query_duration::<gst::format::Bytes>()
                            .map(u64::from)
                            .filter(|n| *n > 0);
                        let fully_retained = total
                            .is_some_and(|total| bytes.iter().any(|(a, b)| *a == 0 && *b >= total));
                        self.diagnostics.push(serde_json::json!({"source":source.name().to_string(),"bytes":bytes.iter().take(8).copied().collect::<Vec<_>>(),"range_count":bytes.len(),"total":total,"complete":fully_retained}));
                        if fully_retained {
                            self.complete.insert(source.name().to_string());
                            ranges = Some(vec![(0., 1.)]);
                            confirmed = vec![(0., 1.)];
                            break;
                        }

                        if let Some(index) = self.indices.get(source.name().as_str()) {
                            let index = index.lock().unwrap();
                            (timed, confirmed) = index.coverage(&bytes, duration / 1e9);
                        }
                        if let Some(index) = self.indices.get(source.name().as_str()) {
                            let (indexed_anchor, local_rate) = {
                                let index = index.lock().unwrap();
                                (index.current_anchor(), index.recent_rate())
                            };
                            // Release the packet lock before native source queries.
                            // queue2's byte position and the decoded pipeline's
                            // clock are not a matched pair (demux/read-ahead and
                            // interleaved audio differ). Only observed pairs count.
                            let anchor = indexed_anchor;
                            // A one-second low-complexity scene must not make
                            // megabytes of unread VBR data look like the whole
                            // remaining video. Keep the local anchor, but bound
                            // extrapolation with the source's average byte rate.
                            let average = total.map(|n| n as f64 / (duration / 1e9)).unwrap_or(0.);
                            let rate = local_rate
                                .unwrap_or(query.stats().2.max(0) as f64)
                                .max(average * 0.75);
                            let complete_tail = anchor.and_then(|(offset, time)| {
                                total.and_then(|total| {
                                    bytes
                                        .iter()
                                        .any(|(a, b)| *a <= offset && *b >= total)
                                        .then_some((time / (duration / 1e9), 1.))
                                })
                            });
                            if let Some(mut tail) = complete_tail.or_else(|| {
                                anchor.and_then(|a| ahead(&bytes, a, rate, duration / 1e9))
                            }) {
                                if complete_tail.is_none() {
                                    // Reserve 100% for observed EOF coverage,
                                    // never a VBR extrapolation that merely hits it.
                                    tail.1 = tail.1.min((1. - 1. / (duration / 1e9)).max(tail.0));
                                    if let Some(range) = anchor
                                        .and_then(|(off, _)| {
                                            bytes.iter().find(|(a, b)| *a <= off && *b > off)
                                        })
                                        .copied()
                                    {
                                        tail.1 = self
                                            .tails
                                            .entry(source.name().to_string())
                                            .or_default()
                                            .update(range, tail.1);
                                    }
                                }
                                timed.push(tail);
                                self.estimated |= complete_tail.is_none();
                            }
                        }
                        let r = normalized(timed);
                        if !r.is_empty() {
                            ranges = Some(r);
                            break;
                        }
                    }
                }
            }
            // Intersect multiple reported video sources, when present.
            confirmed_combined = Some(match confirmed_combined {
                None => confirmed,
                Some(ref old) => intersection(old, &confirmed),
            });
            let current = ranges.unwrap_or_default();
            combined = Some(match combined {
                None => current,
                Some(ref r) => intersection(r, &current),
            });
        }
        self.ranges = combined.unwrap_or_default();
        self.confirmed = confirmed_combined.unwrap_or_default();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seek_source_activity_expires_without_retaining_payload() {
        let mut snapshot = Snapshot::default();
        let mut index = TimeIndex::default();
        index.writes.add(1, 100, 4096);
        let index = Arc::new(Mutex::new(index));
        snapshot.indices.insert("video".into(), index.clone());
        assert!(snapshot.source_recently_active(Duration::from_secs(3)));
        index.lock().unwrap().writes.last_input = Some(Instant::now() - Duration::from_secs(4));
        assert!(!snapshot.source_recently_active(Duration::from_secs(3)));
    }
    #[test]
    fn complete_and_partial_coverage_do_not_jitter_but_real_eviction_can_shrink() {
        assert_eq!(
            union_bytes(vec![(900, 1000), (0, 100), (100, 900)]),
            vec![(0, 1000)]
        );
        let mut tail = TailEstimate::default();
        assert_eq!(tail.update((0, 500), 0.5), 0.5);
        assert_eq!(tail.update((0, 500), 0.4), 0.5); // VBR estimate changed, bytes didn't.
        assert_eq!(tail.update((100, 600), 0.45), 0.5); // Still retained, more arrived.
        assert_eq!(tail.update((100, 300), 0.3), 0.3); // Actual future bytes removed.
        assert_eq!(tail.update((800, 900), 0.9), 0.9); // Different seek range.
    }
    #[test]
    fn native_stale_header_ranges_are_evicted_after_wrap() {
        let mut w = WriteWindow::default();
        w.add(0, 100, 1000);
        w.add(10000, 10100, 1000);
        w.add(100, 1500, 1000);
        let ranges = w.retained(&[(0, 100), (100, 1500), (10000, 10100)]);
        assert_eq!(ranges, vec![(500, 1500)]);
        w.add(3000, 3100, 1000);
        assert!(!w.retained(&[(0, 100)]).iter().any(|(a, _)| *a == 0));
    }
    #[test]
    fn cached_timestamp_without_its_keyframe_is_not_a_safe_seek() {
        let mut snapshot = Snapshot::default();
        let mut index = TimeIndex::default();
        index.keyframes.push_back(Span {
            first: 100,
            last: 300,
            start: 0.,
            end: 0.1,
        });
        index.keyframes.push_back(Span {
            first: 2000,
            last: 2400,
            start: 10.,
            end: 10.1,
        });
        index.add(Span {
            first: 2500,
            last: 2600,
            start: 11.,
            end: 11.033,
        });
        snapshot
            .indices
            .insert("video".into(), Arc::new(Mutex::new(index)));
        snapshot
            .valid_bytes
            .insert("video".into(), vec![(1500, 5000)]);
        assert_eq!(snapshot.seek_retained(5.), Some(false));
        assert_eq!(snapshot.seek_retained(11.), Some(true));
        snapshot
            .valid_bytes
            .insert("video".into(), vec![(2200, 5000)]);
        assert_eq!(snapshot.seek_retained(11.), Some(false)); // Partial keyframe lost.
        assert!(!snapshot.assess_seek(11.).unsafe_native_claim);
        snapshot
            .native_bytes
            .insert("video".into(), vec![(0, 5000)]);
        snapshot.indices["video"].lock().unwrap().writes.truncated = true;
        assert!(snapshot.assess_seek(11.).unsafe_native_claim);
        assert_eq!(snapshot.assess_seek(-1.).reason, "keyframe_not_observed");
        let index = snapshot.indices["video"].lock().unwrap();
        assert_eq!(index.seekable(&[(2000, 5000)], 100.), vec![(0.11, 0.11033)]);
        assert!(index.seekable(&[(2200, 5000)], 100.).is_empty());
    }
    #[test]
    fn forward_tail_uses_actual_unread_bytes_and_drops_evicted_anchors() {
        assert_eq!(
            ahead(&[(1000, 3000)], (2000, 10.), 1000., 100.),
            Some((0.1, 0.11))
        );
        assert_eq!(
            ahead(&[(1000, 9000)], (2000, 10.), 1000., 100.),
            Some((0.1, 0.17))
        );
        assert!(ahead(&[(2100, 3000)], (2000, 10.), 1000., 100.).is_none());
        assert!(ahead(&[(1000, 2000)], (2000, 10.), 1000., 100.).is_none());
        assert!(ahead(&[(1000, 3000)], (2000, 10.), 0., 100.).is_none());
        assert!(ahead(&[(1000, 3000)], (2000, 10.), f64::NAN, 100.).is_none());
    }
    #[test]
    fn sparse_packet_samples_do_not_stripe_continuous_bytes_after_distant_seeks() {
        let mut index = TimeIndex::default();
        let samples = [
            (100, 200, 0., 0.1),
            (250, 350, 0.15, 0.25),
            (400, 500, 0.4, 0.5),
            (3000, 3100, 50., 50.1),
            (3300, 3400, 50.4, 50.5),
            (1500, 1600, 20., 20.1),
        ];
        for (first, last, start, end) in samples {
            index.add(Span {
                first,
                last,
                start,
                end,
            });
        }
        for (first, last, start, end) in [
            (100, 200, 0., 0.1),
            (3000, 3100, 50., 50.1),
            (1500, 1600, 20., 20.1),
        ] {
            index.keyframes.push_back(Span {
                first,
                last,
                start,
                end,
            });
        }
        let (all, confirmed) = index.coverage(&[(100, 1000), (3000, 4000)], 100.);
        assert_eq!(all, vec![(0., 0.005), (0.5, 0.505)]);
        assert_eq!(confirmed, all);
        // New backward data replaces an evicted old region; the still-resident
        // forward region remains separate, without bridging missing bytes.
        let (all, confirmed) = index.coverage(&[(1000, 2000), (3000, 4000)], 100.);
        assert_eq!(all, vec![(0.2, 0.201), (0.5, 0.505)]);
        assert_eq!(confirmed, all);
        assert!(index.coverage(&[(1700, 2000)], 100.).0.is_empty());
        assert!(index.coverage(&[], 100.).1.is_empty());
    }
    #[test]
    fn packet_times_do_not_assume_constant_bitrate_and_evictions_are_removed() {
        let mut index = TimeIndex::default();
        // Equal-duration media with very unequal packet sizes.
        index.add(Span {
            first: 100,
            last: 200,
            start: 0.,
            end: 1.,
        });
        index.add(Span {
            first: 200,
            last: 1200,
            start: 1.,
            end: 2.,
        });
        index.add(Span {
            first: 1200,
            last: 1300,
            start: 2.,
            end: 3.,
        });
        assert_eq!(index.retained(&[(100, 1200)], 10.), vec![(0., 0.2)]);
        assert_eq!(index.retained(&[(200, 1300)], 10.), vec![(0.1, 0.3)]);
        assert!(index.retained(&[(1300, 9000)], 10.).is_empty());
        for i in 0..6000 {
            index.add(Span {
                first: 10000 + i * 100,
                last: 10001 + i * 100,
                start: i as f64,
                end: i as f64 + 0.5,
            });
        }
        assert_eq!(index.spans.len(), 4096);
    }
    #[test]
    fn ranges_exclude_evictions_gaps_and_missing_audio() {
        let a = normalized(vec![(0.4, 0.6), (0.1, 0.3), (0.2, 0.4), (f64::NAN, 1.)]);
        assert_eq!(a, vec![(0.1, 0.6)]);
        assert_eq!(
            intersection(&a, &[(0.2, 0.25), (0.5, 0.8)]),
            vec![(0.2, 0.25), (0.5, 0.6)]
        );
        assert!(intersection(&a, &[]).is_empty());
        let mut s = Snapshot::default();
        s.ranges = a;
        s.checked = None;
        s.refresh(&[], None);
        assert!(s.ranges.is_empty());
    }
}
