//! Conservative, session-only availability. Unknown entries remain selectable.
use super::*;
pub(super) fn metadata(v: &Value) -> (String, bool) {
    let title = v["title"].as_str().unwrap_or("").to_lowercase();
    let formats = v["formats"].as_array();
    let has_stream = formats.is_some_and(|f| f.iter().any(|v| v["url"].is_string()));
    let reason = if !has_stream && (title == "[private video]" || title == "private video") {
        "Private video"
    } else if !has_stream && (title == "[deleted video]" || title == "deleted video") {
        "Deleted video"
    } else if !has_stream && v["availability"] == "needs_auth" {
        if v["age_limit"].as_u64().unwrap_or(0) >= 18 {
            "Requires age verification"
        } else {
            "Sign-in required"
        }
    } else if !has_stream && v["availability"] == "subscriber_only" {
        "Members-only video"
    } else if !has_stream && v["availability"] == "premium_only" {
        "Premium access required"
    } else if v["is_unavailable"] == true || v["isPlayable"] == false {
        "Unavailable"
    } else {
        ""
    };
    let audio_missing = formats.is_some_and(|fs| {
        !fs.is_empty()
            && !fs.iter().any(|f| {
                f["vcodec"] == "none"
                    && f["acodec"].as_str().is_some_and(|c| c != "none")
                    && f["has_drm"] != true
            })
    });
    (reason.into(), audio_missing)
}
pub(super) fn failure(message: &str) -> Option<String> {
    let m = message.to_lowercase();
    if m.contains("not a bot")
        || m.contains("timed out")
        || m.contains("timeout")
        || m.contains("http error 403")
        || m.contains("http error 429")
    {
        return None;
    }
    [
        (
            "not available in your country",
            "Unavailable in your region",
        ),
        (
            "not made this video available in your country",
            "Unavailable in your region",
        ),
        ("blocked it in your country", "Unavailable in your region"),
        ("confirm your age", "Requires age verification"),
        ("age-restricted", "Requires age verification"),
        ("age restricted", "Requires age verification"),
        ("join this channel", "Members-only video"),
        ("members-only", "Members-only video"),
        ("private video", "Private video"),
        ("video has been removed", "Removed video"),
        ("video unavailable", "Unavailable"),
        ("video is not available", "Unavailable"),
    ]
    .into_iter()
    .find_map(|(pattern, label)| m.contains(pattern).then(|| label.into()))
}
pub(super) fn apply(st: &State, video: &mut Video) {
    if let Some((reason, audio_only)) = st.unavailable.get(&video.id) {
        if *audio_only {
            video.audio_unavailable = true;
        } else {
            video.unavailable = reason.clone();
        }
    }
}
pub(super) fn mark(
    state: &Arc<Mutex<State>>,
    weak: &slint::Weak<SetupWindow>,
    id: String,
    reason: String,
    music_only: bool,
) {
    let mut st = state.lock().unwrap();
    if st.unavailable.len() >= 256 && !st.unavailable.contains_key(&id) {
        st.unavailable.pop_first();
    }
    st.unavailable
        .insert(id.clone(), (reason.clone(), music_only));
    let update = |v: &mut Video| {
        if v.id == id {
            if music_only {
                v.audio_unavailable = true;
            } else {
                v.unavailable = reason.clone();
            }
        }
    };
    for v in &mut st.videos {
        update(v);
    }
    for v in &mut st.queue {
        update(v);
    }
    if let Some(v) = st.selected.as_mut() {
        update(v);
    }
    for session in st.sessions.iter_mut().flatten() {
        for v in &mut session.collection.videos {
            update(v);
        }
        for v in &mut session.queue {
            update(v);
        }
        if let Some(v) = session.selected.as_mut() {
            update(v);
        }
    }
    drop(st);
    ui(weak, move |w| {
        for model in [w.get_videos(), w.get_video_playlist_rows()] {
            for i in 0..model.row_count() {
                if let Some(mut row) = model.row_data(i)
                    && row.source_id.as_str() == id
                {
                    if music_only {
                        row.music_available = row.downloaded;
                        row.unavailable_reason = "Audio-only stream unavailable".into();
                    } else {
                        row.available = false;
                        row.music_available = row.downloaded;
                        row.unavailable_reason = reason.clone().into();
                    }
                    model.set_row_data(i, row);
                }
            }
        }
        let saved_music = w.get_music_layout()
            && w.get_videos()
                .iter()
                .any(|row| row.source_id.as_str() == id && row.downloaded);
        if w.get_selected_id().as_str() == id
            && !saved_music
            && (!music_only || w.get_music_layout() || w.get_music_only())
        {
            w.set_selection_playable(false);
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conservative_metadata_and_failures() {
        assert_eq!(
            metadata(&serde_json::json!({"title":"[Private video]"})).0,
            "Private video"
        );
        assert!(
            metadata(&serde_json::json!({"title":"Unknown live stream"}))
                .0
                .is_empty()
        );
        assert!(metadata(&serde_json::json!({"formats":[{"vcodec":"avc1","acodec":"aac"}]})).1);
        assert!(!metadata(&serde_json::json!({"formats":[{"vcodec":"none","acodec":"opus"}]})).1);
        assert!(failure("Sign in to confirm you're not a bot").is_none());
        assert!(failure("HTTP Error 403: Forbidden").is_none());
        assert!(failure("Video unavailable").is_some());
        assert_eq!(
            failure("Video unavailable. This is not available in your country").as_deref(),
            Some("Unavailable in your region")
        );
        assert_eq!(
            failure("Sign in to confirm your age").as_deref(),
            Some("Requires age verification")
        );
        assert!(metadata(&serde_json::json!({"age_limit":18,"availability":"public","formats":[{"url":"https://example.org","vcodec":"avc1"}]})).0.is_empty());
    }
}
