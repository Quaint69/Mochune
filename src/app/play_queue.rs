//! Metadata-only explicit playback queues; no thumbnails or speculative streams.
use super::*;
pub(super) const LIMIT: usize = 500;
pub(super) fn publish(w: &SetupWindow, state: &Arc<Mutex<State>>) {
    let st = state.lock().unwrap();
    w.set_queue_rows(ModelRc::new(VecModel::from(
        st.queue
            .iter()
            .enumerate()
            .map(|(index, v)| crate::QueueCard {
                title: v.title.clone().into(),
                channel: v.channel.clone().into(),
                active: st.queue_has_current && index == 0,
            })
            .collect::<Vec<_>>(),
    )));
}
fn append(queue: &mut Vec<Video>, video: Video) -> bool {
    if video.id.is_empty() || queue.len() >= LIMIT || queue.iter().any(|v| v.id == video.id) {
        return false;
    }
    queue.push(video);
    true
}
/// Video's first row is current; selecting another video replaces only that row.
pub(super) fn current(st: &mut State, video: Video) {
    if st.queue_has_current && !st.queue.is_empty() {
        st.queue.remove(0);
    }
    st.queue.retain(|v| v.id != video.id);
    st.queue.truncate(LIMIT - 1);
    st.queue.insert(0, video.clone());
    st.selected = Some(video);
    st.queue_has_current = true;
    st.queue_index = 0;
    st.queue_is_playlist = st.queue.len() > 1;
}
pub(super) fn enqueue(st: &mut State, video: Video) -> bool {
    if !video.playable(false) {
        return false;
    }
    let added = append(&mut st.queue, video);
    st.queue_is_playlist = st.queue.len() > 1;
    added
}
/// Remove the completed current row, then promote the next playable pending item.
pub(super) fn finish(st: &mut State) -> Option<Video> {
    if st.queue_has_current && !st.queue.is_empty() {
        st.queue.remove(0);
    }
    st.queue_has_current = false;
    st.queue.retain(|v| v.playable(false));
    st.queue_index = 0;
    st.queue_is_playlist = st.queue.len() > 1;
    st.queue.first().cloned()
}
pub(super) fn replace(st: &mut State, videos: &[Video]) {
    st.queue.clear();
    st.queue_has_current = false;
    for v in videos {
        append(&mut st.queue, v.clone());
    }
    st.queue_is_playlist = true;
    st.queue_index = st
        .queue
        .iter()
        .position(|v| st.selected.as_ref().is_some_and(|s| s.id == v.id))
        .unwrap_or(0);
}
pub(super) fn play(w: &SetupWindow, state: &Arc<Mutex<State>>, video: Video) {
    {
        let mut s = state.lock().unwrap();
        current(&mut s, video.clone());
        s.resume_position = None;
        s.related_for = None;
        s.removed_history[usize::from(w.get_music_layout())].remove(&video.id);
        s.format_options.clear();
        s.formats_for = None;
    }
    select(w, &video, -1);
    publish(w, state);
    music::cover(state, video);
    w.invoke_play();
}
pub(super) fn install(w: &SetupWindow, state: Arc<Mutex<State>>, paths: Paths) {
    let weak = w.as_weak();
    let st = state.clone();
    w.on_queue_add(move |index| {
        if let Some(w) = weak.upgrade() {
            let added = {
                let mut s = st.lock().unwrap();
                let video = if index < 0 {
                    s.selected.clone()
                } else {
                    s.videos.get(index as usize).cloned()
                };
                if let Some(v) = video.filter(|v| {
                    v.playable(w.get_music_layout())
                        || (w.get_music_layout() && paths.audio.contains(&v.id))
                }) {
                    let added = enqueue(&mut s, v);
                    added
                } else {
                    false
                }
            };
            publish(&w, &st);
            w.set_status(
                if added {
                    "Added to queue"
                } else {
                    "Already queued, unavailable, or queue is full (500 items)"
                }
                .into(),
            );
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    w.on_queue_remove(move |index| {
        if let Some(w) = weak.upgrade() {
            {
                let mut s = st.lock().unwrap();
                if (index as usize) < s.queue.len() && !(index == 0 && s.queue_has_current) {
                    s.queue.remove(index as usize);
                    s.queue_index = s
                        .queue
                        .iter()
                        .position(|v| s.selected.as_ref().is_some_and(|p| p.id == v.id))
                        .unwrap_or(0);
                }
            }
            publish(&w, &st);
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    w.on_queue_clear(move || {
        if let Some(w) = weak.upgrade() {
            {
                let mut s = st.lock().unwrap();
                let keep_current = s.queue_has_current;
                s.queue.truncate(usize::from(keep_current));
                s.queue_index = 0;
                s.queue_is_playlist = false;
            }
            publish(&w, &st);
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    w.on_queue_choose(move |index| {
        if let Some(w) = weak.upgrade() {
            let video = st.lock().unwrap().queue.get(index as usize).cloned();
            if let Some(video) = video {
                play(&w, &st, video);
            }
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    fn v(id: &str) -> Video {
        Video {
            id: id.into(),
            title: id.into(),
            ..Video::default()
        }
    }
    #[test]
    fn queues_are_bounded_unique_and_replace_without_stopping_selection() {
        let mut s = State::default();
        s.selected = Some(v("playing"));
        replace(&mut s, &[v("one"), v("two"), v("one")]);
        assert_eq!(
            s.queue.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(s.selected.as_ref().unwrap().id, "playing");
        assert!(s.queue_is_playlist);
        assert!(!append(&mut s.queue, v("")));
        for i in 0..LIMIT {
            append(&mut s.queue, v(&format!("id{i}")));
        }
        assert_eq!(s.queue.len(), LIMIT);
        assert!(!append(&mut s.queue, v("overflow")));
    }
    #[test]
    fn video_current_replaces_only_current_and_completed_entries_are_consumed() {
        let mut s = State::default();
        current(&mut s, v("first"));
        assert!(enqueue(&mut s, v("next")));
        assert!(enqueue(&mut s, v("last")));
        current(&mut s, v("manual"));
        assert_eq!(
            s.queue.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["manual", "next", "last"]
        );
        let n = finish(&mut s).unwrap();
        current(&mut s, n);
        assert_eq!(
            s.queue.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["next", "last"]
        );
        let n = finish(&mut s).unwrap();
        current(&mut s, n);
        assert!(finish(&mut s).is_none());
        assert!(s.queue.is_empty());
    }
    #[test]
    fn selecting_a_pending_video_keeps_other_pending_items_without_duplicates() {
        let mut s = State::default();
        current(&mut s, v("first"));
        enqueue(&mut s, v("second"));
        enqueue(&mut s, v("third"));
        current(&mut s, v("third"));
        assert_eq!(
            s.queue.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["third", "second"]
        );
        current(&mut s, v("third"));
        assert_eq!(s.queue.len(), 2);
    }
    #[test]
    fn a_thumbnail_preview_is_not_a_current_queue_entry() {
        let mut s = State::default();
        s.selected = Some(v("preview"));
        enqueue(&mut s, v("preview"));
        assert!(!s.queue_has_current);
        current(&mut s, v("manual"));
        assert_eq!(
            s.queue.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["manual", "preview"]
        );
        let next = finish(&mut s).unwrap();
        assert!(!s.queue_has_current);
        current(&mut s, next);
        assert!(s.queue_has_current);
        assert!(finish(&mut s).is_none());
        assert!(!s.queue_has_current);
    }
}
