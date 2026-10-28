//! Music UI coordination. Serialized library writes keep filesystem syncs off the UI thread.
use super::*;
use crate::{
    PlaylistCard,
    playlists::{self, Library, Store, Track},
};

enum Edit {
    Activate,
    Select(String),
    Create(String),
    Rename(String, String),
    Delete(String),
    Add(String, Track),
    ToggleLike(Track),
    AddMany(String, Vec<Track>),
    Remove(String, usize),
    Move(String, usize, i32),
    Play(usize),
    Enqueue(usize),
    EnqueueAll,
    ImportAll(PathBuf),
    ExportAll(PathBuf),
    Import(PathBuf),
    Export(PathBuf, Option<String>),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaylistBackup {
    schema: String,
    version: u32,
    video: Library,
    music: Library,
}
impl PlaylistBackup {
    fn encode(libraries: &[Library; 2]) -> Result<Vec<u8>> {
        let backup = Self { schema: "io.mochune.playlist-backup".into(), version: 1,
            video: libraries[0].clone(), music: libraries[1].clone() };
        check_import(&backup.video, false, false)?;
        check_import(&backup.music, true, false)?;
        backup.video.validate()?;
        backup.music.validate()?;
        let bytes = serde_json::to_vec_pretty(&backup)?;
        anyhow::ensure!(bytes.len() <= playlists::MAX_BYTES * 2, "Backup exceeds 16 MiB");
        Ok(bytes)
    }
    fn decode(bytes: &[u8]) -> Result<[Library; 2]> {
        anyhow::ensure!(bytes.len() <= playlists::MAX_BYTES * 2, "Backup exceeds 16 MiB");
        let backup: Self = serde_json::from_slice(bytes).context("Choose an All playlists backup, or select Music/Video for a single library")?;
        anyhow::ensure!(matches!(backup.schema.as_str(), "io.mochune.playlist-backup" | "io.ducktube.playlist-backup") && backup.version == 1, "Unsupported playlist backup format");
        let mut libraries = [backup.video, backup.music];
        for (i, library) in libraries.iter_mut().enumerate() {
            // Validate schema/version as well as the tracks before touching either library.
            *library = Library::from_json(&library.to_json()?)?;
            check_import(library, i == 1, false)?;
        }
        libraries[1].ensure_liked_songs();
        Ok(libraries)
    }
}
fn transfer_all(edit: &Edit, libraries: &mut [Library; 2], stores: &[Store; 2], errors: &[Option<String>; 2]) -> Result<String> {
    anyhow::ensure!(errors.iter().all(Option::is_none), "A library could not be loaded; repair it before importing/exporting all");
    match edit {
        Edit::ExportAll(path) => {
            playlists::write_atomic(path, &PlaylistBackup::encode(libraries)?, false)?;
            Ok("Exported video and music playlists".into())
        }
        Edit::ImportAll(path) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            fs::File::open(path)?.take((playlists::MAX_BYTES * 2 + 1) as u64).read_to_end(&mut bytes)?;
            let incoming = PlaylistBackup::decode(&bytes)?;
            let mut drafts = libraries.clone();
            let mut count = 0;
            for (draft, incoming) in drafts.iter_mut().zip(incoming) { count += draft.merge(incoming)?; }
            drafts[1].ensure_liked_songs();
            for draft in &drafts { draft.to_json()?; }
            stores[0].save(&drafts[0])?;
            if let Err(error) = stores[1].save(&drafts[1]) {
                stores[0].save(&libraries[0]).context("Music import failed and the video library rollback failed; library backup retained")?;
                return Err(error.context("Import cancelled; video library restored"));
            }
            *libraries = drafts;
            Ok(format!("Imported {count} new tracks into separate video and music libraries"))
        }
        _ => unreachable!(),
    }
}

fn selected_library_id(w: &SetupWindow) -> String {
    if w.get_music_layout() {
        w.get_playlist_id().into()
    } else {
        w.get_video_playlist_id().into()
    }
}
fn track_video(t: &Track) -> Video {
    Video {
        id: t.youtube_id().unwrap_or("").into(),
        title: t.title.clone(),
        channel: t.artist.clone(),
        duration: t.duration_seconds.unwrap_or(0.),
        views: t.extra.get("view_count").and_then(Value::as_u64),
        music: t.extra.get("music").and_then(Value::as_bool),
        genres: t.extra.get("genres").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).take(16).map(str::to_owned).collect()).unwrap_or_default(),
        ..Video::default()
    }
}
pub(super) fn liked_seeds(library: &Library, cursor: usize) -> Vec<Video> {
    let Ok(liked) = library.get("liked-songs") else { return Vec::new(); };
    // Keep only lightweight references while sampling across the entire library.
    let tracks = liked.tracks.iter().filter(|t| t.youtube_id().is_some()).collect::<Vec<_>>();
    let count = tracks.len().min(10);
    (0..count).map(|i| {
        let index = (cursor % tracks.len() + i * tracks.len() / count) % tracks.len();
        let mut v = track_video(tracks[index]);
        v.music = Some(true);
        v
    }).collect()
}
fn check_import(library: &Library, music: bool, legacy: bool) -> Result<()> {
    let expected = if music { "music" } else { "video" };
    let actual = library
        .extra
        .get("media_kind")
        .and_then(Value::as_str)
        .unwrap_or(if legacy { expected } else { "music" });
    anyhow::ensure!(
        actual == expected,
        "This is a {actual} library. Select {actual} playlists before importing."
    );
    Ok(())
}
pub(super) fn cover(state: &Arc<Mutex<State>>, video: Video) {
    let state = state.lock().unwrap();
    if state.active_music
        && let Some(tx) = &state.cover_tx
    {
        let _ = tx.send((video, true));
    }
}
pub(super) fn audio_cover(state: &Arc<Mutex<State>>, video: Video) {
    if let Some(tx) = &state.lock().unwrap().cover_tx {
        let _ = tx.send((video, false));
    }
}

