//! Cheap views over existing metadata: no extra extraction, image copies or reordering of source rows.
use super::*;
/// Keep the same model and row delegates; replace data, not the list UI.
pub(super) fn publish(w: &SetupWindow, mut cards: Vec<VideoCard>) {
    let old = w.get_videos();
    let art: std::collections::HashMap<_, _> = old
        .iter()
        .filter(|row| row.thumbnail.size().width > 0)
        .map(|row| (row.source_id.to_string(), row.thumbnail))
        .collect();
    for card in &mut cards {
        if card.thumbnail.size().width == 0 {
            if let Some(image) = art.get(card.source_id.as_str()) {
                card.thumbnail = image.clone();
            }
        }
    }
    w.set_videos(patch_rows(old, cards));
    // Call after the publishing closure has released State's lock.
    let weak = w.as_weak();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(w) = weak.upgrade() {
            w.invoke_rebuild_recs();
        }
    });
}
fn publish_indices(w: &SetupWindow, mut rows: Vec<i32>) {
    // A few empty delegates become real rows in place as metadata arrives.
    // Both lists virtualize these slots; there are no placeholder images or timers.
    if rows.is_empty() && w.get_busy() && w.get_videos().row_count() == 0 {
        rows.resize(8, -1);
    }
    w.set_rec_indices(patch_rows(w.get_rec_indices(), rows));
}
fn patch_rows<T: Clone + PartialEq + 'static>(old: ModelRc<T>, rows: Vec<T>) -> ModelRc<T> {
    if let Some(model) = old.as_any().downcast_ref::<VecModel<T>>() {
        while model.row_count() > rows.len() {
            model.remove(model.row_count() - 1);
        }
        for (i, row) in rows.into_iter().enumerate() {
            if i >= model.row_count() {
                model.push(row);
            } else if model.row_data(i).as_ref() != Some(&row) {
                model.set_row_data(i, row);
            }
        }
        old
    } else {
        ModelRc::new(VecModel::from(rows))
    }
}

