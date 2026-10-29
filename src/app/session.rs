//! Inactive modes retain only bounded metadata, never pipelines or decoded images.
use super::*;
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Collection {
    pub videos: Vec<Video>,
    pub title: String,
    pub playlist: String,
    pub history: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Session {
    pub collection: Collection,
    pub selected: Option<Video>,
    pub queue: Vec<Video>,
    pub queue_index: usize,
    pub queue_is_playlist: bool,
    pub queue_has_current: bool,
    pub music_playlist_pending: bool,
    pub music_shuffle_visited: Vec<String>,
    pub position: f64,
    pub duration: f32,
    pub query: String,
    pub quality: i32,
    pub save_to: i32,
    pub browse_tab: i32,
}
fn resume_point(position: f64, duration: f32) -> f64 {
    if !position.is_finite()
        || position < 0.
        || (duration > 0. && position >= f64::from(duration) - 0.25)
    {
        0.
    } else {
        position
    }
}
pub(super) fn install(
    w: &SetupWindow,
    state: Arc<Mutex<State>>,
    paths: Paths,
    browse: Arc<AtomicU64>,
    player: Arc<AtomicU64>,
    tx: mpsc::Sender<PlayerCommand>,
    browse_tx: mpsc::Sender<BrowseRequest>,
    smoke: bool,
) {
    let weak = w.as_weak();
    w.on_mode_change(move |music| {
        let Some(w) = weak.upgrade() else {
            return;
        };
        if music && w.get_anime_mode() { w.invoke_anime_mode_change(false); }
        if music == w.get_music_layout() {
            return;
        }
        if !music && w.get_offline() {
            w.set_status("Offline — video needs a connection.".into());
            return;
        }
        if w.get_fullscreen_mode() {
            w.invoke_toggle_fullscreen();
        }
        let old_index = usize::from(w.get_music_layout());
        let new_index = usize::from(music);
        browse.fetch_add(1, Ordering::Relaxed);
        player.fetch_add(1, Ordering::Relaxed);
        PLAYBACK_PENDING.store(0, Ordering::Release);
        let _ = tx.send(PlayerCommand::Reset);
        let (
            collection,
            selected,
            position,
            duration,
            query,
            quality,
            save_to,
            browse_tab,
            control,
        ) = {
            let mut st = state.lock().unwrap();
            let old = Session {
                collection: Collection {
                    videos: std::mem::take(&mut st.videos),
                    title: w.get_collection_title().to_string(),
                    playlist: w.get_playlist_id().to_string(),
                    history: w.get_history_view(),
                },
                selected: st.selected.take(),
                queue: std::mem::take(&mut st.queue),
                queue_index: st.queue_index,
                queue_is_playlist: st.queue_is_playlist,
                queue_has_current: st.queue_has_current,
                music_playlist_pending: st.music_playlist_pending,
                music_shuffle_visited: std::mem::take(&mut st.music_shuffle_visited),
                position: resume_point(w.get_position() as f64, w.get_duration()),
                duration: w.get_duration(),
                query: w.get_query().to_string(),
                quality: w.get_quality_index(),
                save_to: w.get_save_to(),
                browse_tab: w.get_browse_tab(),
            };
            #[cfg(target_os = "linux")]
            let restoring = linux_renderer::restoring();
            #[cfg(not(target_os = "linux"))]
            let restoring = false;
            if !restoring { st.sessions[old_index] = Some(old); }
            let session = st.sessions[new_index].take().unwrap_or_else(|| Session {
                quality: 2,
                save_to: if music { 0 } else { -1 },
                ..Session::default()
            });
            // The model must be usable before a background restore/thumbnail job can be cancelled.
            st.videos = session.collection.videos.clone();
            st.selected = session.selected.clone();
            st.music_discover_pending = false;
            st.music_follow_discover = false;
            st.music_playlist_pending = session.music_playlist_pending;
            st.music_shuffle_visited = session.music_shuffle_visited;
            st.queue = session.queue;
            st.queue_index = session.queue_index;
            st.queue_is_playlist = session.queue_is_playlist;
            st.queue_has_current = session.queue_has_current;
            st.active_music = music;
            st.resume_position = Some(session.position);
            st.format_options.clear();
            st.formats_for = None;
            st.related_for = None;
            (
                session.collection,
                session.selected,
                session.position,
                session.duration,
                session.query,
                session.quality,
                session.save_to,
                session.browse_tab,
                st.captions.clone(),
            )
        };
        let saved = {
            let mut st = state.lock().unwrap();
            st.settings.last_music_mode = music;
            st.settings.music = music;
            if smoke {
                Ok(())
            } else {
                paths.save("settings.json", &st.settings)
            }
        };
        if saved.is_err() {
            w.invoke_notify("Mode changed for this session; could not save startup mode.".into());
        }
        w.global::<crate::BrowsePages>().invoke_reset_comments();
        w.global::<crate::BrowsePages>().invoke_close_page();
        w.set_music_layout(music);
        w.set_save_to(save_to);
        w.invoke_library_mode();
        w.set_left_pane_hovered(false);
        w.set_page_scroll(0.);
        w.set_results_scroll(0.);
        play_queue::publish(&w, &state);
        w.set_music_only(music);
        w.set_busy(false);
        w.set_browse_error(false);
        w.set_search_notice(false);
        w.set_playback_active(false);
        w.set_recommendations_pending(false);
        w.set_resolving(false);
        w.set_paused(true);
        w.set_video_visible(false);
        w.set_video_frame(Default::default());
        w.set_selected_image(Default::default());
        w.set_active_decoder("".into());
        w.set_player_error("".into());
        w.set_description_expanded(false);
        w.set_description("".into());
        w.set_subscriber_text("".into());
        w.set_video_stats("".into());
        w.set_rating_text("".into());
        if let Some(control) = control {
            control.publish(&w, "", Vec::new());
        }
        w.set_browse_tab(browse_tab);
        w.set_query(query.into());
        w.set_quality_index(quality);
        w.set_quality_compact_labels(compact_labels(&default_format_options()));
        w.set_quality_labels(ModelRc::new(VecModel::from(
            default_format_options()
                .iter()
                .map(|o| o.label.clone().into())
                .collect::<Vec<slint::SharedString>>(),
        )));
        w.set_quality_choice(if music { 5 } else { quality });
        w.set_position(position as f32);
        w.set_duration(duration);
        w.set_clock(format!("{} / {}", clock(position), clock(duration as f64)).into());
        w.set_playing_download(false);
        w.set_has_selection(selected.is_some());
        w.set_selection_playable(
            selected
                .as_ref()
                .is_some_and(|v| v.playable(music) || (music && paths.audio.contains(&v.id))),
        );
        if let Some(video) = selected {
            w.set_selected_id(video.id.clone().into());
            w.set_selected_title(video.title.clone().into());
            w.set_selected_channel(video.channel.clone().into());
            super::music::cover(&state, video);
        } else {
            w.set_selected_id("".into());
            w.set_selected_title(
                if music {
                    "Choose music to play"
                } else {
                    "Choose a video to play"
                }
                .into(),
            );
            w.set_selected_channel("".into());
        }
        w.set_playback_status("Paused · press Play to resume this mode".into());
        #[cfg(target_os = "linux")]
        if linux_renderer::enabled() && !linux_renderer::restoring() {
            // Snapshot the target collection before any hydration work starts.
            w.set_playlist_id(collection.playlist.clone().into());
            w.set_history_view(collection.history);
            w.set_collection_title(collection.title.clone().into());
            if let Err(error) = linux_renderer::prepare(&w, &state, &paths) {
                linux_renderer::rollback(&w);
                w.invoke_notify(format!("Could not switch renderer: {error}").into());
                return;
            } else {
                let _ = slint::quit_event_loop();
                return;
            }
        }
        if collection.videos.is_empty() {
            browse_view::publish(&w, Vec::new());
            w.set_playlist_id("".into());
            w.set_history_view(false);
            w.set_collection_title("For You".into());
            if paths.network.offline() {
                w.invoke_downloads();
            } else if !smoke || std::env::var_os("MOCHUNE_SMOKE_FRESH_HOME").is_some() {
                if music { w.invoke_home(); } else { w.global::<crate::BrowsePages>().invoke_home(); }
            }
        } else {
            browse_view::publish(
                &w,
                collection
                    .videos
                    .iter()
                    .map(|v| cached_card(&paths, v))
                    .collect::<Vec<_>>(),
            );
            w.set_playlist_id(collection.playlist.clone().into());
            w.set_history_view(collection.history);
            w.set_collection_title(collection.title.clone().into());
            // Hydration may be cancelled by Play; that must never discard the restored list.
            let n = browse.fetch_add(1, Ordering::Relaxed) + 1;
            let mut config = settings(&w);
            committed_preferences(&mut config, &state.lock().unwrap().settings);
            w.set_busy(true);
            let _ = browse_tx.send(BrowseRequest {
                task: Browse::Restore(collection),
                settings: config,
                generation: n,
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_sessions_replay_from_start_and_incomplete_sessions_resume() {
        assert_eq!(resume_point(12., 40.), 12.);
        assert_eq!(resume_point(39.9, 40.), 0.);
        assert_eq!(resume_point(40., 40.), 0.);
        assert_eq!(resume_point(12., 0.), 12.);
        assert_eq!(resume_point(f64::NAN, 40.), 0.);
    }
}