fn refresh(
    weak: &slint::Weak<SetupWindow>,
    state: &Arc<Mutex<State>>,
    library: &Library,
    view: &Option<String>,
    generation: &Arc<AtomicU64>,
    force: bool,
    replace_queue: bool,
    selected_mode: Option<bool>,
    audio: &Arc<audio_cache::Store>,
) {
    // Only send sidebar metadata and the current collection across to the UI.
    // No decoded art or full-library clone is retained by the view.
    let music_library = library.extra.get("media_kind").and_then(Value::as_str) == Some("music");
    let liked = library.get("liked-songs").ok();
    let liked_ids = liked
        .map(|p| {
            p.tracks
                .iter()
                .filter_map(|t| t.youtube_id().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let lists = library
        .playlists
        .iter()
        .map(|p| PlaylistCard {
            id: p.id.clone().into(),
            name: p.name.clone().into(),
            count: format!("{} tracks", p.tracks.len()).into(),
        })
        .collect::<Vec<_>>();
    let names = library
        .playlists
        .iter()
        .map(|p| slint::SharedString::from(&p.name))
        .collect::<Vec<_>>();
    let collection = view
        .as_ref()
        .and_then(|id| library.get(id).ok())
        .map(|p| {
            (
                p.id.clone(),
                p.name.clone(),
                p.tracks.iter().map(track_video).collect::<Vec<_>>(),
            )
        })
        .map(|(id, name, mut videos)| {
            let st = state.lock().unwrap();
            for video in &mut videos {
                if video.views.is_none() {
                    video.views = st
                        .history[usize::from(music_library)]
                        .iter()
                        .find(|v| v.id == video.id)
                        .and_then(|v| v.views);
                }
                availability::apply(&st, video);
            }
            (id, name, videos)
        });
    let audio = audio.clone();
    let state = state.clone();
    let generation = generation.clone();
    ui(weak, move |w| {
        if music_library {
            w.set_liked_ids(ModelRc::new(VecModel::from(
                liked_ids
                    .into_iter()
                    .map(slint::SharedString::from)
                    .collect::<Vec<_>>(),
            )));
            w.set_likes_revision(w.get_likes_revision().wrapping_add(1));
        }
        if selected_mode.is_some_and(|music| music != w.get_music_layout()) {
            return;
        }
        let selected = w.get_save_to().max(0) as usize;
        let count = lists.len();
        w.set_playlists(ModelRc::new(VecModel::from(lists)));
        w.set_playlist_names(ModelRc::new(VecModel::from(names)));
        w.set_save_to(selected.min(count.saturating_sub(1)) as i32);
        w.set_export_playlist(
            (w.get_export_playlist().max(0) as usize).min(count.saturating_sub(1)) as i32,
        );
        if !w.get_music_layout() {
            if let Some((id, name, videos)) = collection {
                let index = w
                    .get_playlists()
                    .iter()
                    .position(|p| p.id.as_str() == id)
                    .map(|i| i as i32)
                    .unwrap_or(-1);
                w.set_save_to(index);
                w.set_video_playlist_id(id.into());
                w.set_video_playlist_name(name.into());
                w.set_video_playlist_rows(ModelRc::new(VecModel::from(
                    videos.iter().map(Video::card).collect::<Vec<_>>(),
                )));
            } else {
                w.set_save_to(-1);
                w.set_video_playlist_id("".into());
                w.set_video_playlist_name("".into());
                w.set_video_playlist_rows(Default::default());
            }
            if force {
                w.set_library_scroll(0.);
            }
            return;
        }
        if force && collection.is_none() && w.get_music_layout() {
            generation.fetch_add(1, Ordering::Relaxed);
            state.lock().unwrap().videos.clear();
            browse_view::publish(&w, Vec::new());
            w.set_history_view(false);
            w.set_playlist_id("".into());
            w.set_collection_title("For You".into());
            w.set_browse_tab(0);
            w.set_busy(false);
        }
        if let Some((id, name, videos)) = collection {
            if selected_mode.is_some_and(|music| music != w.get_music_layout()) {
                return;
            }
            if replace_queue {
                w.set_music_sort(0);
                w.set_music_duration(0);
                w.set_shuffle(false);
                {
                    let mut st = state.lock().unwrap();
                    st.music_discover_pending = false;
                    st.music_follow_discover = false;
                }
                generation.fetch_add(1, Ordering::Relaxed);
                w.set_busy(false);
                {
                    let mut st = state.lock().unwrap();
                    stage_playlist(&mut st, &videos);
                }
                play_queue::publish(&w, &state);
            }
            if id == "liked-songs" {
                w.set_music_sort(0);
                w.set_music_duration(0);
            }
            if force {
                if let Some(i) = w.get_playlists().iter().position(|p| p.id.as_str() == id) {
                    w.set_save_to(i as i32);
                }
            }
            // Video's sidebar remains recommendations; playlists only replace its queue.
            if !w.get_music_layout() {
                return;
            }
            if !force && w.get_playlist_id().as_str() != id {
                return;
            }
            generation.fetch_add(1, Ordering::Relaxed);
            browse_view::publish(
                &w,
                videos
                    .iter()
                    .map(|v| {
                        let mut card = v.card();
                        card.downloaded = audio.contains(&v.id);
                        card
                    })
                    .collect::<Vec<_>>(),
            );
            {
                let mut st = state.lock().unwrap();
                if st.queue_is_playlist && w.get_playlist_id().as_str() == id {
                    st.queue = videos.clone();
                    st.queue_index = st
                        .queue
                        .iter()
                        .position(|v| st.selected.as_ref().is_some_and(|c| c.id == v.id))
                        .unwrap_or(0);
                }
                st.videos = videos;
            }
            play_queue::publish(&w, &state);
            w.set_history_view(false);
            w.set_playlist_id(id.into());
            w.set_collection_title(name.into());
            w.set_busy(false);
            w.set_browse_tab(3);
            if force {
                w.set_results_scroll(0.);
            }
            w.invoke_rebuild_recs();
            let st = state.lock().unwrap();
            if let Some(tx) = &st.collection_art_tx {
                let _ = tx.send((generation.load(Ordering::Relaxed), st.videos.iter().take(play_queue::LIMIT).cloned().collect()));
            }
        }
    });
}
pub(super) fn begin_search_discovery(st: &mut State, current: &Video) {
    // Search is a way to choose one song, not an implicit search-results playlist.
    // Never fall through to those other results if recommendation fetching fails.
    st.queue = vec![current.clone()];
    st.queue_index = 0;
    st.queue_is_playlist = false;
    st.music_search_anchor = Some(current.id.clone());
}

pub(super) fn sync_discovery_queue(
    w: &SetupWindow,
    state: &Arc<Mutex<State>>,
    videos: &[Video],
    finished: bool,
) {
    let mut st = state.lock().unwrap();
    if !st.music_follow_discover || !st.active_music || st.queue_is_playlist {
        return;
    }
    st.queue = discovery_queue(st.selected.as_ref(), videos);
    st.queue_index = 0;
    if finished {
        st.music_follow_discover = false;
    }
    drop(st);
    play_queue::publish(w, state);
}
pub(super) fn discovery_rows(
    current: Option<&Video>,
    mut videos: Vec<Video>,
    limit: usize,
) -> Vec<Video> {
    if let Some(current) = current {
        videos.retain(|v| v.id != current.id);
        videos.insert(0, current.clone());
    }
    videos.truncate(limit);
    videos
}
fn discovery_queue(current: Option<&Video>, videos: &[Video]) -> Vec<Video> {
    let mut queue = Vec::with_capacity(videos.len() + 1);
    if let Some(v) = current {
        queue.push(v.clone());
    }
    for v in videos.iter().filter(|v| v.playable(true)) {
        if !queue.iter().any(|q| q.id == v.id) {
            queue.push(v.clone());
        }
        if queue.len() >= play_queue::LIMIT {
            break;
        }
    }
    queue
}

pub(super) fn install(
    w: &SetupWindow,
    state: Arc<Mutex<State>>,
    paths: Paths,
    generation: Arc<AtomicU64>,
    smoke: bool,
) {
    let repeat_window = w.as_weak();
    w.on_toggle_music_repeat(move || {
        if let Some(w) = repeat_window.upgrade() {
            let (list, song) = repeat_cycle(w.get_repeat(), w.get_repeat_one());
            w.set_repeat(list);
            w.set_repeat_one(song);
        }
    });
    let art_generation = generation.clone();
    let shuffle_state = state.clone();
    w.on_reset_shuffle(move || shuffle_state.lock().unwrap().music_shuffle_visited.clear());
    let toast_timer = std::rc::Rc::new(slint::Timer::default());
    let weak_toast = w.as_weak();
    w.on_notify(move |message| {
        if let Some(w) = weak_toast.upgrade() {
            w.set_toast_text(message);
            w.set_toast_visible(true);
            let weak = w.as_weak();
            toast_timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_secs(2),
                move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_toast_visible(false);
                    }
                },
            );
        }
    });
    let (cover_tx, cover_rx) = mpsc::channel::<(Video, bool)>();
    state.lock().unwrap().cover_tx = Some(cover_tx);
    let cover_state = Arc::downgrade(&state);
    let cover_weak = w.as_weak();
    let cover_paths = paths.clone();
    thread::spawn(move || {
        while let Ok(mut job) = cover_rx.recv() {
            for newer in cover_rx.try_iter() { job = newer; }
            let (video, music) = job;
            let Some(state) = cover_state.upgrade() else {
                break;
            };
            let current = {
                let state = state.lock().unwrap();
                state.active_music == music && state.selected.as_ref().is_some_and(|v| v.id == video.id)
            };
            drop(state);
            if !current {
                continue;
            }
            let image = thumbnail_sized(&cover_paths, &video, 480, 360);
            if let Ok((pixels, width, height)) = image {
                let st = cover_state.clone();
                ui(&cover_weak, move |w| {
                    if let Some(state) = st.upgrade()
                        && state
                            .lock()
                            .unwrap()
                            .selected
                            .as_ref()
                            .is_some_and(|v| v.id == video.id)
                    {
                        if w.get_music_layout() == music && (music || w.get_music_only()) {
                            let image = slint::Image::from_rgba8(slint::SharedPixelBuffer::<
                                slint::Rgba8Pixel,
                            >::clone_from_slice(
                                &pixels, width, height
                            ));
                            w.set_selected_image(image);
                        }
                    }
                });
            }
        }
    });
    let (tx, rx) = mpsc::channel::<(bool, Edit)>();
    let stores = [
        Store {
            path: paths.data.join("video-library.json"),
        },
        Store {
            path: paths.data.join("library.json"),
        },
    ];
    let weak = w.as_weak();
    let st = state.clone();
    let audio = paths.audio.clone();
    let initial_mode = w.get_music_layout();
    thread::spawn(move || {
        let loaded = [stores[0].load_or(Library::empty), stores[1].load()];
        let read_errors = loaded
            .each_ref()
            .map(|r| r.as_ref().err().map(ToString::to_string));
        let mut libraries = loaded.map(|r| r.unwrap_or_else(|_| Library::empty()));
        for (i, library) in libraries.iter_mut().enumerate() {
            library.extra.insert(
                "media_kind".into(),
                if i == 1 { "music" } else { "video" }.into(),
            );
        }
        libraries[1].ensure_liked_songs();
        let mut views: [Option<String>; 2] = [None, None];
        refresh(
            &weak,
            &st,
            &libraries[1],
            &None,
            &generation,
            false,
            false,
            Some(true),
            &audio,
        );
        let index = usize::from(initial_mode);
        refresh(
            &weak,
            &st,
            &libraries[index],
            &views[index],
            &generation,
            false,
            false,
            Some(initial_mode),
            &audio,
        );
        while let Ok((mode, edit)) = rx.recv() {
            if matches!(&edit, Edit::ImportAll(_) | Edit::ExportAll(_)) {
                let result = transfer_all(&edit, &mut libraries, &stores, &read_errors);
                if result.is_ok() && matches!(&edit, Edit::ImportAll(_)) {
                    for i in 0..2 {
                        refresh(&weak, &st, &libraries[i], &views[i], &generation, false, false, Some(i == 1), &audio);
                    }
                }
                ui(&weak, move |w| w.invoke_notify(match result { Ok(s) => s, Err(e) => format!("Playlist: {e}") }.into()));
                continue;
            }
            let file_transfer = matches!(&edit, Edit::Import(_) | Edit::Export(_, _));
            let index = usize::from(mode);
            let library = &mut libraries[index];
            let view = &mut views[index];
            let store = &stores[index];
            let force = matches!(&edit, Edit::Select(_) | Edit::Create(_))
                || matches!(&edit,Edit::Delete(id) if view.as_ref()==Some(id));
            let replace_queue = mode && matches!(&edit, Edit::Select(_));
            let refresh_needed = !matches!(&edit, Edit::Play(_) | Edit::Enqueue(_) | Edit::EnqueueAll);
            let feedback = !matches!(
                &edit,
                Edit::Activate | Edit::Select(_) | Edit::Play(_) | Edit::Enqueue(_) | Edit::EnqueueAll
            );
            let result: Result<String> = (|| {
                match &edit {
                    Edit::Activate => return Ok(String::new()),
                    Edit::Select(id) => {
                        library.get(id)?;
                        *view = Some(id.clone());
                        return Ok(String::new());
                    }
                    Edit::EnqueueAll => {
                        anyhow::ensure!(!mode, "Video playlists only");
                        let list = library.get(view.as_deref().context("Choose a playlist first")?)?;
                        let videos = list.tracks.iter().take(play_queue::LIMIT).map(track_video).collect::<Vec<_>>();
                        let state = st.clone();
                        ui(&weak, move |w| {
                            if w.get_music_layout() { return; }
                            let mut st = state.lock().unwrap();
                            let mut added = 0;
                            for mut video in videos {
                                availability::apply(&st, &mut video);
                                if video.playable(false) && play_queue::enqueue(&mut st, video) { added += 1; }
                            }
                            drop(st);
                            play_queue::publish(&w, &state);
                            w.invoke_notify(format!("Added {added} videos to queue").into());
                        });
                        return Ok(String::new());
                    }
                    Edit::Play(i) | Edit::Enqueue(i) => {
                        let list =
                            library.get(view.as_deref().context("Choose a playlist first")?)?;
                        let mut video =
                            track_video(list.tracks.get(*i).context("Track no longer exists")?);
                        availability::apply(&st.lock().unwrap(), &mut video);
                        anyhow::ensure!(
                            video.playable(mode),
                            "{}",
                            if video.unavailable.is_empty() {
                                "This track cannot be played in the current mode"
                            } else {
                                &video.unavailable
                            }
                        );
                        let enqueue = matches!(edit, Edit::Enqueue(_));
                        let state = st.clone();
                        ui(&weak, move |w| {
                            if w.get_music_layout() != mode {
                                return;
                            }
                            if enqueue {
                                let added = play_queue::enqueue(&mut state.lock().unwrap(), video);
                                play_queue::publish(&w, &state);
                                w.invoke_notify(
                                    if added {
                                        "Added to queue"
                                    } else {
                                        "Already queued, unavailable, or queue is full"
                                    }
                                    .into(),
                                );
                            } else {
                                play_queue::play(&w, &state, video);
                            }
                        });
                        return Ok(String::new());
                    }
                    Edit::Export(path, id) => {
                        if let Some(error) = &read_errors[index] {
                            bail!("Library is read-only: {error}");
                        }
                        let m3u = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                            e.eq_ignore_ascii_case("m3u8") || e.eq_ignore_ascii_case("m3u")
                        });
                        let bytes =
                            if m3u {
                                playlists::to_m3u(library.get(
                                    id.as_deref().context("Choose a playlist to export M3U8")?,
                                )?)?
                                .into_bytes()
                            } else {
                                match id {
                                    Some(id) => library.selected(id)?.to_json()?,
                                    None => library.to_json()?,
                                }
                            };
                        playlists::write_atomic(path, &bytes, false)?;
                        return Ok(format!("Exported {}", path.display()));
                    }
                    _ => {}
                }
                if let Some(error) = &read_errors[index] {
                    bail!("Library is read-only: {error}");
                }
                let mut draft = library.clone();
                let mut next_view = view.clone();
                let message = match edit {
                    Edit::Create(name) => {
                        next_view = Some(draft.create(&name)?);
                        "Playlist created".into()
                    }
                    Edit::Rename(id, name) => {
                        draft.rename(&id, &name)?;
                        "Playlist renamed".into()
                    }
                    Edit::Delete(id) => {
                        draft.delete(&id)?;
                        if next_view.as_ref() == Some(&id) {
                            next_view = None;
                        }
                        "Playlist deleted".into()
                    }
                    Edit::ToggleLike(t) => {
                        anyhow::ensure!(mode, "Likes are available in music mode");
                        if draft.toggle_liked_song(t)? {
                            "Added to Liked songs".into()
                        } else {
                            "Removed from Liked songs".into()
                        }
                    }
                    Edit::Add(id, t) => {
                        if draft.add(&id, t)? {
                            "Saved to playlist".into()
                        } else {
                            "Already in this playlist".into()
                        }
                    }
                    Edit::AddMany(id, tracks) => {
                        let mut count = 0;
                        for track in tracks {
                            count += usize::from(draft.add(&id, track)?);
                        }
                        format!("Saved {count} new entries to playlist")
                    }
                    Edit::Remove(id, i) => {
                        draft.remove(&id, i)?;
                        "Track removed".into()
                    }
                    Edit::Move(id, i, d) => {
                        draft.move_track(&id, i, d)?;
                        "Playlist order saved".into()
                    }
                    Edit::Import(path) => {
                        let incoming = playlists::import_file(&path)?;
                        let m3u = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                            e.eq_ignore_ascii_case("m3u8") || e.eq_ignore_ascii_case("m3u")
                        });
                        check_import(&incoming, mode, m3u)?;
                        let n = draft.merge(incoming)?;
                        format!("Imported {n} new tracks")
                    }
                    _ => unreachable!(),
                };
                if mode {
                    draft.ensure_liked_songs();
                }
                draft.extra.insert(
                    "media_kind".into(),
                    if mode { "music" } else { "video" }.into(),
                );
                store.save(&draft)?;
                *library = draft;
                *view = next_view;
                Ok(message)
            })();
            match result {
                Ok(message) => {
                    if refresh_needed {
                        refresh(
                            &weak,
                            &st,
                            library,
                            view,
                            &generation,
                            force,
                            replace_queue,
                            Some(mode),
                            &audio,
                        );
                    }
                    if feedback {
                        ui(&weak, move |w| {
                            if file_transfer || w.get_music_layout() == mode {
                                w.invoke_notify(message.into());
                                w.set_playlist_editor(false);
                            }
                        });
                    }
                }
                Err(error) => {
                    let message = format!("Playlist: {error}");
                    ui(&weak, move |w| {
                        if file_transfer || w.get_music_layout() == mode {
                            w.invoke_notify(message.into());
                        }
                    });
                }
            }
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_library_mode(move || {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((w.get_music_layout(), Edit::Activate));
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    let p = paths.clone();
    w.on_next(move |automatic| {
        if let Some(w) = weak.upgrade() {
            advance(&w, &st, &p, 1, automatic);
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    let p = paths.clone();
    w.on_previous(move || {
        if let Some(w) = weak.upgrade() {
            if w.get_position() > 3. {
                w.invoke_seek(0.);
            } else {
                advance(&w, &st, &p, -1, false);
            }
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_select(move |id| {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((w.get_music_layout(), Edit::Select(id.into())));
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_create(move |name| {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((w.get_music_layout(), Edit::Create(name.into())));
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_rename(move |name| {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((
                w.get_music_layout(),
                Edit::Rename(selected_library_id(&w), name.into()),
            ));
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_delete(move || {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((w.get_music_layout(), Edit::Delete(selected_library_id(&w))));
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_delete_id(move |id| {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((w.get_music_layout(), Edit::Delete(id.into())));
        }
    });
    let weak = w.as_weak();
    w.on_song_liked(move |id, _revision| {
        weak.upgrade()
            .is_some_and(|w| w.get_liked_ids().iter().any(|liked| liked == id))
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    let like_state = state.clone();
    w.on_toggle_like(move |index| {
        if let Some(w) = weak.upgrade() {
            if !w.get_music_layout() {
                return;
            }
            let video = {
                let st = like_state.lock().unwrap();
                if index < 0 {
                    st.selected.clone()
                } else {
                    st.videos.get(index as usize).cloned()
                }
            };
            if let Some(v) = video {
                match Track::youtube(
                    &v.id,
                    &v.title,
                    &v.channel,
                    (v.duration > 0.).then_some(v.duration),
                ) {
                    Ok(mut t) => {
                        t.extra.insert("music".into(), true.into());
                        if !v.genres.is_empty() {
                            t.extra.insert("genres".into(), serde_json::to_value(&v.genres).unwrap_or_default());
                        }
                        if let Some(views) = v.views {
                            t.extra.insert("view_count".into(), views.into());
                        }
                        let _ = sender.send((true, Edit::ToggleLike(t)));
                    }
                    Err(e) => w.invoke_notify(e.to_string().into()),
                }
            }
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    let st = state.clone();
    w.on_playlist_add(move |index| {
        if let Some(w) = weak.upgrade() {
            let video = {
                let s = st.lock().unwrap();
                if index < 0 {
                    s.selected.clone()
                } else {
                    s.videos.get(index as usize).cloned()
                }
            };
            if let (Some(video), Some(list)) =
                (video, w.get_playlists().row_data(w.get_save_to() as usize))
            {
                match Track::youtube(
                    &video.id,
                    &video.title,
                    &video.channel,
                    (video.duration > 0.).then_some(video.duration),
                ) {
                    Ok(mut track) => {
                        if let Some(views) = video.views {
                            track.extra.insert("view_count".into(), views.into());
                        }
                        if let Some(music) = video.music {
                            track.extra.insert("music".into(), music.into());
                        }
                        let _ =
                            sender.send((w.get_music_layout(), Edit::Add(list.id.into(), track)));
                    }
                    Err(e) => w.invoke_notify(e.to_string().into()),
                }
            } else {
                w.invoke_notify("Choose or create a playlist first.".into());
            }
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    let st = state.clone();
    w.on_playlist_save_queue(move || {
        if let Some(w) = weak.upgrade() {
            if let Some(list) = w.get_playlists().row_data(w.get_save_to() as usize) {
                let tracks = st
                    .lock()
                    .unwrap()
                    .queue
                    .iter()
                    .filter_map(|v| {
                        Track::youtube(
                            &v.id,
                            &v.title,
                            &v.channel,
                            (v.duration > 0.).then_some(v.duration),
                        )
                        .ok()
                    })
                    .collect();
                let _ = sender.send((w.get_music_layout(), Edit::AddMany(list.id.into(), tracks)));
            }
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_remove(move |i| {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((
                w.get_music_layout(),
                Edit::Remove(selected_library_id(&w), i as usize),
            ));
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_move(move |i, delta| {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((
                w.get_music_layout(),
                Edit::Move(selected_library_id(&w), i as usize, delta),
            ));
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_play(move |i| {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((w.get_music_layout(), Edit::Play(i as usize)));
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_enqueue(move |i| {
        if let Some(w) = weak.upgrade() {
            let _ = sender.send((w.get_music_layout(), Edit::Enqueue(i as usize)));
        }
    });
    let (art_tx, art_rx) = mpsc::channel::<(u64, Vec<Video>)>();
    state.lock().unwrap().collection_art_tx = Some(art_tx);
    let art_weak = w.as_weak();
    let art_paths = paths.clone();
    thread::spawn(move || {
        while let Ok(mut job) = art_rx.recv() {
            for newer in art_rx.try_iter() { job = newer; }
            let (n, videos) = job;
            for video in videos {
                if !wait_for_playback(Some((&art_generation, n))) { break; }
                if let Ok((pixels, width, height)) = thumbnail(&art_paths, &video) {
                    let id = video.id;
                    ui_generation(&art_weak, &art_generation, n, move |w| {
                        let model = w.get_videos();
                        if let Some((i, mut row)) = model.iter().enumerate().find(|(_, r)| r.source_id.as_str() == id) {
                            row.thumbnail = slint::Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, width, height));
                            model.set_row_data(i, row);
                        }
                    });
                }
            }
        }
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_enqueue_all(move || {
        if let Some(_w) = weak.upgrade().filter(|w| !w.get_music_layout()) {
            let _ = sender.send((false, Edit::EnqueueAll));
        }
    });

    let sender = tx.clone();
    w.on_playlist_transfer(move |import, scope| {
        let sender = sender.clone();
        let scope = scope.clamp(0, 2);
        thread::spawn(move || {
            let kind = match scope { 0 => "video", 1 => "music", _ => "all" };
            let dialog = rfd::FileDialog::new().set_title(format!("{} {kind} playlists", if import { "Import" } else { "Export" }));
            let path = if import {
                let dialog = dialog.add_filter("Mochune JSON", &["json"]);
                let dialog = if scope == 2 { dialog } else { dialog.add_filter("M3U", &["m3u8", "m3u"]) };
                dialog.pick_file()
            } else {
                dialog.set_file_name(format!("mochune-{kind}-playlists.json")).add_filter("Mochune JSON", &["json"]).save_file()
            };
            if let Some(path) = path {
                let edit = match (import, scope) {
                    (true, 2) => Edit::ImportAll(path), (false, 2) => Edit::ExportAll(path),
                    (true, _) => Edit::Import(path), (false, _) => Edit::Export(path, None),
                };
                let _ = sender.send((scope == 1, edit));
            }
        });
    });
    let weak = w.as_weak();
    let sender = tx.clone();
    w.on_playlist_import(move || {
        if let Some(w) = weak.upgrade() {
            let mode = w.get_music_layout();
            let sender = sender.clone();
            if smoke && let Some(path) = std::env::var_os("MOCHUNE_SMOKE_IMPORT") {
                let _ = sender.send((mode, Edit::Import(PathBuf::from(path))));
                return;
            }
            thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Import playlists into current mode")
                    .add_filter("Playlists", &["json", "m3u8", "m3u"])
                    .pick_file()
                {
                    let _ = sender.send((mode, Edit::Import(path)));
                }
            });
        }
    });
    let weak = w.as_weak();
    w.on_playlist_export(move |all| {
        if let Some(w) = weak.upgrade() {
            let mode = w.get_music_layout();
            let id = if all {
                None
            } else {
                w.get_playlists()
                    .row_data(w.get_export_playlist().max(0) as usize)
                    .map(|p| p.id.to_string())
            };
            if !all && id.is_none() {
                w.invoke_notify("Create or import a playlist first.".into());
                return;
            }
            let sender = tx.clone();
            if smoke && let Some(path) = std::env::var_os("MOCHUNE_SMOKE_EXPORT") {
                let _ = sender.send((mode, Edit::Export(PathBuf::from(path), id)));
                return;
            }
            thread::spawn(move || {
                let dialog = rfd::FileDialog::new()
                    .set_title("Export current-mode playlists")
                    .set_file_name(if all {
                        "mochune-library.json"
                    } else {
                        "mochune-playlist.json"
                    })
                    .add_filter("Mochune JSON", &["json"]);
                let dialog = if all {
                    dialog
                } else {
                    dialog.add_filter("Portable M3U8", &["m3u8"])
                };
                if let Some(path) = dialog.save_file() {
                    let _ = sender.send((mode, Edit::Export(path, id)));
                }
            });
        }
    });
}

fn advance(
    w: &SetupWindow,
    state: &Arc<Mutex<State>>,
    paths: &Paths,
    direction: i32,
    automatic: bool,
) {
    if !w.get_music_layout() {
        let next = {
            let mut st = state.lock().unwrap();
            play_queue::finish(&mut st)
        };
        play_queue::publish(w, state);
        crate::diagnostics::record(
            "video_queue_advance",
            serde_json::json!({
                "automatic":automatic,"fullscreen":w.get_fullscreen_mode(),
                "next":next.as_ref().map(|v|v.id.as_str())
            }),
        );
        if let Some(video) = next {
            play_queue::play(w, state, video);
        }
        return;
    }
    let video = {
        let mut st = state.lock().unwrap();
        if paths.network.offline() {
            st.queue.retain(|v| paths.audio.contains(&v.id));
        }
        st.queue.retain(|v| {
            v.playable(w.get_music_layout())
                || (w.get_music_layout() && paths.audio.contains(&v.id))
        });
        let n = st.queue.len();
        if n == 0 {
            return;
        }
        let target = music_target(
            &mut st,
            direction,
            w.get_shuffle(),
            w.get_repeat(),
            playlists::now_ms() as usize,
        );
        let Some(target) = target else {
            return;
        };
        st.queue_index = target;
        let v = st.queue[target].clone();
        st.selected = Some(v.clone());
        st.format_options.clear();
        st.formats_for = None;
        v
    };
    let _ = automatic;
    // Queue covers are bounded to the one current image, independent of browse models.
    w.set_selected_id(video.id.clone().into());
    w.set_selected_title(video.title.clone().into());
    w.set_selected_channel(video.channel.clone().into());
    w.set_selected_image(Default::default());
    w.set_has_selection(true);
    select(w, &video, -1);
    play_queue::publish(w, state);
    cover(state, video);
    w.invoke_play();
}

fn repeat_cycle(list: bool, song: bool) -> (bool, bool) {
    if song {
        (false, false)
    } else if list {
        (false, true)
    } else {
        (true, false)
    }
}

fn music_target(
    st: &mut State,
    direction: i32,
    shuffle: bool,
    repeat: bool,
    seed: usize,
) -> Option<usize> {
    if st.queue.is_empty() {
        return None;
    }
    if st.music_playlist_pending {
        st.music_playlist_pending = false;
        st.music_shuffle_visited.clear();
        return Some(0);
    }
    let current_id = st.selected.as_ref().map(|v| v.id.clone());
    let current = st
        .queue
        .iter()
        .position(|v| Some(v.id.as_str()) == current_id.as_deref());
    if shuffle {
        if let Some(id) = &current_id
            && !st.music_shuffle_visited.contains(id)
        {
            st.music_shuffle_visited.push(id.clone());
        }
        let ids = st.queue.iter().map(|v| v.id.clone()).collect::<Vec<_>>();
        shuffle_target(
            &ids,
            &mut st.music_shuffle_visited,
            current_id.as_deref(),
            direction,
            repeat,
            seed,
        )
    } else {
        current.map_or(
            Some(if direction > 0 { 0 } else { st.queue.len() - 1 }),
            |index| queue_target(index, st.queue.len(), direction, repeat),
        )
    }
}
fn stage_playlist(st: &mut State, videos: &[Video]) {
    // Browsing changes upcoming metadata only. Keep current playback and selection.
    play_queue::replace(st, videos);
    st.music_playlist_pending = true;
    st.music_shuffle_visited.clear();
}

fn shuffle_target(
    ids: &[String],
    visited: &mut Vec<String>,
    current: Option<&str>,
    direction: i32,
    repeat: bool,
    seed: usize,
) -> Option<usize> {
    visited.retain(|id| ids.contains(id));
    if direction < 0 {
        if visited
            .last()
            .is_some_and(|id| Some(id.as_str()) == current)
        {
            visited.pop();
        }
        return visited
            .last()
            .and_then(|id| ids.iter().position(|v| v == id));
    }
    let mut available: Vec<_> = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| !visited.contains(id))
        .map(|(i, _)| i)
        .collect();
    if available.is_empty() && repeat {
        visited.clear();
        available.extend(
            ids.iter()
                .enumerate()
                .filter(|(_, id)| ids.len() == 1 || Some(id.as_str()) != current)
                .map(|(i, _)| i),
        );
    }
    if available.is_empty() {
        return None;
    }
    Some(available[seed % available.len()])
}

fn queue_target(current: usize, len: usize, direction: i32, repeat: bool) -> Option<usize> {
    if len == 0 || current >= len {
        return None;
    }
    if direction < 0 {
        if current > 0 {
            Some(current - 1)
        } else if repeat {
            Some(len - 1)
        } else {
            None
        }
    } else if current + 1 < len {
        Some(current + 1)
    } else if repeat {
        Some(0)
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::{liked_seeds, queue_target};
    use crate::playlists::{Library, Track};
    #[test]
    fn repeat_cycles_off_list_song_off() {
        assert_eq!(super::repeat_cycle(false, false), (true, false));
        assert_eq!(super::repeat_cycle(true, false), (false, true));
        assert_eq!(super::repeat_cycle(false, true), (false, false));
    }
    #[test]
    fn search_song_is_first_in_rebuilt_list_once() {
        let current = super::Video {
            id: "playing".into(),
            ..Default::default()
        };
        let other = super::Video {
            id: "other".into(),
            ..Default::default()
        };
        let rows = super::discovery_rows(Some(&current), vec![other, current.clone()], 40);
        assert_eq!(
            rows.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            vec!["playing", "other"]
        );
    }
    #[test]
    fn liked_seeds_sample_whole_library_with_bounded_rotating_requests() {
        let mut lib = Library::default();
        for i in 0..14 {
            lib.add(
                "liked-songs",
                Track::youtube(
                    &format!("song{i:07}"),
                    &format!("Song {i}"),
                    "Artist",
                    Some(120.),
                )
                .unwrap(),
            )
            .unwrap();
        }
        let seeds = liked_seeds(&lib, 0);
        assert_eq!(seeds.len(), 10);
        assert_eq!(seeds[0].title, "Song 13");
        assert!(seeds.iter().any(|v| v.title == "Song 1"));
        let all = (0..14).flat_map(|cursor| liked_seeds(&lib, cursor)).map(|v| v.id).collect::<std::collections::HashSet<_>>();
        assert_eq!(all.len(), 14);
        assert!(seeds.iter().all(|v| v.music == Some(true)));
        assert!(liked_seeds(&Library::empty(), 0).is_empty());
        let mut small = Library::default();
        small.add("liked-songs", Track::youtube("abcdefghijk", "One song", "Artist", Some(120.)).unwrap()).unwrap();
        assert_eq!(liked_seeds(&small, usize::MAX).len(), 1);
    }
    #[test]
    fn search_pick_drops_other_results_before_discovery_and_keeps_current_for_next() {
        use super::{State, Video};
        let song = |id: &str| Video { id: id.into(), ..Default::default() };
        let picked = song("picked");
        let mut st = State {
            queue: vec![song("search-first"), picked.clone(), song("search-last")],
            queue_index: 1,
            ..Default::default()
        };
        super::begin_search_discovery(&mut st, &picked);
        st.selected = Some(picked.clone());
        assert_eq!(st.queue.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), ["picked"]);
        assert_eq!(super::music_target(&mut st, 1, false, false, 0), None);
        let rows = super::discovery_rows(Some(&picked), vec![song("recommended"), picked.clone()], 40);
        st.queue = super::discovery_queue(st.selected.as_ref(), &rows);
        assert_eq!(super::music_target(&mut st, 1, false, false, 0), Some(1));
        assert_eq!(st.queue[1].id, "recommended");
        assert_eq!(st.music_search_anchor.as_deref(), Some("picked"));
        assert!(!st.queue_is_playlist);
    }
    #[test]
    fn search_discovery_continues_with_current_then_unique_playable_songs() {
        use super::Video;
        let a = Video {
            id: "a".into(),
            ..Default::default()
        };
        let b = Video {
            id: "b".into(),
            ..Default::default()
        };
        let bad = Video {
            id: "c".into(),
            unavailable: "Unavailable".into(),
            ..Default::default()
        };
        let queue = super::discovery_queue(Some(&a), &[a.clone(), b.clone(), b, bad]);
        assert_eq!(
            queue.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(queue_target(0, queue.len(), 1, false), Some(1));
        assert_eq!(queue_target(1, queue.len(), 1, false), None);
    }
    #[test]
    fn video_and_music_libraries_remain_separate_and_exports_keep_their_kind() {
        use crate::playlists::{Library, Store, Track};
        let path = std::env::temp_dir().join(format!(
            "duck-library-isolation-{}-{}",
            std::process::id(),
            crate::playlists::now_ms()
        ));
        let video = Store {
            path: path.join("video-library.json"),
        };
        let music = Store {
            path: path.join("library.json"),
        };
        let mut vl = video.load_or(Library::empty).unwrap();
        assert!(vl.playlists.is_empty());
        let mut ml = music.load().unwrap();
        let mid = ml.playlists[0].id.clone();
        ml.extra.insert("media_kind".into(), "music".into());
        ml.add(
            &mid,
            Track::youtube("abcdefghijk", "Music track", "Artist", Some(30.)).unwrap(),
        )
        .unwrap();
        music.save(&ml).unwrap();
        let vid = vl.create("Video collection").unwrap();
        vl.extra.insert("media_kind".into(), "video".into());
        video.save(&vl).unwrap();
        assert_eq!(
            video
                .load_or(Library::empty)
                .unwrap()
                .get(&vid)
                .unwrap()
                .tracks
                .len(),
            0
        );
        assert_eq!(music.load().unwrap().get(&mid).unwrap().tracks.len(), 1);
        assert!(super::check_import(&ml, false, false).is_err());
        assert!(super::check_import(&vl, true, false).is_err());
        assert!(super::check_import(&vl.selected(&vid).unwrap(), false, false).is_ok());
        assert!(super::check_import(&Library::default(), false, false).is_err());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn queue_edges_repeat_and_shuffle() {
        assert_eq!(queue_target(0, 0, 1, true), None);
        assert_eq!(queue_target(0, 2, -1, false), None);
        assert_eq!(queue_target(0, 2, 1, false), Some(1));
        assert_eq!(queue_target(1, 2, 1, false), None);
        assert_eq!(queue_target(1, 2, 1, true), Some(0));
        assert_eq!(queue_target(0, 2, -1, true), Some(1));
        assert_eq!(queue_target(0, 1, 1, false), None);
        assert_eq!(queue_target(0, 1, 1, true), Some(0));
    }
    #[test]
    fn playlist_browsing_preserves_playing_song_then_starts_at_first() {
        let video = |id: &str| super::Video {
            id: id.into(),
            ..Default::default()
        };
        let current = video("bbbbbbbbbbb");
        let mut st = super::State {
            selected: Some(current),
            ..Default::default()
        };
        super::stage_playlist(&mut st, &[video("aaaaaaaaaaa"), video("bbbbbbbbbbb")]);
        assert_eq!(st.selected.as_ref().unwrap().id, "bbbbbbbbbbb");
        // The playing song is also in the selected playlist; still start from its beginning.
        assert_eq!(super::music_target(&mut st, 1, false, false, 0), Some(0));
        assert!(!st.music_playlist_pending);
    }
    #[test]
    fn shuffle_visits_each_song_once_and_only_loops_when_repeat_is_on() {
        for seed in 0..12 {
            let mut st = super::State {
                queue: (0..7)
                    .map(|i| super::Video {
                        id: i.to_string(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            st.selected = Some(st.queue[0].clone());
            let mut seen = std::collections::HashSet::from(["0".to_owned()]);
            for _ in 1..7 {
                let i = super::music_target(&mut st, 1, true, false, seed).unwrap();
                assert!(seen.insert(st.queue[i].id.clone()));
                st.selected = Some(st.queue[i].clone());
            }
            assert_eq!(super::music_target(&mut st, 1, true, false, seed), None);
            let i = super::music_target(&mut st, 1, true, true, seed).unwrap();
            assert_ne!(st.queue[i].id, st.selected.as_ref().unwrap().id);
        }
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;
    fn libraries() -> [Library; 2] {
        std::array::from_fn(|i| {
            let mut lib = Library::empty();
            lib.extra.insert("media_kind".into(), if i == 0 { "video" } else { "music" }.into());
            if i == 1 { lib.ensure_liked_songs(); }
            let id = lib.create(if i == 0 { "Videos" } else { "Music" }).unwrap();
            lib.add(&id, Track::youtube(if i == 0 { "abcdefghijk" } else { "01234567890" }, "Title", "Channel", Some(100.)).unwrap()).unwrap();
            lib
        })
    }
    #[test]
    fn combined_backup_round_trips_without_mixing_modes_and_merges_idempotently() {
        let original = libraries();
        let bytes = PlaylistBackup::encode(&original).unwrap();
        let decoded = PlaylistBackup::decode(&bytes).unwrap();
        assert_eq!(original, decoded);
        for (mut existing, incoming) in original.into_iter().zip(decoded) {
            assert_eq!(existing.merge(incoming).unwrap(), 0);
        }
    }
    #[test]
    fn previous_brand_backup_imports_both_libraries() {
        let bytes = PlaylistBackup::encode(&libraries()).unwrap();
        let mut old: Value = serde_json::from_slice(&bytes).unwrap();
        old["schema"] = "io.ducktube.playlist-backup".into();
        old["music"]["schema"] = "io.ducktube.playlists".into();
        old["video"]["schema"] = "io.ducktube.playlists".into();
        let decoded = PlaylistBackup::decode(&serde_json::to_vec(&old).unwrap()).unwrap();
        for library in decoded { assert_eq!(library.schema, playlists::SCHEMA); }
    }
    #[test]
    fn combined_backup_rejects_swapped_modes_versions_and_invalid_tracks() {
        let bytes = PlaylistBackup::encode(&libraries()).unwrap();
        let original: Value = serde_json::from_slice(&bytes).unwrap();
        for case in 0..4 {
            let mut bad = original.clone();
            match case {
                0 => bad["video"]["media_kind"] = "music".into(),
                1 => bad["version"] = 99.into(),
                2 => bad["music"]["version"] = 99.into(),
                _ => bad["music"]["playlists"][1]["tracks"][0]["source"]["id"] = "bad".into(),
            }
            assert!(PlaylistBackup::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        assert!(PlaylistBackup::decode(&libraries()[0].to_json().unwrap()).is_err());
    }
    #[test]
    fn combined_import_rolls_back_video_if_music_cannot_be_saved() {
        let root = PathBuf::from(std::env::var_os("RUST_EXTERNAL_ROOT").expect("guarded external toolchain")).join("tmp").join(format!("transfer-test-{}-{}", std::process::id(), playlists::now_ms()));
        fs::create_dir_all(&root).unwrap();
        let stores = [Store { path: root.join("video.json") }, Store { path: root.join("blocked/music.json") }];
        let mut existing = libraries();
        let original = existing.clone();
        stores[0].save(&existing[0]).unwrap();
        fs::write(root.join("blocked"), b"not a directory").unwrap();
        let backup = root.join("backup.json");
        fs::write(&backup, PlaylistBackup::encode(&libraries()).unwrap()).unwrap();
        assert!(transfer_all(&Edit::ImportAll(backup), &mut existing, &stores, &[None,None]).is_err());
        assert_eq!(existing, original);
        assert_eq!(stores[0].load().unwrap(), original[0]);
        fs::remove_dir_all(root).unwrap();
    }
}