fn indices(videos: &[Video], duration: i32, order: i32) -> Vec<i32> {
    let known = |v: &Video| v.duration.is_finite() && v.duration > 0.;
    let mut rows: Vec<_> = videos
        .iter()
        .enumerate()
        .filter(|(_, v)| match duration {
            1 => known(v) && v.duration < 240.,
            2 => known(v) && (240. ..=1200.).contains(&v.duration),
            3 => known(v) && v.duration > 1200.,
            _ => true,
        })
        .map(|(i, _)| i as i32)
        .collect();
    rows.sort_by(|&a, &b| {
        let va = &videos[a as usize];
        let vb = &videos[b as usize];
        let comparison = match order {
            1 => vb.views.cmp(&va.views),
            2 | 3 => match (known(va), known(vb)) {
                (true, true) if order == 2 => va.duration.total_cmp(&vb.duration),
                (true, true) => vb.duration.total_cmp(&va.duration),
                (a, b) => b.cmp(&a),
            },
            4 | 5 => match (va.published, vb.published) {
                (Some(a), Some(b)) if order == 4 => b.cmp(&a),
                (Some(a), Some(b)) => a.cmp(&b),
                (a, b) => b.is_some().cmp(&a.is_some()),
            },
            _ => std::cmp::Ordering::Equal,
        };
        comparison.then_with(|| a.cmp(&b))
    });
    rows
}
// Negative values are UI-only artist/separator delegates, never playback indices.
fn artist_rows(rows: Vec<i32>, count: usize) -> Vec<i32> {
    let count=count.min(3);
    if count==0 {return rows;}
    (0..count).map(|i|-10-i as i32).chain(std::iter::once(-20)).chain(rows.into_iter().skip(count)).collect()
}
fn pin_search_pick(videos: &[Video], rows: &mut Vec<i32>, id: &str) {
    if let Some(index) = videos.iter().position(|v| v.id == id) {
        rows.retain(|&i| i != index as i32);
        rows.insert(0, index as i32);
    }
}
pub(super) fn install(w: &SetupWindow, state: Arc<Mutex<State>>) {
    let weak = w.as_weak();
    w.on_prepare_browse(move || {
        let Some(w) = weak.upgrade() else {
            return;
        };
        let rows = w.get_rec_indices();
        if rows.row_count() == 0 || rows.row_data(0) == Some(-1) {
            publish_indices(&w, Vec::new());
        }
    });
    let weak = w.as_weak();
    w.on_rebuild_recs(move || {
        let Some(w) = weak.upgrade() else {
            return;
        };
        let state = state.lock().unwrap();
        let (duration, order) = if w.get_music_layout() {
            (
                w.get_music_duration(),
                if w.get_playlist_id() == "liked-songs" || (w.get_browse_tab()==6 && matches!(w.get_music_sort(),1|4|5)) {
                    0
                } else {
                    w.get_music_sort()
                },
            )
        } else if w.get_history_view() {
            (0, 0)
        } else {
            (w.get_rec_duration(), w.get_rec_sort())
        };
        let mut rows = indices(&state.videos, duration, order);
        if w.get_music_layout() && w.get_browse_tab() == 1 {
            if let Some(id) = state.music_search_anchor.as_deref()
                .filter(|id| state.selected.as_ref().is_some_and(|v| v.id == *id))
            {
                pin_search_pick(&state.videos, &mut rows, id);
            }
        }
        if w.get_music_layout() && w.get_music_artist_search() && w.get_browse_tab()==-1 {
            rows=artist_rows(rows,w.global::<crate::BrowsePages>().get_channel_hits().row_count());
        }
        publish_indices(&w, rows);
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    fn rows() -> Vec<Video> {
        [
            (1200., Some(10)),
            (0., None),
            (239., Some(50)),
            (240., Some(0)),
            (1201., Some(50)),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (duration, views))| Video {
            id: i.to_string(),
            duration,
            views,
            ..Video::default()
        })
        .collect()
    }
    #[test]
    fn search_pick_stays_first_even_when_filtered_or_sorted() {
        let videos = rows();
        let mut shown = indices(&videos, 1, 1);
        assert!(!shown.contains(&0));
        pin_search_pick(&videos, &mut shown, "0");
        assert_eq!(shown[0], 0);
        assert_eq!(shown.iter().filter(|&&i| i == 0).count(), 1);
        let mut shown = indices(&videos, 0, 1);
        pin_search_pick(&videos, &mut shown, "0");
        assert_eq!(shown[0], 0);
        assert_eq!(shown.iter().filter(|&&i| i == 0).count(), 1);
    }
    #[test]
    fn loading_slots_become_data_without_replacing_the_model() {
        let model = ModelRc::new(VecModel::from(vec![-1; 8]));
        let identity = model.as_any() as *const dyn std::any::Any as *const ();
        let model = patch_rows(model, (0..50).collect());
        assert_eq!(
            model.as_any() as *const dyn std::any::Any as *const (),
            identity
        );
        assert_eq!(model.row_count(), 50);
        let model = patch_rows(model, vec![4, 2, 0]);
        assert_eq!(
            model.as_any() as *const dyn std::any::Any as *const (),
            identity
        );
        assert_eq!(model.iter().collect::<Vec<_>>(), [4, 2, 0]);
        let model = patch_rows(model, vec![]);
        assert_eq!(
            model.as_any() as *const dyn std::any::Any as *const (),
            identity
        );
        assert_eq!(model.row_count(), 0);
    }
    #[test]
    fn duration_boundaries_exclude_unknown_only_when_filtered() {
        let v = rows();
        assert_eq!(indices(&v, 0, 0), vec![0, 1, 2, 3, 4]);
        assert_eq!(indices(&v, 1, 0), vec![2]);
        assert_eq!(indices(&v, 2, 0), vec![0, 3]);
        assert_eq!(indices(&v, 3, 0), vec![4]);
    }
    #[test]
    fn date_sort_keeps_unknown_dates_last_and_original_order_for_ties() {
        let mut v = rows();
        v[0].published = Some(20);
        v[2].published = Some(10);
        v[3].published = Some(20);
        assert_eq!(indices(&v, 0, 4), vec![0, 3, 2, 1, 4]);
        assert_eq!(indices(&v, 0, 5), vec![2, 0, 3, 1, 4]);
    }
    #[test]
    fn sorting_keeps_source_indices_ties_and_unknowns_last() {
        let v = rows();
        assert_eq!(indices(&v, 0, 1), vec![2, 4, 0, 3, 1]);
        assert_eq!(indices(&v, 0, 2), vec![2, 3, 0, 4, 1]);
        assert_eq!(indices(&v, 0, 3), vec![4, 0, 3, 2, 1]);
        assert_eq!(indices(&v, 2, 2), vec![3, 0]);
        assert_eq!(indices(&v, 0, 0), vec![0, 1, 2, 3, 4]);
    }
}
