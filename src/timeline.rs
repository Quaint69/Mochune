//! Retain the small timeline model across updates and source resets.
use crate::{BufferedRange, SetupWindow};
use slint::{Model, ModelRc, VecModel};
pub(crate) const SLOTS: usize = 32;

/// Render confirmed decodable video ranges brightly, with only the uncovered
/// estimated tail dimmed. Keep the model bounded and reuse its existing rows.
pub fn styled_ranges(all: &[(f64, f64)], confirmed: &[(f64, f64)]) -> Vec<BufferedRange> {
    let mut covers: Vec<_> = confirmed
        .iter()
        .take(16)
        .copied()
        .filter(|(a, b)| a.is_finite() && b.is_finite() && b > a)
        .map(|(a, b)| (a.clamp(0., 1.), b.clamp(0., 1.)))
        .filter(|(a, b)| b > a)
        .collect();
    covers.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::with_capacity(SLOTS);
    for &(a, b) in &covers {
        out.push(BufferedRange {
            start: a as f32,
            end: b as f32,
            estimated: false,
        });
    }
    for &(a, b) in all.iter().take(16) {
        if !a.is_finite() || !b.is_finite() || b <= a {
            continue;
        }
        let (mut at, b) = (a.clamp(0., 1.), b.clamp(0., 1.));
        for &(c, d) in &covers {
            if d <= at || c >= b {
                continue;
            }
            if c > at && out.len() < SLOTS {
                out.push(BufferedRange {
                    start: at as f32,
                    end: c.min(b) as f32,
                    estimated: true,
                });
            }
            at = at.max(d);
        }
        if b > at && out.len() < SLOTS {
            out.push(BufferedRange {
                start: at as f32,
                end: b as f32,
                estimated: true,
            });
        }
    }
    out
}

fn rows(ranges: impl IntoIterator<Item = BufferedRange>) -> Vec<BufferedRange> {
    let mut out: Vec<_> = ranges
        .into_iter()
        .filter(|r| r.start.is_finite() && r.end.is_finite() && r.end > r.start)
        .take(SLOTS)
        .map(|r| BufferedRange {
            start: r.start.clamp(0., 1.),
            end: r.end.clamp(0., 1.),
            estimated: r.estimated,
        })
        .collect();
    out.resize(SLOTS, BufferedRange::default());
    out
}
fn update(
    model: &ModelRc<BufferedRange>,
    ranges: impl IntoIterator<Item = BufferedRange>,
) -> Option<ModelRc<BufferedRange>> {
    let rows = rows(ranges);
    if let Some(existing) = model.as_any().downcast_ref::<VecModel<BufferedRange>>()
        && existing.row_count() == SLOTS
    {
        for (i, row) in rows.into_iter().enumerate() {
            if existing.row_data(i).as_ref() != Some(&row) {
                existing.set_row_data(i, row);
            }
        }
        None
    } else {
        Some(ModelRc::new(VecModel::from(rows)))
    }
}
pub fn publish(w: &SetupWindow, ranges: impl IntoIterator<Item = BufferedRange>) {
    if let Some(model) = update(&w.get_buffered_ranges(), ranges) {
        w.set_buffered_ranges(model);
    }
}
pub fn clear(w: &SetupWindow) {
    publish(w, []);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn estimates_do_not_cover_confirmed_data_and_evictions_remove_bright_rows() {
        let rows = styled_ranges(&[(0., 0.9)], &[(0.2, 0.4), (0.5, 0.7)]);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows.iter().filter(|r| !r.estimated).count(), 2);
        for dim in rows.iter().filter(|r| r.estimated) {
            for bright in rows.iter().filter(|r| !r.estimated) {
                assert!(dim.end <= bright.start || dim.start >= bright.end);
            }
        }
        assert!(
            styled_ranges(&[(0.5, 0.9)], &[])
                .iter()
                .all(|r| r.estimated)
        );
        assert!(styled_ranges(&[(f64::NAN, 1.)], &[(2., 3.)]).is_empty());
        assert!(styled_ranges(&vec![(0., 1.); 100], &vec![(0.2, 0.4); 100]).len() <= SLOTS);
    }
    #[test]
    fn updates_and_evictions_keep_the_same_bounded_model() {
        let r = |start, end| BufferedRange {
            start,
            end,
            estimated: false,
        };
        let model = update(&ModelRc::default(), [r(0., 0.5)]).unwrap();
        for i in 1..100 {
            assert!(update(&model, [r(0., i as f32 / 100.)]).is_none());
            assert_eq!(model.row_count(), SLOTS);
        }
        assert!(update(&model, [r(0.8, 0.9)]).is_none());
        assert_eq!(model.row_data(0), Some(r(0.8, 0.9)));
        assert!(update(&model, []).is_none());
        assert!(model.iter().all(|r| r == BufferedRange::default()));
        assert!(update(&model, [r(f32::NAN, 1.), r(0.5, 0.4)]).is_none());
        assert!(model.iter().all(|r| r == BufferedRange::default()));
    }
}
