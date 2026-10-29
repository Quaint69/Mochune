//! Isolated provider browsing and playback. One cancellable worker, one pending job.
//! YouTube history, recommendations, playlists and stream caches never receive these IDs.
use super::*;
use crate::{SourceRow, Sources};
use ani_lib::{AnikotoCzClient, StreamLink, TranslationType};
use anyhow::ensure;
use std::sync::{Condvar, atomic::AtomicBool};
mod folders;
mod library;
mod library_data;
mod providers;
mod reader;
mod reader_cache;
mod relay;
use library::{library_rows, remember};
use providers::{Client, Entry, Pages, Streams};

pub(super) struct Playback {
    pub video: Video,
    pub info: Value,
    entry: Entry,
    streams: Vec<StreamLink>,
    choice: usize,
    created: Instant,
    // Stop accepting HTTP first; the owning runtime then cancels remaining streams.
    relay: Option<relay::Relay>,
    runtime: Option<tokio::runtime::Runtime>,
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.relay.take();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
#[derive(Default)]
struct Browser {
    library: library::Library,
    reader: reader::Reader,
    resume: Option<library::SavedTitle>,
    folder: String,
    results: Vec<Entry>,
    children: Vec<Entry>,
    parent: Option<Entry>,
    current: Option<usize>,
    pages: Vec<String>,
    page: usize,
    referer: Option<String>,
    offset: usize,
    more: bool,
    saved_queue: Vec<Video>,
    saved_index: usize,
    saved_playlist: bool,
    saved_current: bool,
    saved_query: String,
    saved_position: f64,
    saved_duration: f32,
    saved_quality: i32,
    saved_choice: i32,
    saved_music_only: bool,
    saved_formats: Vec<FormatOption>,
    saved_formats_for: Option<String>,
    saved_details: Option<details::Details>,
    last: Option<Task>,
}
#[derive(Clone, PartialEq)]
enum Task {
    Search(String),
    Children(Entry, usize),
    Play(Entry),
    Quality(Entry, Vec<StreamLink>, usize, Instant),
    Read(Entry),
    Page(String, Option<String>, usize, Option<Arc<Vec<u8>>>, bool),
}
enum Reply {
    Results(Vec<Entry>),
    Children(Entry, Vec<Entry>, usize, bool),
    Playback(Playback),
    Pages(Pages),
    Page(Arc<Vec<u8>>, Option<(Vec<u8>, u32, u32)>, f32, usize),
}
struct Work {
    epoch: u64,
    mode: i32,
    provider: i32,
    dubbed: bool,
    task: Task,
}
#[derive(Default)]
struct Jobs {
    epoch: AtomicU64,
    inflight: AtomicU64,
    stop: AtomicBool,
    pending: Mutex<Option<Work>>,
    save: Mutex<Option<Vec<u8>>>,
    wake: Condvar,
}
pub(super) struct Guard {
    jobs: Arc<Jobs>,
    worker: Option<thread::JoinHandle<()>>,
    state: Arc<Mutex<State>>,
    _automation: Option<slint::Timer>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.jobs.stop.store(true, Ordering::Release);
        self.jobs.epoch.fetch_add(1, Ordering::AcqRel);
        self.jobs.pending.lock().unwrap().take();
        self.jobs.wake.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.state.lock().unwrap().anime_playback = None;
    }
}
fn public_error(error: &anyhow::Error) -> String {
    let text = error.to_string();
    let end = text
        .find("http://")
        .or_else(|| text.find("https://"))
        .unwrap_or(text.len());
    text[..end].trim().chars().take(400).collect()
}
fn language(dubbed: bool) -> TranslationType {
    if dubbed {
        TranslationType::Dub
    } else {
        TranslationType::Sub
    }
}
fn sources(w: &SetupWindow) -> Sources<'_> {
    w.global::<Sources>()
}
fn rows(w: &SetupWindow, b: &Browser) {
    let g = sources(w);
    g.set_nested(b.parent.is_some());
    g.set_more(b.more);
    let entries = if b.parent.is_some() {
        &b.children
    } else {
        &b.results
    };
    let values = entries
        .iter()
        .enumerate()
        .map(|(i, e)| SourceRow {
            title: e.title.clone().into(),
            detail: e.detail.clone().into(),
            active: (b.parent.is_some() || g.get_mode() == 2) && b.current == Some(i),
        })
        .collect::<Vec<_>>();
    if let Some(model) = g.get_rows().as_any().downcast_ref::<VecModel<SourceRow>>() {
        model.set_vec(values);
    } else {
        g.set_rows(ModelRc::new(VecModel::from(values)));
    }
    let active = b
        .parent
        .as_ref()
        .or_else(|| b.current.and_then(|i| b.results.get(i)));
    g.set_can_bookmark(active.is_some());
    g.set_bookmarked(active.is_some_and(|e| {
        b.library.titles.iter().any(|s| {
            s.mode == g.get_mode()
                && s.provider == g.get_provider()
                && s.entry.id == e.id
                && s.bookmarked
        })
    }));
    library_rows(w, b);
    g.set_can_previous(b.current.is_some_and(|i| i > 0));
    g.set_can_next(b.current.is_some_and(|i| i + 1 < b.children.len()));
}
fn submit(w: &SetupWindow, jobs: &Jobs, browser: &Mutex<Browser>, task: Task) {
    let g = sources(w);
    if g.get_mode() == 0 {
        return;
    }
    if w.get_offline() {
        g.set_status("This source needs an Internet connection.".into());
        return;
    }
    if jobs.inflight.load(Ordering::Acquire) == jobs.epoch.load(Ordering::Acquire)
        && jobs.inflight.load(Ordering::Acquire) != 0
    {
        let data = browser.lock().unwrap();
        let same = match (data.last.as_ref(), &task) {
            (Some(Task::Page(u, _, i, _, _)), Task::Page(v, _, n, _, _)) => u == v && i == n,
            (Some(old), new) => old == new,
            _ => false,
        };
        if same {
            if matches!(&task,Task::Page(_,_,i,_,_) if *i==g.get_page() as usize) {
                g.set_busy(true);
            }
            return;
        }
    }
    let epoch = jobs.epoch.fetch_add(1, Ordering::AcqRel) + 1;
    jobs.inflight.store(epoch, Ordering::Release);
    browser.lock().unwrap().last = Some(task.clone());
    *jobs.pending.lock().unwrap() = Some(Work {
        epoch,
        mode: g.get_mode(),
        provider: g.get_provider(),
        dubbed: g.get_dubbed(),
        task,
    });
    jobs.wake.notify_one();
    let background = matches!(browser.lock().unwrap().last,Some(Task::Page(_,_,i,_,_)) if i!=g.get_page() as usize);
    g.set_busy(!background);
    if !background {
        g.set_status("Loading…".into());
    }
}
fn stream_info(
    video: &Video,
    entry: &Entry,
    local: &StreamLink,
    streams: &[StreamLink],
    choice: usize,
) -> Value {
    let mut headers = serde_json::Map::new();
    if let Some(v) = &local.headers.referer {
        headers.insert("Referer".into(), v.clone().into());
    }
    if let Some(v) = &local.headers.origin {
        headers.insert("Origin".into(), v.clone().into());
    }
    for (k, v) in &local.headers.extra {
        headers.insert(k.clone(), v.clone().into());
    }
    let mut subtitles = serde_json::Map::new();
    for (i, t) in local.subtitles.iter().take(32).enumerate() {
        subtitles.insert(
            format!("source-{i}"),
            serde_json::json!([{"url":t.url,"ext":"vtt","name":t.label}]),
        );
    }
    serde_json::json!({"id":video.id,"title":video.title,"uploader":video.channel,"description":entry.description,"url":local.url,"http_headers":headers,"subtitles":subtitles,"vcodec":"unknown","acodec":"unknown","external_source":true,"protocol":if local.hls{"m3u8_native"}else{"https"},"source_qualities":streams.iter().map(|s|s.resolution.clone()).collect::<Vec<_>>(),"source_choice":choice})
}
fn preferred(streams: &[StreamLink]) -> usize {
    streams
        .iter()
        .position(|s| s.resolution.starts_with("720"))
        .or_else(|| {
            streams
                .iter()
                .position(|s| s.resolution.starts_with("1080"))
        })
        .unwrap_or(0)
}
fn play(
    runtime: tokio::runtime::Runtime,
    client: Client,
    entry: Entry,
    streams: Vec<StreamLink>,
    choice: usize,
    mode: i32,
    provider: i32,
) -> Result<Reply> {
    let mut selected = streams
        .get(choice)
        .context("This quality is unavailable")?
        .clone();
    if !selected.hls && selected.provider == "Dramachi" {
        selected.url = runtime.block_on(providers::indexed_movie_url(&client, &selected.url))?;
    }
    let (relay, local) = if selected.hls {
        let (relay, local) = runtime.block_on(relay::start(client, &selected))?;
        (Some(relay), local)
    } else {
        (None, selected)
    };
    let video = Video {
        id: format!("external:{mode}:{provider}:{}", entry.id),
        title: entry.title.clone(),
        channel: local.provider.clone(),
        music: Some(false),
        ..Video::default()
    };
    let mut info = stream_info(&video, &entry, &local, &streams, choice);
    if mode == 2 && provider == 0 {
        let index = url::Url::parse(&local.url)
            .ok()
            .and_then(|u| u.fragment()?.split_once(";index=")?.1.parse::<u64>().ok())
            .unwrap_or(0);
        info["source_index_bytes"] = index.into();
    }
    let runtime = if relay.is_some() {
        Some(runtime)
    } else {
        runtime.shutdown_background();
        None
    };
    Ok(Reply::Playback(Playback {
        video,
        info,
        entry,
        streams,
        choice,
        created: Instant::now(),
        relay,
        runtime,
    }))
}
fn perform(work: &Work, jobs: &Jobs) -> Result<Reply> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let client = Client::new()?;
    let result=runtime.block_on(async {
  let action=async {
   match &work.task {
    Task::Search(query) if work.mode==1=>{let c=AnikotoCzClient::builder().timeout(Duration::from_secs(12)).build()?;let shows=c.search(query,language(work.dubbed)).await?;Ok(Reply::Results(shows.into_iter().take(40).map(|s|Entry{id:s.id,title:s.name,detail:format!("{} episodes · Anikoto.cz",s.episodes),..Default::default()}).collect()))},
    Task::Search(query) if work.mode==2=>Ok(Reply::Results(providers::movie_search(&client,query,work.provider).await?)),
    Task::Search(query)=>Ok(Reply::Results(providers::manga_search(&client,query,work.provider).await?)),
    Task::Children(e,offset) if work.mode==1=>{let c=AnikotoCzClient::builder().timeout(Duration::from_secs(12)).build()?;let episodes=c.episodes(&e.id,language(work.dubbed)).await?;Ok(Reply::Children(e.clone(),episodes.into_iter().take(2000).map(|n|Entry{id:n.clone(),title:format!("Episode {n}"),detail:if work.dubbed{"Dub"}else{"Sub"}.into(),description:e.description.clone()}).collect(),*offset,false))},
    Task::Children(e,offset)=>{let (chapters,more)=providers::chapters(&client,e,work.provider,*offset).await?;Ok(Reply::Children(e.clone(),chapters,*offset,more))},
    Task::Read(e)=>Ok(Reply::Pages(providers::pages(&client,e.clone(),work.provider).await?)),
    Task::Page(url,referer,index,cached,decode)=>{let bytes=if let Some(bytes)=cached{bytes.clone()}else{let headers=referer.as_ref().map(|v|vec![("Referer".into(),v.clone())]).unwrap_or_default();Arc::new(client.bytes(url,&headers,12*1024*1024).await?)};
      let make_reader=||->Result<_>{let mut r=image::ImageReader::new(std::io::Cursor::new(bytes.as_slice())).with_guessed_format()?;let mut limits=image::Limits::default();limits.max_alloc=Some(40*1024*1024);limits.max_image_width=Some(10000);limits.max_image_height=Some(20000);r.limits(limits);Ok(r)};
      let (w,h)=make_reader()?.into_dimensions()?;ensure!(w>0&&h>0,"Invalid manga image dimensions");
      let image=if *decode{let image=make_reader()?.decode()?.thumbnail(1000,2200).to_rgb8();let (w,h)=image.dimensions();Some((image.into_raw(),w,h))}else{None};
      Ok(Reply::Page(bytes,image,h as f32/w as f32,*index))},
    Task::Play(_) | Task::Quality(_,_,_,_)=>bail!("internal playback dispatch"),
   }
  };
  tokio::select!{result=action=>result,_=tokio::time::sleep(Duration::from_secs(45))=>bail!("Source timed out. Retry or select the other available provider."),_=async{while !jobs.stop.load(Ordering::Acquire)&&jobs.epoch.load(Ordering::Acquire)==work.epoch{tokio::time::sleep(Duration::from_millis(60)).await;}}=>bail!("Request cancelled")}
 });
    result
}
fn resolve(work: &Work, jobs: &Jobs, browser: &Mutex<Browser>) -> Result<Reply> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let client = Client::new()?;
    let result:Result<Streams>=runtime.block_on(async {
 let action=async {match &work.task{
 Task::Quality(e,streams,_,created) if created.elapsed()<Duration::from_secs(if work.mode==1 {60}else{300})=>Ok(Streams{entry:e.clone(),streams:streams.clone()}),
 Task::Quality(e,_,_,_) if work.mode==1=>{let (id,episode)=e.id.rsplit_once(':').context("Episode identity missing; select the episode again")?;let c=AnikotoCzClient::builder().timeout(Duration::from_secs(12)).build()?;let streams=c.streams(id,episode,language(work.dubbed)).await?;Ok(Streams{entry:e.clone(),streams})},
 Task::Quality(e,_,_,_)=>providers::movie_streams(&client,e.clone(),work.provider).await,
 Task::Play(e) if work.mode==1=>{let parent=browser.lock().unwrap().parent.clone().context("Choose a series first")?;let c=AnikotoCzClient::builder().timeout(Duration::from_secs(12)).build()?;let streams=c.streams(&parent.id,&e.id,language(work.dubbed)).await?;Ok(Streams{entry:Entry{id:format!("{}:{}",parent.id,e.id),title:format!("{} · {}",parent.title,e.title),description:parent.description,..Default::default()},streams})},
 Task::Play(e)=>providers::movie_streams(&client,e.clone(),work.provider).await,
 _=>bail!("Invalid playback request")}};
 tokio::select!{result=action=>result,_=tokio::time::sleep(Duration::from_secs(45))=>bail!("Stream resolution timed out. Retry or choose another provider."),_=async{while !jobs.stop.load(Ordering::Acquire)&&jobs.epoch.load(Ordering::Acquire)==work.epoch{tokio::time::sleep(Duration::from_millis(60)).await;}}=>bail!("Request cancelled")}
 });
    let mut streams = result?;
    if work.mode == 1 {
        runtime.block_on(providers::discover_hls_options(
            &client,
            &mut streams.streams,
        ))?;
    }
    streams.streams.truncate(32);
    ensure!(
        !streams.streams.is_empty(),
        "No playable source returned for this title"
    );
    let choice = match work.task {
        Task::Quality(_, ref old, i, _) => {
            let label = &old.get(i).context("Selected quality missing")?.resolution;
            streams
                .streams
                .iter()
                .position(|s| &s.resolution == label)
                .context(
                    "Selected quality is no longer available. Choose another quality explicitly.",
                )?
        }
        _ => preferred(&streams.streams),
    };
    let mut reply = play(
        runtime,
        client,
        streams.entry,
        streams.streams,
        choice,
        work.mode,
        work.provider,
    )?;
    if let (Task::Quality(_, _, _, created), Reply::Playback(p)) = (&work.task, &mut reply) {
        if created.elapsed() < Duration::from_secs(if work.mode == 1 { 60 } else { 300 }) {
            p.created = *created;
        }
    }
    Ok(reply)
}
fn reset_source_playback(
    w: &SetupWindow,
    state: &Mutex<State>,
    tx: &mpsc::Sender<PlayerCommand>,
    generation: &AtomicU64,
) {
    generation.fetch_add(1, Ordering::AcqRel);
    PLAYBACK_PENDING.store(0, Ordering::Release);
    let _ = tx.send(PlayerCommand::Reset);
    state.lock().unwrap().anime_playback = None;
    w.set_has_selection(false);
    w.set_resolving(false);
    w.set_playback_active(false);
    w.set_video_visible(false);
    w.set_video_frame(Default::default());
    w.set_position(0.);
    w.set_duration(0.);
    w.set_paused(true);
    w.set_player_error("".into());
    w.invoke_reset_captions();
    crate::timeline::clear(w);
}
fn start(
    w: &SetupWindow,
    state: &Arc<Mutex<State>>,
    tx: &mpsc::Sender<PlayerCommand>,
    generation: &AtomicU64,
    resume: bool,
) {
    let Some(p) = state.lock().unwrap().anime_playback.clone() else {
        return;
    };
    let n = generation.fetch_add(1, Ordering::AcqRel) + 1;
    PLAYBACK_PENDING.store(n, Ordering::Release);
    let mut s = settings(w);
    s.music = false;
    s.remember = false;
    s.auto_related = false;
    s.playback_request = true;
    s.resume_position = if resume { w.get_position() as f64 } else { 0. };
    w.global::<crate::BrowsePages>().invoke_close_page();
    w.set_has_selection(true);
    w.set_selection_playable(true);
    w.set_music_only(false);
    w.set_selected_id(p.video.id.clone().into());
    w.set_selected_title(p.video.title.clone().into());
    w.set_selected_channel(p.video.channel.clone().into());
    w.set_selected_channel_url("".into());
    w.set_selected_image(Default::default());
    w.set_subscriber_text("".into());
    w.set_video_stats("".into());
    w.set_rating_text("".into());
    if !resume {
        w.invoke_reset_captions();
    }
    w.set_player_error("".into());
    w.set_playback_active(false);
    w.set_resolving(true);
    w.set_recommendations_pending(false);
    w.set_related_after_frame(false);
    w.set_playback_status("Preparing source stream…".into());
    if !resume {
        w.set_position(0.);
        w.set_duration(0.);
        w.set_paused(false);
    }
    if !resume {
        let _ = tx.send(PlayerCommand::Reset);
    }
    let _ = tx.send(PlayerCommand::Play(p.video.clone(), s, n));
}
pub(super) fn install(
    w: &SetupWindow,
    state: Arc<Mutex<State>>,
    tx: mpsc::Sender<PlayerCommand>,
    generation: Arc<AtomicU64>,
    browse: Arc<AtomicU64>,
    smoke: bool,
) -> Guard {
    let jobs = Arc::new(Jobs::default());
    let browser = Arc::new(Mutex::new(Browser {
        library: library::Library::load(),
        folder: "*".into(),
        ..Default::default()
    }));
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    let st = state.clone();
    let send = tx.clone();
    let gen_id = generation.clone();
    let worker = thread::spawn(move || {
        loop {
            let work = {
                let mut pending = j.pending.lock().unwrap();
                while pending.is_none()
                    && j.save.lock().unwrap().is_none()
                    && !j.stop.load(Ordering::Acquire)
                {
                    pending = j.wake.wait(pending).unwrap();
                }
                let save = j.save.lock().unwrap().take();
                if let Some(bytes) = save {
                    drop(pending);
                    if let Err(error) = crate::playlists::write_atomic(
                        &crate::identity::data_directory().join("sources-library.json"),
                        &bytes,
                        true,
                    ) {
                        crate::diagnostics::record(
                            "source_library_save_failed",
                            serde_json::json!({"error":public_error(&error)}),
                        );
                    }
                    continue;
                }
                if j.stop.load(Ordering::Acquire) {
                    break;
                }
                pending.take().unwrap()
            };
            let began = Instant::now();
            let result = if matches!(work.task, Task::Play(_) | Task::Quality(_, _, _, _)) {
                resolve(&work, &j, &b)
            } else {
                perform(&work, &j)
            };
            crate::diagnostics::record(
                "source_request",
                serde_json::json!({"mode":work.mode,"provider":work.provider,"ok":result.is_ok(),"elapsed_ms":began.elapsed().as_millis()}),
            );
            let j = j.clone();
            let b = b.clone();
            let st = st.clone();
            let send = send.clone();
            let gen_id = gen_id.clone();
            ui(&weak, move |w| {
                if work.epoch != j.epoch.load(Ordering::Acquire)
                    || sources(&w).get_mode() != work.mode
                {
                    return;
                }
                j.inflight.store(0, Ordering::Release);
                let g = sources(&w);
                g.set_busy(false);
                match result {
                    Err(error) => {
                        if let Task::Page(_, _, index, _, _) = work.task {
                            let mut data = b.lock().unwrap();
                            data.reader.failed.insert(index);
                            let background = index != data.page;
                            drop(data);
                            if background {
                                reader::schedule(&w, &j, &b);
                                return;
                            }
                        }
                        g.set_status(
                            format!("Source failed: {}", public_error(&error))
                                .chars()
                                .take(600)
                                .collect::<String>()
                                .into(),
                        );
                    }
                    Ok(Reply::Results(entries)) => {
                        let mut b = b.lock().unwrap();
                        b.results = entries;
                        b.children.clear();
                        b.parent = None;
                        b.current = None;
                        b.more = false;
                        rows(&w, &b);
                        g.set_title("Search results".into());
                        g.set_status(
                            if b.results.is_empty() {
                                "No matching titles. Try another query or provider."
                            } else {
                                "Choose a title."
                            }
                            .into(),
                        );
                    }
                    Ok(Reply::Children(e, entries, offset, more)) => {
                        let mut b = b.lock().unwrap();
                        if offset == 0 {
                            b.children = entries;
                            b.current = None;
                        } else {
                            let mut ids = b
                                .children
                                .iter()
                                .map(|e| e.id.clone())
                                .collect::<std::collections::HashSet<_>>();
                            for e in entries {
                                if b.children.len() == 2000 {
                                    break;
                                }
                                if ids.insert(e.id.clone()) {
                                    b.children.push(e);
                                }
                            }
                        }
                        b.offset = offset + 100;
                        b.more = more && b.children.len() < 2000;
                        b.parent = Some(e.clone());
                        if work.mode == 3 && b.resume.is_none() {
                            b.resume = b
                                .library
                                .titles
                                .iter()
                                .find(|s| {
                                    s.mode == 3
                                        && s.provider == work.provider
                                        && s.entry.id == e.id
                                        && s.chapter.is_some()
                                })
                                .cloned();
                        }
                        let resume = b.resume.clone().filter(|r| r.entry.id == e.id);
                        if let Some(chapter) = resume.as_ref().and_then(|s| s.chapter.clone()) {
                            if !b.children.iter().any(|c| c.id == chapter.id) {
                                b.children.push(chapter);
                            }
                        }

                        if let Some(saved) = resume {
                            if let Some(chapter) = saved.chapter {
                                if let Some(index) =
                                    b.children.iter().position(|c| c.id == chapter.id)
                                {
                                    let weak = w.as_weak();
                                    slint::Timer::single_shot(Duration::ZERO, move || {
                                        if let Some(w) = weak.upgrade() {
                                            sources(&w).invoke_pick(index as i32);
                                        }
                                    });
                                }
                            }
                        }

                        rows(&w, &b);
                        g.set_title(e.title.into());
                        g.set_status(
                            format!(
                                "{} {}",
                                b.children.len(),
                                if work.mode == 1 {
                                    "episodes"
                                } else {
                                    "chapters"
                                }
                            )
                            .into(),
                        );
                    }
                    Ok(Reply::Playback(p)) => {
                        if work.mode == 2 {
                            let mut data = b.lock().unwrap();
                            if let Some(row) = data.results.iter_mut().find(|e| e.id == p.entry.id)
                            {
                                row.title = p.video.title.clone();
                                row.detail = p.video.channel.clone();
                            }
                        }
                        let resume = matches!(work.task, Task::Quality(_, _, _, _));
                        let description = format!(
                            "Playing · {} {} · {}",
                            p.streams.len(),
                            if p.streams.len() == 1 {
                                "quality supplied"
                            } else {
                                "quality options"
                            },
                            if p.streams[p.choice].subtitles.is_empty() {
                                "No captions supplied"
                            } else {
                                "CC available"
                            }
                        );
                        st.lock().unwrap().anime_playback = Some(Arc::new(p));
                        g.set_status(description.into());
                        start(&w, &st, &send, &gen_id, resume);
                        rows(&w, &b.lock().unwrap());
                    }
                    Ok(Reply::Pages(pages)) => {
                        let mut data = b.lock().unwrap();
                        data.reader.clear();
                        data.pages = pages.urls;
                        data.reader.ratios = vec![1.45; data.pages.len()];
                        data.page = data.page.min(data.pages.len().saturating_sub(1));
                        data.referer = pages.referer;
                        g.set_reader_title(pages.entry.title.into());
                        g.set_page_count(data.pages.len() as i32);
                        g.set_page(data.page as i32);
                        g.set_page_image(Default::default());
                        rows(&w, &data);
                        reader::publish(&w, &mut data);
                        g.set_jump_serial(g.get_jump_serial().wrapping_add(1));
                        drop(data);
                        reader::schedule(&w, &j, &b);
                    }
                    Ok(Reply::Page(bytes, image, ratio, index)) => {
                        let mut data = b.lock().unwrap();
                        let current = data.page;
                        data.reader.insert(index, bytes, current);
                        let preceding_resize = index < data.page
                            && data
                                .reader
                                .ratios
                                .get(index)
                                .is_some_and(|r| (r - ratio).abs() > 0.001);
                        if let Some(r) = data.reader.ratios.get_mut(index) {
                            *r = ratio;
                        }
                        if let Some((bytes, width, height)) = image {
                            let buffer =
                                slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
                                    &bytes, width, height,
                                );
                            let model = g.get_reader_pages();
                            if let Some(model) =
                                model.as_any().downcast_ref::<VecModel<crate::ReaderPage>>()
                            {
                                if let Some(mut row) = model.row_data(index) {
                                    row.image = slint::Image::from_rgb8(buffer);
                                    model.set_row_data(index, row);
                                }
                            }
                            data.reader.decoded.insert(index, ());
                        }
                        reader::publish(&w, &mut data);
                        if preceding_resize {
                            g.set_jump_serial(g.get_jump_serial().wrapping_add(1));
                        }
                        if index == data.page {
                            remember(&w, &mut data, &j, false);
                        }
                        g.set_status(
                            format!(
                                "Page {} / {} · {} prefetched",
                                data.page + 1,
                                data.pages.len(),
                                data.reader.compressed.len()
                            )
                            .into(),
                        );
                        drop(data);
                        reader::schedule(&w, &j, &b);
                    }
                }
                if smoke
                    && (std::env::var_os("MOCHUNE_SMOKE_ANIME").is_some()
                        || std::env::var_os("MOCHUNE_SMOKE_SOURCE").is_some())
                    && !g.get_busy()
                {
                    let data = b.lock().unwrap();
                    let auto = match &work.task {
                        Task::Search(_) => !data.results.is_empty(),
                        Task::Children(_, _) => !data.children.is_empty(),
                        _ => false,
                    };
                    drop(data);
                    if auto {
                        let weak = w.as_weak();
                        slint::Timer::single_shot(Duration::from_millis(100), move || {
                            if let Some(w) = weak.upgrade() {
                                sources(&w).invoke_pick(0);
                            }
                        });
                    }
                }
            });
        }
    });
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    let st = state.clone();
    let send = tx.clone();
    let gen_id = generation.clone();
    w.global::<Sources>().on_select_mode(move |mode| {
        let Some(w) = weak.upgrade() else {
            return;
        };
        let mode = mode.clamp(0, 3);
        let g = sources(&w);
        let old = g.get_mode();
        if old == mode {
            return;
        }
        if w.get_music_layout() {
            return;
        }
        if w.get_fullscreen_mode() {
            w.invoke_toggle_fullscreen();
        }
        j.epoch.fetch_add(1, Ordering::AcqRel);
        j.pending.lock().unwrap().take();
        browse.fetch_add(1, Ordering::AcqRel);
        gen_id.fetch_add(1, Ordering::AcqRel);
        PLAYBACK_PENDING.store(0, Ordering::Release);
        let _ = send.send(PlayerCommand::Reset);
        let mut b = b.lock().unwrap();
        let mut s = st.lock().unwrap();
        if old == 0 {
            b.saved_queue = std::mem::take(&mut s.queue);
            b.saved_index = s.queue_index;
            b.saved_playlist = s.queue_is_playlist;
            b.saved_current = s.queue_has_current;
            b.saved_query = w.get_query().to_string();
            b.saved_position = w.get_position() as f64;
            b.saved_duration = w.get_duration();
            b.saved_quality = w.get_quality_index();
            b.saved_choice = w.get_quality_choice();
            b.saved_music_only = w.get_music_only();
            b.saved_formats = std::mem::take(&mut s.format_options);
            b.saved_formats_for = s.formats_for.take();
            b.saved_details = Some(details::Details::capture(&w));
            s.queue_index = 0;
            s.queue_is_playlist = false;
            s.queue_has_current = false;
        }
        s.anime_playback = None;
        b.folder = "*".into();
        b.results.clear();
        b.children.clear();
        b.parent = None;
        b.pages.clear();
        b.reader.clear();
        g.set_reader_pages(ModelRc::default());
        g.set_total_ratio(0.);
        b.current = None;
        b.last = None;
        b.more = false;
        g.set_mode(mode);
        g.set_provider(0);
        g.set_busy(false);
        g.set_page_image(Default::default());
        g.set_page_count(0);
        g.set_reader_title("".into());
        g.set_title(
            if mode == 1 {
                "Anime · Anikoto.cz"
            } else if mode == 2 {
                "Movies"
            } else {
                "Manga"
            }
            .into(),
        );
        g.set_status("Search a title in the search field above.".into());
        rows(&w, &b);
        library_rows(&w, &b);
        w.global::<crate::BrowsePages>().invoke_close_page();
        w.set_busy(false);
        w.set_resolving(false);
        w.set_playback_active(false);
        w.set_video_visible(false);
        w.set_video_frame(Default::default());
        w.set_video_display_aspect(0.);
        w.set_has_selection(false);
        w.set_player_error("".into());
        w.set_recommendations_pending(false);
        w.set_position(0.);
        w.set_duration(0.);
        w.set_paused(true);
        crate::timeline::clear(&w);
        w.invoke_reset_captions();
        if mode == 0 {
            s.queue = std::mem::take(&mut b.saved_queue);
            s.queue_index = b.saved_index;
            s.queue_is_playlist = b.saved_playlist;
            s.queue_has_current = b.saved_current;
            w.set_query(b.saved_query.clone().into());
            let selected = s.selected.clone();
            let saved_position = b.saved_position;
            let saved_duration = b.saved_duration;
            let saved_details = b.saved_details.take();
            s.format_options = std::mem::take(&mut b.saved_formats);
            s.formats_for = b.saved_formats_for.take();
            w.set_quality_index(b.saved_quality);
            w.set_quality_choice(b.saved_choice);
            w.set_music_only(b.saved_music_only);
            let options = if s.format_options.is_empty() {
                default_format_options()
            } else {
                s.format_options.clone()
            };
            w.set_quality_compact_labels(compact_labels(&options));
            w.set_quality_labels(ModelRc::new(VecModel::from(
                options
                    .iter()
                    .map(|o| slint::SharedString::from(&o.label))
                    .collect::<Vec<_>>(),
            )));
            s.resume_position = Some(saved_position);
            drop(s);
            drop(b);
            if let Some(video) = selected {
                select(&w, &video, -1);
                w.set_position(saved_position as f32);
                w.set_duration(saved_duration);
                if let Some(details) = saved_details {
                    details.apply(&w);
                }
            } else {
                w.global::<crate::BrowsePages>().invoke_home();
            }
            play_queue::publish(&w, &st);
        } else {
            w.set_query("".into());
        }
    });
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    w.global::<Sources>().on_search(move |query| {
        if let Some(w) = weak.upgrade() {
            let query = query.trim();
            if query.is_empty() {
                return;
            }
            submit(&w, &j, &b, Task::Search(query.chars().take(180).collect()));
        }
    });
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    w.global::<Sources>().on_pick(move |index| {
        if let Some(w) = weak.upgrade() {
            w.invoke_focus_video_shortcuts();
            if index < 0 {
                return;
            }
            let g = sources(&w);
            let mut data = b.lock().unwrap();
            let e = if data.parent.is_some() {
                data.children.get(index as usize)
            } else {
                data.results.get(index as usize)
            }
            .cloned();
            let Some(e) = e else {
                return;
            };
            let task = if g.get_mode() == 2 {
                data.current = Some(index as usize);
                Task::Play(e)
            } else if data.parent.is_none() {
                Task::Children(e, 0)
            } else {
                data.current = Some(index as usize);
                if g.get_mode() == 3 {
                    data.page = data
                        .resume
                        .take()
                        .filter(|s| s.chapter.as_ref().is_some_and(|c| c.id == e.id))
                        .map(|s| s.page)
                        .unwrap_or_else(|| {
                            data.library
                                .titles
                                .iter()
                                .find(|s| {
                                    s.mode == 3
                                        && s.provider == g.get_provider()
                                        && s.chapter.as_ref().is_some_and(|c| c.id == e.id)
                                })
                                .map(|s| s.page)
                                .unwrap_or(0)
                        });
                    data.reader.clear();
                    data.pages.clear();
                    sources(&w).set_reader_pages(ModelRc::default());
                    sources(&w).set_page_count(0);
                    sources(&w).set_page_image(Default::default());
                    Task::Read(e)
                } else {
                    Task::Play(e)
                }
            };
            drop(data);
            submit(&w, &j, &b, task);
        }
    });
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    w.global::<Sources>().on_back(move || {
        if let Some(w) = weak.upgrade() {
            j.epoch.fetch_add(1, Ordering::AcqRel);
            j.pending.lock().unwrap().take();
            let mut data = b.lock().unwrap();
            data.parent = None;
            data.children.clear();
            data.current = None;
            data.more = false;
            rows(&w, &data);
            sources(&w).set_busy(false);
            sources(&w).set_title("Search results".into());
            sources(&w).set_status("Choose a title.".into());
        }
    });
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    w.global::<Sources>().on_load_more(move || {
        if let Some(w) = weak.upgrade() {
            let data = b.lock().unwrap();
            let task = data
                .parent
                .clone()
                .filter(|_| data.more)
                .map(|e| Task::Children(e, data.offset));
            drop(data);
            if let Some(task) = task {
                submit(&w, &j, &b, task);
            }
        }
    });
    let weak = w.as_weak();
    let next_browser = browser.clone();
    w.global::<Sources>().on_next(move |direction, automatic| {
        if let Some(w) = weak.upgrade() {
            let g = sources(&w);
            if automatic && !g.get_auto_next() {
                return;
            }
            let data = next_browser.lock().unwrap();
            let next = data
                .current
                .and_then(|i| i.checked_add_signed(direction as isize))
                .filter(|&i| i < data.children.len());
            drop(data);
            if let Some(index) = next {
                g.invoke_pick(index as i32);
            }
        }
    });
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    w.global::<Sources>().on_turn_page(move |direction| {
        if let Some(w) = weak.upgrade() {
            w.invoke_focus_video_shortcuts();
            let mut data = b.lock().unwrap();
            let next = data
                .page
                .checked_add_signed(direction as isize)
                .filter(|&i| i < data.pages.len());
            if let Some(index) = next {
                data.page = index;
                reader::publish(&w, &mut data);
                sources(&w).set_jump_serial(sources(&w).get_jump_serial().wrapping_add(1));
                remember(&w, &mut data, &j, false);
            }
            drop(data);
            if next.is_some() {
                reader::schedule(&w, &j, &b);
            }
        }
    });
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    w.global::<Sources>().on_retry(move || {
        if let Some(w) = weak.upgrade() {
            let g = sources(&w);
            if g.get_busy() {
                j.epoch.fetch_add(1, Ordering::AcqRel);
                j.pending.lock().unwrap().take();
                g.set_busy(false);
                g.set_status("Cancelled. Retry to try again.".into());
                return;
            }
            let mut data = b.lock().unwrap();
            let mut last = data.last.clone();
            if let Some(Task::Page(_, _, index, _, _)) = last.as_ref() {
                data.page = *index;
                last = data
                    .current
                    .and_then(|i| data.children.get(i))
                    .cloned()
                    .map(Task::Read);
            }
            drop(data);
            if let Some(task) = last {
                submit(&w, &j, &b, task);
            }
        }
    });
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    let provider_state = state.clone();
    let provider_tx = tx.clone();
    let provider_generation = generation.clone();
    w.global::<Sources>().on_select_provider(move |index| {
        if let Some(w) = weak.upgrade() {
            let g = sources(&w);
            if g.get_provider() == index {
                return;
            }
            j.epoch.fetch_add(1, Ordering::AcqRel);
            j.pending.lock().unwrap().take();
            reset_source_playback(&w, &provider_state, &provider_tx, &provider_generation);
            g.set_provider(index.clamp(0, 1));
            g.set_busy(false);
            g.set_page_image(Default::default());
            g.set_page_count(0);
            let mut data = b.lock().unwrap();
            data.results.clear();
            data.children.clear();
            data.parent = None;
            data.more = false;
            data.current = None;
            data.pages.clear();
            data.reader.clear();
            g.set_reader_pages(ModelRc::default());
            g.set_total_ratio(0.);
            data.last = None;
            rows(&w, &data);
            g.set_status(
                if g.get_mode() == 2 && index == 1 {
                    "Search a movie title, or paste a TMDB movie link/ID."
                } else {
                    "Search a title above."
                }
                .into(),
            );
        }
    });
    let weak = w.as_weak();
    let j = jobs.clone();
    let b = browser.clone();
    let language_state = state.clone();
    let language_tx = tx.clone();
    let language_generation = generation.clone();
    w.global::<Sources>().on_set_dubbed(move |enabled| {
        if let Some(w) = weak.upgrade() {
            let g = sources(&w);
            if g.get_dubbed() == enabled {
                return;
            }
            reset_source_playback(&w, &language_state, &language_tx, &language_generation);
            g.set_dubbed(enabled);
            let parent = b.lock().unwrap().parent.clone();
            if let Some(e) = parent {
                submit(&w, &j, &b, Task::Children(e, 0));
            } else {
                let query = w.get_query().to_string();
                if !query.trim().is_empty() {
                    submit(&w, &j, &b, Task::Search(query));
                }
            }
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    let send = tx.clone();
    let gen_id = generation.clone();
    let j = jobs.clone();
    let b = browser.clone();
    w.global::<Sources>().on_replay(move || {
        if let Some(w) = weak.upgrade() {
            let active = st.lock().unwrap().anime_playback.clone();
            if let Some(active) = active {
                let resume = w.get_duration() > 0. && w.get_position() < w.get_duration() - 0.5;
                if !resume {
                    let _ = send.send(PlayerCommand::Reset);
                    w.set_position(0.);
                }
                if active.created.elapsed()
                    > Duration::from_secs(if sources(&w).get_mode() == 1 { 60 } else { 300 })
                {
                    submit(
                        &w,
                        &j,
                        &b,
                        Task::Quality(
                            active.entry.clone(),
                            active.streams.clone(),
                            active.choice,
                            active.created,
                        ),
                    );
                } else {
                    start(&w, &st, &send, &gen_id, resume);
                }
            }
        }
    });
    let weak = w.as_weak();
    let st = state.clone();
    let j = jobs.clone();
    let b = browser.clone();
    w.global::<Sources>().on_choose_quality(move |index| {
        if index < 0 {
            return;
        }
        if let Some(w) = weak.upgrade() {
            let active = st.lock().unwrap().anime_playback.clone();
            if let Some(p) = active {
                let index = index as usize;
                if index >= p.streams.len() || index == p.choice {
                    return;
                }
                submit(
                    &w,
                    &j,
                    &b,
                    Task::Quality(p.entry.clone(), p.streams.clone(), index, p.created),
                );
            }
        }
    });
    folders::install(w, browser.clone(), jobs.clone());
    let weak = w.as_weak();
    let b = browser.clone();
    let j = jobs.clone();
    w.global::<Sources>().on_bookmark(move || {
        if let Some(w) = weak.upgrade() {
            remember(&w, &mut b.lock().unwrap(), &j, true);
        }
    });
    let weak = w.as_weak();
    let b = browser.clone();
    let j = jobs.clone();
    w.global::<Sources>().on_open_bookmark(move |index| {
        if let Some(w) = weak.upgrade() {
            let g = sources(&w);
            let saved = {
                let data = b.lock().unwrap();
                data.library
                    .visible(g.get_mode(), &data.folder)
                    .nth(index.max(0) as usize)
                    .cloned()
            };
            if let Some(saved) = saved {
                g.invoke_select_provider(saved.provider);
                {
                    let mut data = b.lock().unwrap();
                    data.results = vec![saved.entry.clone()];
                    data.resume = Some(saved.clone());
                    data.current = Some(0);
                }
                let task = if g.get_mode() == 2 {
                    Task::Play(saved.entry)
                } else {
                    Task::Children(saved.entry, 0)
                };
                submit(&w, &j, &b, task);
            }
        }
    });
    let weak = w.as_weak();
    let b = browser.clone();
    let j = jobs.clone();
    w.global::<Sources>().on_reader_scroll(move |normalized| {
        if let Some(w) = weak.upgrade() {
            let mut data = b.lock().unwrap();
            let mut sum = 0.;
            let mut index = 0;
            for (i, r) in data.reader.ratios.iter().enumerate() {
                if sum > *&normalized + 0.01 {
                    break;
                }
                index = i;
                sum += r;
            }
            if index != data.page && !data.pages.is_empty() {
                data.page = index;
                reader::publish(&w, &mut data);
                remember(&w, &mut data, &j, false);
                drop(data);
                reader::schedule(&w, &j, &b);
            }
        }
    });
    if smoke {
        let mode = std::env::var("MOCHUNE_SMOKE_SOURCE")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .or_else(|| std::env::var_os("MOCHUNE_SMOKE_ANIME").map(|_| 1));
        if let Some(mode) = mode {
            let query = std::env::var("MOCHUNE_SMOKE_SOURCE_QUERY")
                .or_else(|_| std::env::var("MOCHUNE_SMOKE_ANIME"))
                .unwrap_or_default();
            let provider = std::env::var("MOCHUNE_SMOKE_SOURCE_PROVIDER")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let weak = w.as_weak();
            slint::Timer::single_shot(Duration::from_millis(300), move || {
                if let Some(w) = weak.upgrade() {
                    let g = sources(&w);
                    g.invoke_select_mode(mode);
                    if provider != 0 {
                        g.invoke_select_provider(provider);
                    }
                    g.invoke_search(query.into());
                }
            });
        }
    }
    let automation = if smoke {
        std::env::var("MOCHUNE_SMOKE_SOURCE_ACTIONS").ok().map(|action|{
            let timer=slint::Timer::default();let weak=w.as_weak();let st=state.clone();let step=std::cell::Cell::new(0);let before=std::cell::Cell::new(0f32);let chapter=std::cell::RefCell::new(String::new());
            timer.start(slint::TimerMode::Repeated,Duration::from_millis(150),move||{
                let Some(w)=weak.upgrade()else{return;};let g=sources(&w);
                if g.get_busy(){return;}
                if action=="folder-audit" && step.get()==0 && g.get_can_bookmark() && !g.get_busy() {
                    g.set_folder_name("Reading".into());g.set_folder_rename(false);g.invoke_save_folder();if !g.get_bookmarked(){g.invoke_bookmark();}let created=g.get_folder_index()>=2&&g.get_bookmarks().row_count()==1;g.set_folder_name("Reading now".into());g.set_folder_rename(true);g.invoke_save_folder();let renamed=g.get_folder_names().iter().any(|s|s.as_str()=="Reading now");g.invoke_delete_folder();crate::diagnostics::record("source_smoke_folders",serde_json::json!({"created":created,"renamed":renamed,"deleted":g.get_folder_index()==1,"bookmark_kept":g.get_bookmarks().row_count()==1}));step.set(1);
                }else if action=="movie-cc" {
                    if step.get()==0&&w.get_playback_active(){if let Some(i)=w.get_caption_labels().iter().position(|s|s.as_str().to_ascii_lowercase().starts_with("english")){w.invoke_choose_caption(i as i32);step.set(1);}}
                    else if step.get()==1&&w.get_caption_choice()>0&&w.get_caption_status().is_empty(){crate::diagnostics::record("source_smoke_movie_cc",serde_json::json!({"loaded":true,"choice":w.get_caption_choice(),"tracks":w.get_caption_labels().row_count()-1}));step.set(2);}
                }else if action=="reader-audit" {
                    if step.get()==0&&g.get_page_image().size().width>0&&g.get_loaded_pages()>=2&&g.get_reader_pages().row_count()>5 {if !g.get_bookmarked(){g.invoke_bookmark();}let offset=g.get_reader_pages().row_data(4).unwrap().offset;g.invoke_reader_scroll(offset);step.set(1);}
                    else if step.get()==1&&g.get_page()==4&&g.get_page_image().size().width>0&&g.get_loaded_pages()>=6 {g.set_zoom(1.25);w.invoke_toggle_fullscreen();g.invoke_open_bookmark(0);step.set(2);}
                    else if step.get()==2&&g.get_page()==4&&g.get_page_image().size().width>0 {crate::diagnostics::record("source_smoke_reader_audit",serde_json::json!({"resume_page":g.get_page(),"prefetched":g.get_loaded_pages(),"bookmarks":g.get_bookmarks().row_count(),"fullscreen":w.get_fullscreen_mode(),"zoom":g.get_zoom(),"decoded":g.get_reader_pages().iter().filter(|r|r.image.size().width>0).count()}));step.set(3);}
                }else if action=="reader" {
                    if step.get()==0&&g.get_page_image().size().width>0&&g.get_page_count()>1 {chapter.replace(g.get_reader_title().to_string());g.invoke_turn_page(1);step.set(1);}
                    else if step.get()==1&&g.get_page()==1&&g.get_page_image().size().width>0 {g.invoke_next(1,false);step.set(2);}
                    else if step.get()==2&&g.get_page()==0&&g.get_page_image().size().width>0&&g.get_reader_title().as_str()!=chapter.borrow().as_str(){crate::diagnostics::record("source_smoke_reader",serde_json::json!({"page_turn":true,"next_chapter":true,"width":g.get_page_image().size().width}));step.set(3);}
                }else if action=="quality" {
                    if step.get()==0&&w.get_playback_active()&&w.get_position()>2. {before.set(w.get_position());let count=w.get_quality_labels().row_count();if count>1{g.invoke_choose_quality((count-1) as i32);step.set(1);}}
                    else if step.get()==1&&w.get_playback_active()&&!w.get_resolving()&&w.get_quality_choice() as usize+1==w.get_quality_labels().row_count()&&w.get_position()+1.>=before.get(){crate::diagnostics::record("source_smoke_quality",serde_json::json!({"before":before.get(),"after":w.get_position(),"choice":w.get_quality_choice(),"width":w.get_video_frame().size().width,"hardware":w.get_acceleration_verified()}));step.set(2);}
                }else if action=="switch"&&step.get()==0&&w.get_playback_active()&&w.get_position()>2. {
                    let hardware=w.get_acceleration_verified();g.invoke_select_mode(0);
                    crate::diagnostics::record("source_smoke_cleanup",serde_json::json!({"hardware_before":hardware,"source_mode":g.get_mode(),"source_session_released":st.lock().unwrap().anime_playback.is_none(),"rows":g.get_rows().row_count(),"image_width":g.get_page_image().size().width,"paused":w.get_paused()}));step.set(1);
                }
            });timer
        })
    } else {
        None
    };
    Guard {
        jobs,
        worker: Some(worker),
        state,
        _automation: automation,
    }
}
