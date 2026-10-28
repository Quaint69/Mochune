//! Personal, local-first home; bounded public channel browsing and opt-in comments.
//! One worker and one replaceable pending request. No background feed or comment polling.
use super::*;
use crate::{BrowsePages, ChannelRow, CommentRow, PageRow};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Condvar, atomic::AtomicBool};

const ROW_LIMIT: usize = 40;
const HOME_LIMIT: usize = 120;
const HOME_BATCH: usize = 20;
fn home_target(current: usize) -> usize { current.saturating_add(HOME_BATCH).min(HOME_LIMIT) }
fn history_rows(history: &[Video]) -> Vec<(PageRowData, Option<Video>)> {
    history.iter().take(HOME_LIMIT).map(|v| page_video(v, 0)).collect()
}
#[derive(Default)]
struct HomeFeed {
    sources: VecDeque<(Video, Option<recommendations::HomeCursor>)>,
    pending: VecDeque<Video>,
    seen: HashSet<String>,
    refresh: usize,
}
impl HomeFeed {
    fn accept(&mut self, candidates: Vec<Video>) {
        for video in candidates {
            if self.pending.len() == 40 { break; }
            if self.seen.len() < HOME_LIMIT * 2 && self.seen.insert(video.id.clone()) {
                self.pending.push_back(video);
            }
        }
    }
}
const SAVED_LIMIT: usize = 30;
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Channel {
    title: String,
    subtitle: String,
    uri: String,
    avatar: String,
}
impl Channel {
    fn row(&self) -> ChannelRow {
        ChannelRow {
            title: self.title.clone().into(),
            subtitle: self.subtitle.clone().into(),
            uri: self.uri.clone().into(),
            image: Default::default(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Preferences {
    version: u32,
    recent: bool,
    playlists: bool,
    channels: Vec<Channel>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: 1,
            recent: true,
            playlists: true,
            channels: Vec::new(),
        }
    }
}
#[derive(Default)]
struct Data {
    rows: Vec<(PageRowData, Option<Video>)>,
    prefs: Preferences,
    error: Option<String>,
    wanted_art: HashMap<String, HashSet<i32>>,
    profile: Option<(String, Channel)>,
    navigation: Navigation,
    feed: HomeFeed,
    home: Option<HomeSnapshot>,
}
// Metadata/continuations only: returning Home never retains a second image model.
struct HomeSnapshot {
    rows: Vec<(PageRowData,Option<Video>)>,
    feed: HomeFeed,
    mode: i32,
    more: bool,
    status: String,
    scroll: f32,
}
#[derive(Clone, Default, PartialEq, Debug)]
enum Location {
    #[default]
    Player,
    Home,
    Search(String),
    Channel {
        input: String,
        tab: i32,
        sort: i32,
        filter: String,
        duration: i32,
    },
}
#[derive(Default)]
struct Navigation {
    current: Location,
    previous: Vec<Location>,
    restore: Option<Location>,
}
impl Navigation {
    fn enter(&mut self, next: Location) -> Location {
        if let Some(target) = self.restore.take() {
            self.current = target;
        } else if self.current != next {
            if self.previous.len() == 16 {
                self.previous.remove(0);
            }
            self.previous
                .push(std::mem::replace(&mut self.current, next));
        }
        self.current.clone()
    }
    #[cfg(test)]
    fn back(&mut self) -> Option<Location> {
        let target = self.previous.pop()?;
        self.restore = Some(target.clone());
        Some(target)
    }
}
fn navigate(w: &SetupWindow, data: &Arc<Mutex<Data>>, next: Location) -> Location {
    let p = w.global::<BrowsePages>();
    let mut data = data.lock().unwrap();
    if p.get_page() == 2 {
        if let Location::Channel {
            tab,
            sort,
            filter,
            duration,
            ..
        } = &mut data.navigation.current
        {
            *tab = p.get_tab();
            *sort = p.get_sort();
            *filter = p.get_filter().chars().take(500).collect();
            *duration = p.get_duration_filter();
        }
    }
    if p.get_page()==1 && next != Location::Home && (!p.get_busy() || !data.rows.is_empty()) {
        data.home=Some(HomeSnapshot {
            rows: std::mem::take(&mut data.rows),
            feed: std::mem::take(&mut data.feed),
            mode: p.get_home_mode(), more: p.get_home_has_more() && !p.get_busy(),
            status:p.get_status().to_string(), scroll:p.get_home_scroll(),
        });
    }
    let target = data.navigation.enter(next);
    let can_go_back = !data.navigation.previous.is_empty();
    drop(data);
    p.set_can_go_back(can_go_back);
    target
}

#[derive(Clone, Default)]
struct PageRowData {
    title: String,
    subtitle: String,
    stats: String,
    uri: String,
    kind: i32,
}
impl PageRowData {
    fn row(&self) -> PageRow {
        PageRow {
            title: self.title.clone().into(),
            subtitle: self.subtitle.clone().into(),
            stats: self.stats.clone().into(),
            uri: self.uri.clone().into(),
            kind: self.kind,
            image: Default::default(),
            loading: false,
            duration: self
                .stats
                .rsplit(" · ")
                .next()
                .filter(|s| s.contains(':') && s.split(':').all(|part| part.parse::<u32>().is_ok()))
                .unwrap_or("")
                .into(),
        }
    }
}
#[derive(Clone)]
enum Job {
    Home,
    HomeMore,
    HomeNew,
    HomeHistory,
    Artwork(Vec<String>),
    Search(String),
    Channels(String, bool),
    Channel(String, i32, i32),
    Comments(String, bool),
}
struct Mailbox {
    pending: Mutex<Option<(u64, Job)>>,
    artwork: Mutex<VecDeque<(u64, String)>>,
    art_requests: Mutex<HashSet<(u64, String)>>,
    ready: Condvar,
    epoch: Arc<AtomicU64>,
    stopped: AtomicBool,
}
impl Mailbox {
    fn send(&self, job: Job) {
        let mut pending = self.pending.lock().unwrap();
        let epoch = if matches!(job, Job::HomeMore) {
            self.epoch.load(Ordering::Acquire)
        } else {
            let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
            self.artwork.lock().unwrap().clear();
            self.art_requests.lock().unwrap().clear();
            epoch
        };
        *pending = Some((epoch, job));
        self.ready.notify_one();
    }
    fn request_art(&self, id: String) {
        let _pending = self.pending.lock().unwrap();
        let n = self.epoch.load(Ordering::Acquire);
        let mut art = self.artwork.lock().unwrap();
        if art.len() < 48 && self.art_requests.lock().unwrap().insert((n, id.clone())) {
            art.push_back((n, id));
            self.ready.notify_one();
        }
    }
    fn prune_art(&self, wanted: &HashSet<String>) {
        let _pending = self.pending.lock().unwrap();
        let mut queue = self.artwork.lock().unwrap();
        let mut requests = self.art_requests.lock().unwrap();
        queue.retain(|key| {
            if wanted.contains(&key.1) { true } else { requests.remove(key); false }
        });
    }
    fn prioritize_art(&self, ordered: &[String]) {
        let _pending = self.pending.lock().unwrap();
        self.artwork.lock().unwrap().make_contiguous().sort_by_key(|(_, id)| {
            ordered.iter().position(|v| v == id).unwrap_or(usize::MAX)
        });
    }
    fn cancel(&self) {
        let mut pending = self.pending.lock().unwrap();
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.artwork.lock().unwrap().clear();
        self.art_requests.lock().unwrap().clear();
        *pending = None;
    }
}
// Covers both queued and executing work, including the UI publication. A
// recycled row cannot enqueue a second fetch while the first is still pending.
struct ArtworkClaim {
    mailbox: Arc<Mailbox>,
    key: (u64, String),
}
impl Drop for ArtworkClaim {
    fn drop(&mut self) { self.mailbox.art_requests.lock().unwrap().remove(&self.key); }
}
pub(super) struct Guard {
    mailbox: Arc<Mailbox>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.mailbox.stopped.store(true, Ordering::Release);
        self.mailbox.cancel();
        self.mailbox.ready.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Accept channel identities only, never arbitrary web URLs or video paths.
pub(super) fn channel_url(input: &str) -> Option<String> {
    let u = url::Url::parse(input).ok()?;
    if u.scheme() != "https"
        || !matches!(
            u.host_str()?,
            "youtube.com" | "www.youtube.com" | "m.youtube.com"
        )
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some()
    {
        return None;
    }
    let parts: Vec<_> = u.path_segments()?.filter(|s| !s.is_empty()).collect();
    let base = match parts.as_slice() {
        [handle, ..] if handle.starts_with('@') && handle.len() > 1 => format!("/{handle}"),
        ["channel", id, ..]
            if id.starts_with("UC")
                && id.len() == 24
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') =>
        {
            format!("/channel/{id}")
        }
        [prefix @ ("c" | "user"), name, ..] if !name.is_empty() => format!("/{prefix}/{name}"),
        _ => return None,
    };
    let expected = if parts[0].starts_with('@') { 1 } else { 2 };
    if parts.len() > expected
        && (parts.len() != expected + 1
            || !matches!(parts[expected], "videos" | "shorts" | "featured" | "about"))
    {
        return None;
    }
    if base.len() > 240
        || base.contains("%2f")
        || base.contains("%2F")
        || base.contains("%5c")
        || base.contains("%5C")
    {
        return None;
    }
    Some(format!("https://www.youtube.com{base}"))
}
pub(super) fn channel_search_query(query: &str) -> Option<&str> {
    query
        .trim()
        .strip_prefix('@')
        .map(str::trim)
        .filter(|s| !s.is_empty())
}
fn safe_avatar_url(value: &str) -> Option<String> {
    let value = if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.into()
    };
    let url = url::Url::parse(&value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !matches!(
            url.host_str()?,
            "yt3.ggpht.com" | "yt3.googleusercontent.com"
        )
        || value.len() > 2048
    {
        return None;
    }
    // Google-hosted avatar sizing is part of the image URL, not a second API.
    if let Some(start) = value.rfind("=s") {
        let digits = value[start + 2..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digits > 0 {
            return Some(format!(
                "{}128{}",
                &value[..start + 2],
                &value[start + 2 + digits..]
            ));
        }
    }
    Some(value)
}
fn avatar_url(thumbnails: &Value) -> String {
    thumbnails
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| {
            let id = t["id"].as_str().unwrap_or("");
            id.contains("avatar")
                || (t["width"]
                    .as_u64()
                    .is_some_and(|w| w > 0 && Some(w) == t["height"].as_u64()))
                || (t["width"].is_null() && t["height"].is_null())
        })
        .filter_map(|t| safe_avatar_url(t["url"].as_str()?))
        .next()
        .unwrap_or_default()
}
fn avatar_pixels(uri: &str) -> Result<(Vec<u8>, u32, u32)> {
    let uri = safe_avatar_url(uri).context("No safe channel portrait URL")?;
    let _permit = ImagePermit::acquire();
    let mut response = metadata_agent().get(&uri).call()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(512 * 1024)
        .read_to_vec()?;
    let image = decode_thumbnail(&bytes)?.thumbnail(128, 128).to_rgba8();
    let (width, height) = image.dimensions();
    Ok((image.into_raw(), width, height))
}
fn channel_profile(info: &Value, uri: &str) -> Channel {
    Channel {
        title: short(info, "channel", 300),
        uri: uri.into(),
        avatar: avatar_url(&info["thumbnails"]),
        subtitle: info["channel_follower_count"]
            .as_u64()
            .or_else(|| info["follower_count"].as_u64())
            .map(|n| format!("{} subscribers", view_count(n).trim_end_matches(" views")))
            .unwrap_or_default(),
    }
}
fn header_subscribers(value: &Value, depth: usize) -> Option<String> {
    if depth > 14 {
        return None;
    }
    let text = channel_text(value);
    if text.len() < 100 && text.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        if let Some(end) = text.to_ascii_lowercase().find("subscribers") {
            return Some(text[..end + "subscribers".len()].into());
        }
    }
    match value {
        Value::Object(map) => map.values().find_map(|v| header_subscribers(v, depth + 1)),
        Value::Array(rows) => rows.iter().find_map(|v| header_subscribers(v, depth + 1)),
        _ => None,
    }
}
fn complete_profile(mut profile: Channel) -> Channel {
    if !profile.avatar.is_empty() && !profile.subtitle.is_empty() {
        return profile;
    }
    // Only the opened channel header gets this bounded fallback, never every video card.
    let result = (|| -> Result<Value> {
        let mut response = metadata_agent()
            .get(&profile.uri)
            .header("Accept-Language", "en-US,en;q=0.8")
            .call()?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(4 * 1024 * 1024)
            .read_to_vec()?;
        let page = std::str::from_utf8(&bytes)?;
        recommendations::json_after(page, "var ytInitialData = ")
            .or_else(|| recommendations::json_after(page, "ytInitialData = "))
            .context("Channel header unavailable")
    })();
    if let Ok(info) = result {
        if profile.avatar.is_empty() {
            profile.avatar =
                avatar_url(&info["metadata"]["channelMetadataRenderer"]["avatar"]["thumbnails"]);
        }
        if profile.subtitle.is_empty() {
            profile.subtitle = header_subscribers(&info["header"], 0).unwrap_or_default();
        }
        if profile.title.is_empty() {
            profile.title = channel_text(&info["metadata"]["channelMetadataRenderer"]["title"]);
        }
    }
    profile
}
fn hydrate_channels(
    found: &[Channel],
    weak: &slint::Weak<SetupWindow>,
    epoch: &Arc<AtomicU64>,
    n: u64,
) {
    for channel in found {
        if !wait_for_playback(Some((epoch, n))) {
            break;
        }
        if let Ok((pixels, width, height)) = avatar_pixels(&channel.avatar) {
            let uri = channel.uri.clone();
            post(weak, epoch, n, move |w| {
                let p = w.global::<BrowsePages>();
                let image = slint::Image::from_rgba8(
                    slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &pixels, width, height,
                    ),
                );
                if p.get_channel_url().as_str() == uri {
                    p.set_channel_avatar(image.clone());
                }
                let rows = p.get_channel_hits();
                for i in 0..rows.row_count() {
                    if let Some(mut row) = rows.row_data(i).filter(|r| r.uri.as_str() == uri) {
                        row.image = image.clone();
                        rows.set_row_data(i, row);
                    }
                }
            });
        }
    }
}
#[cfg(test)]
fn playlist_url(input: &str) -> Option<String> {
    let u = url::Url::parse(input).ok()?;
    if u.scheme() != "https"
        || !matches!(u.host_str()?, "youtube.com" | "www.youtube.com")
        || u.path() != "/playlist"
    {
        return None;
    }
    let id = u.query_pairs().find(|(k, _)| k == "list")?.1;
    if id.is_empty()
        || id.len() > 100
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return None;
    }
    Some(format!("https://www.youtube.com/playlist?list={id}"))
}
fn short(v: &Value, key: &str, max: usize) -> String {
    v[key].as_str().unwrap_or("").chars().take(max).collect()
}
fn channel_text(v: &Value) -> String {
    v.as_str()
        .or(v["simpleText"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            v["runs"]
                .as_array()
                .map(|r| r.iter().filter_map(|v| v["text"].as_str()).collect())
                .unwrap_or_default()
        })
        .chars()
        .take(300)
        .collect()
}
fn collect_channels(v: &Value, out: &mut Vec<Channel>, depth: usize) {
    if depth > 40 || out.len() == 3 {
        return;
    }
    if let Some(c) = v.get("channelRenderer") {
        let id = c["channelId"].as_str().unwrap_or("");
        if let Some(uri) = channel_url(&format!("https://www.youtube.com/channel/{id}")) {
            if !out.iter().any(|c| c.uri == uri) {
                out.push(Channel {
                    title: channel_text(&c["title"]),
                    subtitle: channel_text(&c["subscriberCountText"]),
                    avatar: avatar_url(&c["thumbnail"]["thumbnails"]),
                    uri,
                });
            }
        }
        return;
    }
    match v {
        Value::Object(m) => {
            for (key, value) in m {
                if !matches!(key.as_str(), "adSlotRenderer" | "menu" | "onTap") {
                    collect_channels(value, out, depth + 1);
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                collect_channels(v, out, depth + 1);
            }
        }
        _ => (),
    }
}
fn channels(query: &str) -> Result<Vec<Channel>> {
    let mut url = url::Url::parse("https://www.youtube.com/results")?;
    url.query_pairs_mut()
        .append_pair("search_query", query)
        .append_pair("sp", "EgIQAg==")
        .append_pair("hl", "en");
    let mut response = metadata_agent()
        .get(url.as_str())
        .header("Accept-Language", "en-US,en;q=0.8")
        .call()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()?;
    let page = std::str::from_utf8(&bytes)?;
    let data = recommendations::json_after(page, "var ytInitialData = ")
        .or_else(|| recommendations::json_after(page, "ytInitialData = "))
        .context("Channel search unavailable")?;
    let mut result = Vec::new();
    collect_channels(&data, &mut result, 0);
    Ok(result)
}
fn read_preferences(paths: &Paths) -> Result<Preferences> {
    let file = paths.data.join("home.json");
    if !file.exists() {
        return Ok(Preferences::default());
    }
    anyhow::ensure!(
        fs::metadata(&file)?.len() <= 64 * 1024,
        "Home preferences are too large"
    );
    let mut p: Preferences = serde_json::from_slice(&fs::read(file)?)?;
    anyhow::ensure!(p.version == 1, "Unsupported home preferences version");
    p.channels.retain(|c| {
        channel_url(&c.uri).as_deref() == Some(c.uri.as_str())
            && c.title.len() <= 1000
            && c.subtitle.len() <= 1000
    });
    p.channels.truncate(SAVED_LIMIT);
    Ok(p)
}
fn save_preferences(paths: &Paths, data: &Data) -> Result<()> {
    anyhow::ensure!(
        data.error.is_none(),
        "Home preferences could not be read; original preserved"
    );
    paths.save("home.json", &data.prefs)
}
fn comments(info: &Value) -> Vec<CommentRow> {
    info["comments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let body = short(c, "text", 2000);
            if body.trim().is_empty() {
                return None;
            }
            Some(CommentRow {
                loading: false,
                author: short(c, "author", 160).into(),
                body: body.into(),
                detail: [
                    c["like_count"].as_u64().map(|n| format!("{n} likes")),
                    c["is_favorited"]
                        .as_bool()
                        .filter(|v| *v)
                        .map(|_| "Creator liked".into()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ")
                .into(),
            })
        })
        .take(ROW_LIMIT)
        .collect()
}
fn visible_rows(
    rows: &[(PageRowData, Option<Video>)],
    filter: &str,
    duration: i32,
    sort: i32,
) -> Vec<PageRow> {
    let query = filter.to_lowercase();
    let mut indices: Vec<_> = (0..rows.len())
        .filter(|&i| {
            let (r, v) = &rows[i];
            r.title.to_lowercase().contains(&query)
                && v.as_ref().is_none_or(|v| match duration {
                    1 => v.duration > 0. && v.duration < 240.,
                    2 => (240. ..=1200.).contains(&v.duration),
                    3 => v.duration > 1200.,
                    _ => true,
                })
        })
        .collect();
    if sort != 0 {
        indices.sort_by_key(|&i| {
            let video = rows[i].1.as_ref();
            let value = if sort == 3 {
                video.and_then(|v| v.views)
            } else {
                video.and_then(|v| v.published)
            };
            (
                value.is_none(),
                if sort == 2 {
                    value.unwrap_or(0)
                } else {
                    u64::MAX - value.unwrap_or(0)
                },
            )
        });
    }
    indices.into_iter().map(|i| rows[i].0.row()).collect()
}
fn post(
    weak: &slint::Weak<SetupWindow>,
    epoch: &Arc<AtomicU64>,
    n: u64,
    f: impl FnOnce(SetupWindow) + Send + 'static,
) {
    ui_generation(weak, epoch, n, f);
}
fn reconcile_rows(old: ModelRc<PageRow>, mut rows: Vec<PageRow>) -> ModelRc<PageRow> {
    if let Some(model) = old.as_any().downcast_ref::<VecModel<PageRow>>() {
        for row in &mut rows {
            if row.image.size().width == 0 {
                if let Some(previous) = model.iter().find(|r| r.uri == row.uri) {
                    row.image = previous.image;
                }
            }
        }
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
fn placeholders(kind: i32, count: usize) -> Vec<PageRow> {
    (0..count)
        .map(|_| PageRow {
            kind,
            loading: true,
            ..Default::default()
        })
        .collect()
}
fn prime_rows(w: &SetupWindow, count: usize) {
    let p = w.global::<BrowsePages>();
    p.set_rows(reconcile_rows(p.get_rows(), placeholders(0, count)));
}
fn prime_home(w: &SetupWindow) {
    publish_rows(w, placeholders(0, HOME_BATCH));
}
fn apply_artwork(w: &SetupWindow, uri: &str, image: slint::Image) {
    let p = w.global::<BrowsePages>();
    let mut models = vec![p.get_rows()];
    models.extend(p.get_sections().iter().map(|s| s.rows));
    for model in models {
        for i in 0..model.row_count() {
            if let Some(mut row) = model
                .row_data(i)
                .filter(|r| r.uri.as_str() == uri && r.image != image)
            {
                row.image = image.clone();
                model.set_row_data(i, row);
            }
        }
    }
}
fn load_artwork(
    ids: Vec<String>,
    n: u64,
    mailbox: &Arc<Mailbox>,
    weak: &slint::Weak<SetupWindow>,
    data: &Arc<Mutex<Data>>,
    paths: &Paths,
) {
    for id in ids {
        if !wait_for_playback(Some((&mailbox.epoch, n))) {
            break;
        }
        let claim = ArtworkClaim { mailbox: mailbox.clone(), key: (n, id.clone()) };
        let video = {
            let d = data.lock().unwrap();
            if !d.wanted_art.get(&id).is_some_and(|roles| !roles.is_empty()) {
                continue;
            }
            d.rows
                .iter()
                .find(|(_, v)| v.as_ref().is_some_and(|v| v.id == id))
                .and_then(|r| r.1.clone())
        };
        let Some(video) = video else { continue };
        if let Ok((pixels, width, height)) = thumbnail_sized(paths, &video, 320, 180) {
            let d = data.clone();
            post(weak, &mailbox.epoch, n, move |w| {
                let _claim = claim;
                if d.lock()
                    .unwrap()
                    .wanted_art
                    .get(&id)
                    .is_some_and(|roles| !roles.is_empty())
                {
                    let image = slint::Image::from_rgba8(slint::SharedPixelBuffer::<
                        slint::Rgba8Pixel,
                    >::clone_from_slice(
                        &pixels, width, height
                    ));
                    apply_artwork(&w, &id, image);
                }
            });
        }
    }
}
fn publish_rows(w: &SetupWindow, rows: Vec<PageRow>) {
    let p = w.global::<BrowsePages>();
    p.set_sections(ModelRc::default());
    p.set_rows(reconcile_rows(p.get_rows(), rows));
    p.set_art_epoch(p.get_art_epoch().wrapping_add(1));
}

fn page_video(video: &Video, kind: i32) -> (PageRowData, Option<Video>) {
    let mut metadata = Vec::new();
    if let Some(views) = video.views {
        metadata.push(view_count(views));
    }
    if video.duration > 0. {
        metadata.push(clock(video.duration));
    }
    (
        PageRowData {
            title: video.title.clone(),
            subtitle: video.channel.clone(),
            stats: metadata.join(" · "),
            uri: video.id.clone(),
            kind,
        },
        Some(video.clone()),
    )
}
// Read the actual channel sort controls; legacy URL ?sort= values are ignored
// by current YouTube pages. Never substitute a local sort of the newest batch.
fn channel_sort_token(node: &Value, sort: i32, depth: usize) -> Option<String> {
    if depth > 40 {
        return None;
    }
    let label = match sort {
        2 => "Oldest",
        3 => "Popular",
        _ => "Latest",
    };
    for key in ["chipViewModel", "chipCloudChipRenderer"] {
        if let Some(chip) = node.get(key) {
            if channel_text(&chip["text"]).eq_ignore_ascii_case(label) {
                let command = if key == "chipViewModel" {
                    &chip["tapCommand"]["innertubeCommand"]
                } else {
                    &chip["navigationEndpoint"]
                };
                if let Some(token) = command
                    .pointer("/continuationCommand/token")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 16384)
                {
                    return Some(token.into());
                }
            }
        }
    }
    match node {
        Value::Object(m) => m
            .values()
            .find_map(|v| channel_sort_token(v, sort, depth + 1)),
        Value::Array(a) => a
            .iter()
            .find_map(|v| channel_sort_token(v, sort, depth + 1)),
        _ => None,
    }
}
fn sorted_channel(target: &str, sort: i32, epoch: &Arc<AtomicU64>, n: u64) -> Result<Value> {
    sorted_channel_limit(target,sort,epoch,n,30)
}
fn sorted_channel_limit(target: &str, sort: i32, epoch: &Arc<AtomicU64>, n: u64, limit: usize) -> Result<Value> {
    let mut response = metadata_agent()
        .get(target)
        .header("Accept-Language", "en-US,en;q=0.8")
        .call()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()?;
    let page = std::str::from_utf8(&bytes)?;
    let initial = recommendations::json_after(page, "var ytInitialData = ")
        .or_else(|| recommendations::json_after(page, "ytInitialData = "))
        .context("Channel feed unavailable")?;
    let context = recommendations::json_after(page, "ytcfg.set(")
        .and_then(|v| v.get("INNERTUBE_CONTEXT").cloned())
        .context("Channel request context unavailable")?;
    let mut token = channel_sort_token(&initial, sort, 0)
        .context("YouTube does not offer that sort for this channel tab")?;
    let meta = &initial["metadata"]["channelMetadataRenderer"];
    let title = channel_text(&meta["title"]);
    let mut info = serde_json::json!({"channel":title,"title":title,"description":meta["description"],"thumbnails":meta["avatar"]["thumbnails"],"header":initial["header"]});
    drop(initial);
    drop(bytes);
    let mut videos = Vec::new();
    let mut seen_tokens = HashSet::new();
    for _ in 0..3 {
        anyhow::ensure!(wait_for_playback(Some((epoch, n))), "Request cancelled");
        if !seen_tokens.insert(token.clone()) {
            break;
        }
        let mut response = metadata_agent()
            .post("https://www.youtube.com/youtubei/v1/browse")
            .send_json(serde_json::json!({"context":context,"continuation":token}))?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(4 * 1024 * 1024)
            .read_to_vec()?;
        let next: Value = serde_json::from_slice(&bytes)?;
        let (batch, continuation) = recommendations::browse_candidates(&next);
        for mut v in batch {
            if videos.len() == limit {
                break;
            }
            if !videos.iter().any(|old: &Video| old.id == v.id) {
                v.channel = title.clone();
                videos.push(v);
            }
        }
        if videos.len() >= limit {
            break;
        }
        let Some(next) = continuation else {
            break;
        };
        token = next;
    }
    info["entries"] = serde_json::json!(videos.iter().map(|v| serde_json::json!({
        "id":v.id,"title":v.title,"channel":v.channel,"duration":v.duration,"view_count":v.views,"timestamp":v.published
    })).collect::<Vec<_>>());
    Ok(info)
}
fn reconcile_comments(old: ModelRc<CommentRow>, rows: Vec<CommentRow>) -> ModelRc<CommentRow> {
    if let Some(model) = old.as_any().downcast_ref::<VecModel<CommentRow>>() {
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

fn channel_video_rows(info: &Value, channel: &str) -> Vec<(PageRowData, Option<Video>)> {
    info["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .take(ROW_LIMIT)
        .filter_map(|entry| {
            let mut video = video_from(entry)?;
            // Flat channel feeds can omit per-video authors; the opened channel
            // supplies that identity without fetching every video's metadata.
            if entry["channel"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| entry["uploader"].as_str().filter(|s| !s.trim().is_empty()))
                .is_none()
                && !channel.trim().is_empty()
            {
                video.channel = channel.into();
            }
            Some(page_video(&video, 0))
        })
        .collect()
}
fn home_search_target(query: &str) -> Result<String> {
    Ok(search_target(query)?.replacen("ytsearch30:", "ytsearch20:", 1))
}

fn run_job(
    job: Job,
    n: u64,
    mailbox: &Arc<Mailbox>,
    weak: &slint::Weak<SetupWindow>,
    state: &Arc<Mutex<State>>,
    data: &Arc<Mutex<Data>>,
    paths: &Paths,
) -> Result<()> {
    let epoch = &mailbox.epoch;
    let mut channel_images = Vec::new();
    anyhow::ensure!(wait_for_playback(Some((epoch, n))), "Request cancelled");
    if !matches!(&job, Job::Artwork(_) | Job::Comments(..) | Job::HomeMore) {
        data.lock().unwrap().wanted_art.clear();
    }
    match job {
        Job::Artwork(ids) => {
            load_artwork(ids, n, mailbox, weak, data, paths);
            return Ok(());
        }
        Job::Home | Job::HomeMore | Job::HomeNew | Job::HomeHistory => {
            let more = matches!(job, Job::HomeMore);
            let fresh = matches!(job, Job::HomeNew);
            let local_history = matches!(job, Job::HomeHistory);
            let (history, mut settings) = {
                let st = state.lock().unwrap();
                (st.history[0].clone(), st.settings.clone())
            };
            settings.music = false;
            if local_history {
                let rows = history_rows(&history);
                data.lock().unwrap().feed = HomeFeed::default();
                data.lock().unwrap().rows = rows.clone();
                post(weak, epoch, n, move |w| {
                    publish_rows(&w, rows.iter().map(|r| r.0.row()).collect());
                    let p = w.global::<BrowsePages>();
                    p.set_home_has_more(false); p.set_busy(false); p.set_status("".into());
                    p.invoke_artwork_reflow();
                });
            } else {
                let (mut feed, mut rows) = {
                    let mut d = data.lock().unwrap();
                    let previous = if fresh { d.rows.iter().map(|r| r.0.uri.clone()).take(HOME_LIMIT).collect() } else { HashSet::new() };
                    let mut feed = std::mem::take(&mut d.feed);
                    if !more {
                        let refresh = feed.refresh.wrapping_add(usize::from(fresh));
                        feed = HomeFeed { refresh, seen: previous, ..Default::default() };
                        let mut seeds = recommendations::history_seeds(&history, false);
                        if !seeds.is_empty() { let len = seeds.len(); seeds.rotate_left(refresh % len); }
                        feed.sources = seeds.into_iter().map(|v| (v, None)).collect();
                        d.rows.clear();
                    }
                    (feed, d.rows.clone())
                };
                let target = home_target(rows.len());
                let mut requests = 0;
                while rows.len() < target {
                    if let Some(video) = feed.pending.pop_front() {
                        rows.push(page_video(&video, 0));
                        continue;
                    }
                    if requests == 3 || paths.network.offline() { break; }
                    let Some((mut seed, cursor)) = feed.sources.pop_front() else { break };
                    anyhow::ensure!(wait_for_playback(Some((epoch, n))), "Request cancelled");
                    requests += 1;
                    match recommendations::home_page(&seed.id, cursor) {
                        Ok((page, cursor)) => {
                            if page.classified { seed.music = page.music; }
                            let candidates = recommendations::filter(page.videos, &settings, &history, Some(&seed));
                            feed.accept(candidates);
                            if let Some(cursor) = cursor { feed.sources.push_back((seed, Some(cursor))); }
                        }
                        Err(error) => { crate::diagnostics::record("home_page_failed", serde_json::json!({"error":error.to_string()})); }
                    }
                }
                anyhow::ensure!(epoch.load(Ordering::Acquire) == n, "Request cancelled");
                // Do not repeatedly auto-fetch empty filtered pages at the bottom.
                let added = rows.len() > data.lock().unwrap().rows.len();
                let has_more = rows.len() < HOME_LIMIT && added && (!feed.pending.is_empty() || !feed.sources.is_empty());
                let status = if paths.network.offline() { "Offline" }
                    else if rows.is_empty() && !history.is_empty() { "No new recommendations available. Try New again later." }
                    else { "" };
                {
                    let mut d = data.lock().unwrap(); d.rows = rows.clone(); d.feed = feed;
                }
                post(weak, epoch, n, move |w| {
                    let p = w.global::<BrowsePages>();
                    publish_rows(&w, rows.iter().map(|r| r.0.row()).collect());
                    p.set_home_has_more(has_more); p.set_status(status.into()); p.set_busy(false);
                    p.invoke_artwork_reflow();
                });
            }
            return Ok(());
        }
        Job::Search(query) => {
            anyhow::ensure!(
                !paths.network.offline(),
                "Offline — search needs a connection"
            );
            let video_query = channel_search_query(&query).unwrap_or(&query);
            if channel_search_query(&query).is_some() {
                channel_images = channels(video_query).unwrap_or_default();
                let rows = channel_images.clone();
                post(weak, epoch, n, move |w| {
                    w.global::<BrowsePages>()
                        .set_channel_hits(ModelRc::new(VecModel::from(
                            rows.iter().map(Channel::row).collect::<Vec<_>>(),
                        )))
                });
            }
            let target = home_search_target(video_query)?;
            let settings = Settings::default();
            let info = extract(paths, &settings, &target, true, Some((epoch, n)))?;
            let videos = videos_from(&info).into_iter().take(20).collect::<Vec<_>>();
            let empty = videos.is_empty();
            data.lock().unwrap().rows = videos.iter().map(|v| page_video(v, 0)).collect();
            post(weak, epoch, n, move |w| {
                let p = w.global::<BrowsePages>();
                p.set_busy(false);
                p.set_status(if empty { "No videos found." } else { "" }.into());
            });
        }
        Job::Channels(query, page) => {
            anyhow::ensure!(
                !paths.network.offline(),
                "Offline — channel search needs a connection"
            );
            let found = channels(&query)?;
            channel_images = found.clone();
            if page {
                data.lock().unwrap().rows = found
                    .iter()
                    .map(|c| {
                        (
                            PageRowData {
                                title: c.title.clone(),
                                subtitle: c.subtitle.clone(),
                                stats: String::new(),
                                uri: c.uri.clone(),
                                kind: 3,
                            },
                            None,
                        )
                    })
                    .collect();
            }
            post(weak, epoch, n, move |w| {
                let p = w.global::<BrowsePages>();
                p.set_channel_hits(ModelRc::new(VecModel::from(
                    found.iter().map(Channel::row).collect::<Vec<_>>(),
                )));
                if page {
                    p.set_busy(false);
                    p.set_status(
                        if found.is_empty() {
                            "No matching channels found."
                        } else {
                            "Choose a channel."
                        }
                        .into(),
                    );
                } else {
                    p.set_channel_hits(ModelRc::new(VecModel::from(
                        found.iter().map(Channel::row).collect::<Vec<_>>(),
                    )));
                }
            });
            if !page {
                hydrate_channels(&channel_images, weak, epoch, n);
                return Ok(());
            }
        }
        Job::Channel(url, tab, sort) => {
            anyhow::ensure!(
                !paths.network.offline(),
                "Offline — channel browsing needs a connection"
            );
            let base = channel_url(&url).context("Unsupported channel URL")?;
            let target = format!("{base}/{}", ["videos", "shorts"][tab.clamp(0, 1) as usize]);
            let settings = Settings::default();
            let info = if sort == 0 {
                extract(paths, &settings, &target, true, Some((epoch, n)))?
            } else {
                sorted_channel(&target, sort, epoch, n)?
            };
            let mut profile = channel_profile(&info, &base);
            if profile.subtitle.is_empty() {
                profile.subtitle = header_subscribers(&info["header"], 0).unwrap_or_default();
            }
            if let Some((_, known)) = data
                .lock()
                .unwrap()
                .profile
                .as_ref()
                .filter(|(uri, _)| uri == &base)
            {
                if profile.avatar.is_empty() {
                    profile.avatar = known.avatar.clone();
                }
                if profile.subtitle.is_empty() {
                    profile.subtitle = known.subtitle.clone();
                }
            }
            profile = complete_profile(profile);
            let title = profile.title.clone();
            let rows = channel_video_rows(&info, &title);
            let subscribers = if profile.subtitle.is_empty() {
                "Subscribers unavailable".into()
            } else {
                profile.subtitle.clone()
            };
            data.lock().unwrap().profile = Some((base, profile.clone()));
            channel_images.push(profile);
            let description = short(&info, "description", 500);
            let status = if rows.is_empty() {
                "No public items found in this tab. This tab may be empty, private, or unavailable."
                    .into()
            } else {
                String::new()
            };
            data.lock().unwrap().rows = rows;
            post(weak, epoch, n, move |w| {
                let p = w.global::<BrowsePages>();
                p.set_busy(false);
                p.set_title(if title.is_empty() {
                    "Channel".into()
                } else {
                    title.into()
                });
                p.set_description(description.into());
                p.set_channel_subscribers(subscribers.into());
                p.set_status(status.into());
            });
        }
        Job::Comments(id, newest) => {
            anyhow::ensure!(
                !paths.network.offline(),
                "Offline — comments need a connection"
            );
            let settings = Settings {
                comment_request: Some(newest),
                format_override: Some("bestaudio/best".into()),
                ..Default::default()
            };
            let info = extract(
                paths,
                &settings,
                &format!("https://www.youtube.com/watch?v={id}"),
                false,
                Some((epoch, n)),
            )?;
            let rows = comments(&info);
            let status = if rows.is_empty() {
                "No public comments returned. They may be disabled or unavailable.".into()
            } else {
                format!(
                    "{} comments · loaded on request · replies are not loaded",
                    rows.len()
                )
            };
            post(weak, epoch, n, move |w| {
                if w.get_selected_id() != id {
                    return;
                }
                let p = w.global::<BrowsePages>();
                p.set_comments(reconcile_comments(p.get_comments(), rows));
                p.set_comments_status(status.into());
                p.set_comments_busy(false);
                p.set_comments_loaded(true);
            });
            return Ok(());
        }
    }
    if epoch.load(Ordering::Acquire) != n {
        return Ok(());
    }
    let rows = data.lock().unwrap().rows.clone();
    let initial = rows.clone();
    post(weak, epoch, n, move |w| {
        let p = w.global::<BrowsePages>();
        let visible = if p.get_page() == 2 {
            visible_rows(
                &initial,
                &p.get_filter(),
                p.get_duration_filter(),
                0, // The server already returned the requested channel order.
            )
        } else {
            initial.iter().map(|r| r.0.row()).collect()
        };
        publish_rows(&w, visible);
        w.global::<BrowsePages>().invoke_artwork_reflow();
    });
    hydrate_channels(&channel_images, weak, epoch, n);
    Ok(())
}

pub(super) fn music_artist_videos(
    paths: &Paths, settings: &Settings, uri: &str, sort: i32, epoch: &Arc<AtomicU64>, n: u64,
) -> Result<(String,Vec<Video>)> {
    let base = channel_url(uri).context("Invalid artist channel")?;
    let target=format!("{base}/videos");
    let info = if matches!(sort,1|4|5) {
        sorted_channel_limit(&target,match sort {1=>3,5=>2,_=>1},epoch,n,40)?
    } else { extract(paths, settings, &target, true, Some((epoch,n)))? };
    let title = short(&info,"channel",300);
    Ok((title.clone(),channel_video_rows(&info,&title).into_iter().filter_map(|(_,v)|v)
        .filter(|v|v.playable(true)).take(40).collect()))
}

fn interrupt_comments(p: &BrowsePages<'_>) {
    if p.get_comments_busy() {
        p.set_comments_busy(false);
        p.set_comments_loaded(false);
        p.set_comments(reconcile_comments(p.get_comments(), Vec::new()));
        p.set_comments_status("Comment loading cancelled. Use Refresh to try again.".into());
    }
}

/// Local-only render fixtures, reached exclusively by the existing smoke-test mode.
pub(crate) fn preview(w: &SetupWindow, name: &str) {
    let p = w.global::<BrowsePages>();
    w.set_music_layout(false);
    if name == "home-back" {
        p.set_page(0);
        p.invoke_home();
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_secs(1), move || {
            if let Some(w) = weak.upgrade() {
                w.global::<BrowsePages>().invoke_back();
            }
        });
        return;
    }
    if name == "channel-loading" {
        p.set_page(2);
        p.set_channel_url("https://www.youtube.com/channel/UCQeRaTukNYft1_6AZPACnog".into());
        p.set_title("".into());
        p.set_busy(true);
        prime_rows(w, 30);
        return;
    }
    if name == "home-return" {
        p.set_page(0);
        w.set_has_selection(true);
        p.invoke_home();
        return;
    }
    if name == "comments-loading" {
        p.set_page(0);
        w.set_has_selection(true);
        p.set_comments_open(true);
        p.set_comments_busy(true);
        p.set_comments(reconcile_comments(
            p.get_comments(),
            (0..8)
                .map(|_| CommentRow {
                    loading: true,
                    ..Default::default()
                })
                .collect(),
        ));
        return;
    }
    let paged_fixture = name == "feed";
    let name = if paged_fixture { "artwork" } else { name };
    let long_description = name == "channel-long";
    let name = if long_description { "channel" } else { name };
    if name == "empty" {
        p.invoke_home();
        return;
    }
    if name == "search-loading" {
        p.set_page(3);
        p.set_title("Results for ducks".into());
        w.set_query("ducks".into());
        p.set_busy(true);
        prime_rows(w, 20);
        return;
    }
    if name == "skeleton" {
        p.set_page(1);
        p.set_title("Home".into());
        p.set_busy(true);
        prime_home(w);
        return;
    }
    if name == "artwork" {
        let weak = w.as_weak();
        let wanted = std::rc::Rc::new(std::cell::RefCell::new(
            HashMap::<String, HashSet<i32>>::new(),
        ));
        let pending = std::rc::Rc::new(std::cell::RefCell::new(HashSet::<String>::new()));
        w.global::<BrowsePages>()
            .on_artwork_visible(move |uri, role, visible| {
                let Some(w) = weak.upgrade() else { return };
                let id = uri.to_string();
                let needed = {
                    let mut wanted = wanted.borrow_mut();
                    let roles = wanted.entry(id.clone()).or_default();
                    if visible {
                        roles.insert(role);
                    } else {
                        roles.remove(&role);
                    }
                    !roles.is_empty()
                };
                let loaded = w
                    .global::<BrowsePages>()
                    .get_rows()
                    .iter()
                    .any(|r| r.uri == uri && r.image.size().width > 0);
                if needed == loaded || !pending.borrow_mut().insert(id.clone()) {
                    return;
                }
                let weak = w.as_weak();
                let wanted = wanted.clone();
                let pending = pending.clone();
                slint::Timer::single_shot(Duration::ZERO, move || {
                    pending.borrow_mut().remove(&id);
                    if let Some(w) = weak.upgrade() {
                        if !wanted
                            .borrow()
                            .get(&id)
                            .is_some_and(|roles| !roles.is_empty())
                        {
                            apply_artwork(&w, &id, Default::default());
                            return;
                        }
                        if w.global::<BrowsePages>()
                            .get_rows()
                            .iter()
                            .any(|r| r.uri.as_str() == id && r.image.size().width > 0)
                        {
                            return;
                        }
                        let mut pixels =
                            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(320, 180);
                        let hue = id.bytes().fold(0u8, u8::wrapping_add);
                        for pixel in pixels.make_mut_slice() {
                            *pixel = slint::Rgba8Pixel::new(hue, 110, 160, 255);
                        }
                        apply_artwork(&w, &id, slint::Image::from_rgba8(pixels));
                    }
                });
            });
    }
    if name == "comments" || name == "comments-stress" {
        p.set_sections(Default::default());
        p.set_page(0);
        w.set_has_selection(true);
        w.set_selected_title("Comment layout preview".into());
        p.set_comments_open(true);
        p.set_comments_loaded(true);
        p.set_comments_status("Public comments · loaded on request".into());
        p.set_comments(ModelRc::new(VecModel::from(vec![
            CommentRow {loading:false,author:"@viewer".into(),detail:"42 likes".into(),body:"This is a local comment fixture, not an online request. Long text wraps inside the video details column without resizing the picture.".into()},
            CommentRow {loading:false,author:"@another-viewer".into(),detail:"3 likes".into(),body:"Second comment.\nText can span multiple lines and remains selectable.".into()},
        ])));
        if name == "comments-stress" {
            p.set_comments(ModelRc::new(VecModel::from(
                (0..40)
                    .map(|i| CommentRow {
                        loading: false,
                        author: format!("@viewer-{i}").into(),
                        detail: "42 likes".into(),
                        body: "A long selectable comment for bounded layout testing. "
                            .repeat(38)
                            .chars()
                            .take(2000)
                            .collect::<String>()
                            .into(),
                    })
                    .collect::<Vec<_>>(),
            )));
        }
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_secs(2), move || {
            if let Some(w) = weak.upgrade() {
                w.set_page_scroll(-600.);
            }
        });
        return;
    }
    p.set_page(if name == "channel" {
        2
    } else if name == "search" || name == "channel-search" {
        3
    } else {
        1
    });
    p.set_title(
        if name == "channel" {
            "Sample creator"
        } else {
            "Your home"
        }
        .into(),
    );
    p.set_channel_url(
        if name == "channel" {
            "https://www.youtube.com/@sample"
        } else {
            ""
        }
        .into(),
    );
    p.set_description(if long_description { "A deliberately long description to verify that the channel header keeps its fixed height and ends with an ellipsis. ".repeat(12).into() } else { "Local layout fixtures. No network or playback requested.".into() });
    let mut portrait = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(96, 96);
    for (i, pixel) in portrait.make_mut_slice().iter_mut().enumerate() {
        *pixel = slint::Rgba8Pixel::new(80 + (i % 96) as u8, 100, 180, 255);
    }
    let portrait = slint::Image::from_rgba8(portrait);
    if name == "channel" {
        p.set_channel_avatar(portrait.clone());
        p.set_channel_subscribers("1.2M subscribers".into());
        w.set_has_selection(true);
    }
    if name == "channel-search" {
        p.set_channel_hits(ModelRc::new(VecModel::from(vec![
            ChannelRow {
                title: "Sample creator".into(),
                subtitle: "1.2M subscribers".into(),
                uri: "https://www.youtube.com/@sample".into(),
                image: portrait.clone(),
            },
            ChannelRow {
                title: "Second creator".into(),
                subtitle: "28K subscribers".into(),
                uri: "https://www.youtube.com/@second".into(),
                image: portrait,
            },
        ])));
    }
    let count = if paged_fixture { HOME_BATCH } else if name == "home" || name == "artwork" {
        std::env::var("MOCHUNE_SMOKE_HOME_ITEMS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(48).clamp(1, HOME_LIMIT)
    } else if name == "search" || name == "channel-search" {
        20
    } else {
        14
    };
    let rows = (0..count)
        .map(|i| {
            PageRowData {
                title: format!(
                    "Sample {} · a longer title to check wrapping and clipping",
                    i + 1
                ),
                subtitle: "Creator".into(),
                stats: "125.0K views · 4:10".into(),
                uri: if name == "artwork" {
                    format!("art{i:08}")
                } else {
                    format!("fixture-{i}")
                },
                kind: if name == "home" || name == "artwork" {
                    10 + (i / 12) as i32
                } else {
                    0
                },
            }
            .row()
        })
        .collect();
    publish_rows(w, rows);
    if paged_fixture {
        p.set_home_has_more(true);
        let weak = w.as_weak();
        p.on_home_more(move || {
            if let Some(w) = weak.upgrade() {
                let p = w.global::<BrowsePages>();
                if p.get_busy() || !p.get_home_has_more() { return; }
                p.set_busy(true);
                let mut rows: Vec<_> = p.get_rows().iter().collect();
                let start = rows.len();
                rows.extend((start..home_target(start)).map(|i| PageRowData {
                    uri: format!("art{i:08}"), title: format!("Fixture video {}", i + 1),
                    subtitle: "Creator".into(), stats: "125K views · 4:10".into(), kind: 0,
                }.row()));
                let count = rows.len();
                publish_rows(&w, rows);
                p.set_home_has_more(count < HOME_LIMIT); p.set_busy(false);
            }
        });
    }
}

pub(super) fn install(w: &SetupWindow, state: Arc<Mutex<State>>, paths: Paths) -> Guard {
    let mut initial = Data::default();
    match read_preferences(&paths) {
        Ok(p) => initial.prefs = p,
        Err(e) => initial.error = Some(format!("{e:#}")),
    }
    let data = Arc::new(Mutex::new(initial));
    {
        let d = data.lock().unwrap();
        let p = w.global::<BrowsePages>();
        p.set_show_recent(d.prefs.recent);
        p.set_show_playlists(d.prefs.playlists);
    }
    let mailbox = Arc::new(Mailbox {
        pending: Mutex::new(None),
        artwork: Mutex::new(VecDeque::new()),
        art_requests: Mutex::new(HashSet::new()),
        ready: Condvar::new(),
        epoch: Arc::new(AtomicU64::new(0)),
        stopped: AtomicBool::new(false),
    });
    let worker_mail = mailbox.clone();
    let worker_data = data.clone();
    let worker_state = state.clone();
    let weak = w.as_weak();
    let worker_paths = paths.clone();
    let thread = thread::spawn(move || {
        loop {
            let (n, job) = {
                let mut pending = worker_mail.pending.lock().unwrap();
                while pending.is_none()
                    && worker_mail.artwork.lock().unwrap().is_empty()
                    && !worker_mail.stopped.load(Ordering::Acquire)
                {
                    pending = worker_mail.ready.wait(pending).unwrap();
                }
                if worker_mail.stopped.load(Ordering::Acquire) {
                    break;
                }
                if let Some(job) = pending.take() {
                    job
                } else {
                    let n = worker_mail.epoch.load(Ordering::Acquire);
                    let mut art = worker_mail.artwork.lock().unwrap();
                    let ids = art
                        .drain(..)
                        .filter(|(epoch, _)| *epoch == n)
                        .map(|(_, id)| id)
                        .collect();
                    (n, Job::Artwork(ids))
                }
            };
            let comments = matches!(&job, Job::Comments(..));
            let silent = matches!(&job, Job::Channels(_, false) | Job::Artwork(_));
            if let Err(error) = run_job(
                job,
                n,
                &worker_mail,
                &weak,
                &worker_state,
                &worker_data,
                &worker_paths,
            ) {
                if !silent {
                    post(&weak, &worker_mail.epoch, n, move |w| {
                        let p = w.global::<BrowsePages>();
                        if comments {
                            p.set_comments_busy(false);
                            p.set_comments(reconcile_comments(p.get_comments(), Vec::new()));
                            p.set_comments_status(
                                format!("Could not load comments: {error:#}").into(),
                            );
                        } else {
                            p.set_home_has_more(false);
                            p.set_busy(false);
                            let rows = p.get_rows().iter().filter(|r| !r.loading).collect();
                            publish_rows(&w, rows);
                            p.set_status(format!("Could not load page: {error:#}").into());
                        }
                    });
                }
            }
        }
    });
    let weak = w.as_weak();
    let reflow_pending = std::rc::Rc::new(std::cell::Cell::new(false));
    w.global::<BrowsePages>().on_artwork_reflow(move || {
        if reflow_pending.replace(true) { return; }
        let pending = reflow_pending.clone();
        let weak = weak.clone();
        slint::Timer::single_shot(Duration::from_millis(16), move || {
            pending.set(false);
            if let Some(w) = weak.upgrade() {
                let p = w.global::<BrowsePages>();
                p.set_art_epoch(p.get_art_epoch().wrapping_add(1));
            }
        });
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    let previous = std::rc::Rc::new(std::cell::RefCell::new((u64::MAX,HashSet::<String>::new())));
    w.global::<BrowsePages>().on_artwork_window(move |first, end| {
        let Some(w) = weak.upgrade() else { return };
        let p = w.global::<BrowsePages>();
        let rows = p.get_rows();
        let ordered: Vec<_> = (first.max(0) as usize..(end.max(0) as usize).min(rows.row_count()))
            .filter_map(|i| rows.row_data(i)).filter(|r| !r.loading && !r.uri.is_empty())
            .map(|r| r.uri.to_string()).collect();
        let wanted: HashSet<_> = ordered.iter().cloned().collect();
        let epoch=mail.epoch.load(Ordering::Acquire);
        let (old_epoch,old)=previous.replace((epoch,wanted.clone()));
        mail.prune_art(&wanted);
        for id in &ordered {
            if old_epoch!=epoch || !old.contains(id) {p.invoke_artwork_visible(id.as_str().into(),0,true);}
        }
        mail.prioritize_art(&ordered);
        for id in old.difference(&wanted) {p.invoke_artwork_visible(id.as_str().into(),0,false);}

    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    let d = data.clone();
    w.global::<BrowsePages>()
        .on_artwork_visible(move |uri, role, visible| {
            let id = uri.to_string();
            if !crate::playlists::valid_youtube_id(&id) {
                return;
            }
            if let Some(w) = weak.upgrade() {
                let wanted = {
                    let mut data = d.lock().unwrap();
                    let roles = data.wanted_art.entry(id.clone()).or_default();
                    if visible {
                        roles.insert(role);
                    } else {
                        roles.remove(&role);
                    }
                    !roles.is_empty()
                };
                if !wanted {
                    if w.global::<BrowsePages>()
                        .get_rows()
                        .iter()
                        .any(|r| r.uri == uri && r.image.size().width > 0)
                    {
                        let weak = w.as_weak();
                        let data = d.clone();
                        slint::Timer::single_shot(Duration::ZERO, move || {
                            if let Some(w) = weak.upgrade() {
                                if !data
                                    .lock()
                                    .unwrap()
                                    .wanted_art
                                    .get(&id)
                                    .is_some_and(|roles| !roles.is_empty())
                                {
                                    apply_artwork(&w, &id, Default::default());
                                }
                            }
                        });
                    }
                } else {
                    let loaded = w
                        .global::<BrowsePages>()
                        .get_rows()
                        .iter()
                        .any(|r| r.uri == uri && r.image.size().width > 0);
                    if !loaded {
                        mail.request_art(id);
                    }
                }
            }
        });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    let d = data.clone();
    let state_for_home = state.clone();
    w.global::<BrowsePages>().on_home(move || {
        if let Some(w) = weak.upgrade() {
            let p = w.global::<BrowsePages>();
            interrupt_comments(&p);
            let refresh=p.get_page()==1;
            let saved=if refresh {None} else {d.lock().unwrap().home.take()};
            navigate(&w, &d, Location::Home);
            if let Some(saved)=saved {
                mail.cancel();
                let rows=saved.rows.iter().map(|r|r.0.row()).collect();
                {
                    let mut data=d.lock().unwrap();data.rows=saved.rows;data.feed=saved.feed;
                }
                p.set_busy(false);p.set_channel_hits(Default::default());
                p.set_channel_avatar(Default::default());p.set_channel_url("".into());p.set_description("".into());
                publish_rows(&w,rows);
                p.set_home_mode(saved.mode);p.set_home_has_more(saved.more);p.set_status(saved.status.into());
                p.set_page(1);p.set_title("".into());
                p.set_home_scroll(saved.scroll);
                p.set_home_reset(p.get_home_reset().wrapping_add(1));
                p.invoke_artwork_reflow();
                return;
            }
            p.set_page(1);
            p.set_title("".into());
            p.set_home_mode(0); p.set_home_has_more(false);
            p.set_home_scroll(0.);
            p.set_home_reset(p.get_home_reset().wrapping_add(1));
            p.set_description("".into());
            p.set_channel_url("".into());
            // A fresh profile has no Video seeds. Settle synchronously instead
            // of queueing an empty Home job behind playback/helper work.
            let empty_home = state_for_home.lock().unwrap().history[0].is_empty();
            p.set_busy(!empty_home);
            if empty_home {
                mail.cancel();
                let mut data = d.lock().unwrap();
                data.rows.clear(); data.feed = HomeFeed::default();
                drop(data);
                publish_rows(&w, Vec::new());
            } else {
                prime_home(&w);
            }
            p.set_status(
                d.lock()
                    .unwrap()
                    .error
                    .as_ref()
                    .map(|e| format!("Home preferences unreadable; original preserved: {e}"))
                    .unwrap_or_default()
                    .into(),
            );
            if !empty_home { mail.send(if refresh {Job::HomeNew} else {Job::Home}); }
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    w.global::<BrowsePages>().on_home_more(move || {
        if let Some(w) = weak.upgrade() {
            let p = w.global::<BrowsePages>();
            if p.get_page() == 1 && p.get_home_mode() == 0 && !p.get_busy() && p.get_home_has_more() {
                p.set_busy(true); mail.send(Job::HomeMore);
            }
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    w.global::<BrowsePages>().on_home_select(move |mode| {
        if let Some(w) = weak.upgrade() {
            let p = w.global::<BrowsePages>();
            if p.get_page() != 1 { return; }
            let history = mode == 1 && p.get_home_mode() != 1;
            p.set_home_mode(if history { 1 } else { 0 });
            p.set_home_has_more(false); p.set_busy(true); p.set_status("".into());
            p.set_home_scroll(0.);
            p.set_home_reset(p.get_home_reset().wrapping_add(1));
            prime_home(&w);
            mail.send(if history { Job::HomeHistory } else { Job::HomeNew });
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    let d = data.clone();
    w.global::<BrowsePages>().on_open_channel(move |input| {
        if let Some(w) = weak.upgrade() {
            let p = w.global::<BrowsePages>();
            interrupt_comments(&p);
            let location = navigate(
                &w,
                &d,
                Location::Channel {
                    input: input.chars().take(500).collect(),
                    tab: 0,
                    sort: 0,
                    filter: String::new(),
                    duration: 0,
                },
            );
            let Location::Channel {
                tab,
                sort,
                filter,
                duration,
                ..
            } = location
            else {
                return;
            };
            let uri = channel_url(&input);
            let known = uri.as_ref().and_then(|uri| {
                p.get_channel_hits()
                    .iter()
                    .find(|row| row.uri.as_str() == uri)
            });
            let same_channel = p.get_page() == 2
                && uri
                    .as_ref()
                    .is_some_and(|uri| p.get_channel_url().as_str() == uri);
            let known_title = if let Some(row) = &known {
                row.title.clone()
            } else if same_channel {
                p.get_title()
            } else {
                "".into()
            };
            let known_subscribers = if let Some(row) = &known {
                row.subtitle.clone()
            } else if same_channel {
                p.get_channel_subscribers()
            } else {
                "".into()
            };
            let known_image = if let Some(row) = &known {
                row.image.clone()
            } else if same_channel {
                p.get_channel_avatar()
            } else {
                Default::default()
            };
            p.set_sections(Default::default());
            p.set_page(2);
            p.set_channel_avatar(known_image);
            p.set_channel_subscribers(known_subscribers);
            p.set_channel_hits(Default::default());
            p.set_playlist_page(false);
            p.set_parent_channel("".into());
            p.set_description("".into());
            p.set_status("".into());
            p.set_busy(true);
            p.set_filter(filter.into());
            p.set_sort(sort);
            p.set_duration_filter(duration);
            p.set_tab(tab);
            if let Some(uri) = uri {
                p.set_saved_channel(
                    d.lock()
                        .unwrap()
                        .prefs
                        .channels
                        .iter()
                        .any(|c| c.uri == uri),
                );
                p.set_title(known_title);
                p.set_channel_url(uri.clone().into());
                prime_rows(&w, 30);
                mail.send(Job::Channel(uri, tab, sort));
            } else {
                p.set_title(format!("Channels for {input}").into());
                p.set_channel_url("".into());
                mail.send(Job::Channels(input.chars().take(200).collect(), true));
            }
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    w.global::<BrowsePages>().on_channel_refresh(move || {
        if let Some(w) = weak.upgrade() {
            let p = w.global::<BrowsePages>();
            p.set_busy(true);
            prime_rows(&w, 30);
            p.set_status("".into());
            p.set_filter("".into());
            mail.send(Job::Channel(
                p.get_channel_url().into(),
                p.get_tab(),
                p.get_sort(),
            ));
        }
    });
    let weak = w.as_weak();
    let d = data.clone();
    w.global::<BrowsePages>().on_filter_rows(move || {
        if let Some(w) = weak.upgrade() {
            let p = w.global::<BrowsePages>();
            let old = p.get_rows();
            let mut rows = visible_rows(
                &d.lock().unwrap().rows,
                &p.get_filter(),
                p.get_duration_filter(),
                0, // The server already returned the requested channel order.
            );
            for row in &mut rows {
                if let Some(old) = old.iter().find(|r| r.uri == row.uri) {
                    row.image = old.image;
                }
            }
            publish_rows(&w, rows);
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    let d = data.clone();
    w.global::<BrowsePages>().on_search_home(move |query| {
        if let Some(w) = weak.upgrade() {
            let p = w.global::<BrowsePages>();
            interrupt_comments(&p);
            p.set_sections(Default::default());
            p.set_channel_hits(Default::default());
            navigate(&w, &d, Location::Search(query.chars().take(200).collect()));
            p.set_page(3);
            prime_rows(&w, 20);
            p.set_title(
                format!(
                    "Results for {}",
                    query.trim().chars().take(200).collect::<String>()
                )
                .into(),
            );
            p.set_description("".into());
            p.set_status("".into());
            p.set_busy(true);
            p.set_parent_channel("".into());
            mail.send(Job::Search(query.chars().take(200).collect()));
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    w.global::<BrowsePages>().on_search_channels(move |q| {
        if let Some(w) = weak.upgrade() {
            w.global::<BrowsePages>()
                .set_channel_hits(Default::default());
            let Some(query) = channel_search_query(&q) else {
                return;
            };
            mail.send(Job::Channels(query.chars().take(200).collect(), false));
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    let st = state.clone();
    let d = data.clone();
    w.global::<BrowsePages>().on_activate(move |uri, kind| {
        if let Some(w) = weak.upgrade() {
            let p = w.global::<BrowsePages>();
            match kind {
                3 => p.invoke_open_channel(uri),
                1 => w.invoke_notify("Channel playlists are not supported.".into()),
                2 => {
                    navigate(&w, &d, Location::Player);
                    mail.cancel();
                    p.set_sections(Default::default());
                    p.set_page(0);
                    p.set_rows(Default::default());
                    w.invoke_playlist_select(uri);
                    w.set_left_pane_hovered(true);
                }
                _ => {
                    let video = d
                        .lock()
                        .unwrap()
                        .rows
                        .iter()
                        .find(|(r, _)| r.uri == uri.as_str())
                        .and_then(|(_, v)| v.clone());
                    if let Some(video) = video {
                        navigate(&w, &d, Location::Player);
                        mail.cancel();
                        p.set_sections(Default::default());
                        p.set_page(0);
                        p.set_rows(Default::default());
                        play_queue::play(&w, &st, video);
                    }
                }
            }
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    let d = data.clone();
    w.global::<BrowsePages>().on_close_page(move || {
        if let Some(w) = weak.upgrade() {
            navigate(&w, &d, Location::Player);
            mail.cancel();
            let p = w.global::<BrowsePages>();
            p.set_sections(Default::default());
            p.set_channel_avatar(Default::default());
            p.set_channel_hits(Default::default());
            p.set_page(0);
            p.set_busy(false);
            interrupt_comments(&p);
            p.set_rows(Default::default());
        }
    });
    let weak = w.as_weak();
    let d = data.clone();
    w.global::<BrowsePages>().on_back(move || {
        if let Some(w) = weak.upgrade() {
            let p=w.global::<BrowsePages>();
            if p.get_page()==2 {
                d.lock().unwrap().navigation.restore=None;
                p.invoke_home();
            }
        }
    });
    let weak = w.as_weak();
    let d = data.clone();
    let p = paths.clone();
    w.global::<BrowsePages>().on_configure_home(move || {
        if let Some(w) = weak.upgrade() {
            let ui = w.global::<BrowsePages>();
            let mut data = d.lock().unwrap();
            data.prefs.recent = ui.get_show_recent();
            data.prefs.playlists = ui.get_show_playlists();
            let result = save_preferences(&p, &data);
            drop(data);
            if let Err(e) = result {
                w.invoke_notify(format!("Could not save home: {e}").into());
            }
            ui.invoke_home();
        }
    });
    let weak = w.as_weak();
    let d = data.clone();
    let p = paths.clone();
    w.global::<BrowsePages>().on_save_channel(move || {
        if let Some(w) = weak.upgrade() {
            let ui = w.global::<BrowsePages>();
            let Some(uri) = channel_url(&ui.get_channel_url()) else {
                return;
            };
            let mut data = d.lock().unwrap();
            let mut next = data.prefs.clone();
            if let Some(index) = next.channels.iter().position(|c| c.uri == uri) {
                next.channels.remove(index);
            } else if next.channels.len() < SAVED_LIMIT {
                next.channels.push(Channel {
                    title: ui.get_title().chars().take(200).collect(),
                    subtitle: "Saved locally".into(),
                    uri: uri.clone(),
                    avatar: String::new(),
                });
            } else {
                w.invoke_notify("Home holds up to 30 saved channels.".into());
                return;
            }
            let old = std::mem::replace(&mut data.prefs, next);
            if let Err(e) = save_preferences(&p, &data) {
                data.prefs = old;
                w.invoke_notify(format!("Could not save channel: {e}").into());
            }
            ui.set_saved_channel(data.prefs.channels.iter().any(|c| c.uri == uri));
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    w.global::<BrowsePages>().on_load_comments(move || {
        if let Some(w) = weak.upgrade() {
            if !w.global::<BrowsePages>().get_comments_open() { return; }
            let id = w.get_selected_id();
            if crate::playlists::youtube_id_from_uri(&format!("https://youtu.be/{id}")).is_none() {
                return;
            }
            let p = w.global::<BrowsePages>();
            p.set_comments_busy(true);
            p.set_comments_status("Loading public comments…".into());
            p.set_comments(reconcile_comments(
                p.get_comments(),
                (0..8)
                    .map(|_| CommentRow {
                        loading: true,
                        ..Default::default()
                    })
                    .collect(),
            ));
            p.set_comments_loaded(false);
            mail.send(Job::Comments(id.into(), p.get_comments_order() == 1));
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    w.global::<BrowsePages>().on_toggle_comments(move || {
        let Some(w) = weak.upgrade() else { return; };
        let p = w.global::<BrowsePages>();
        let active = !p.get_comments_open();
        p.set_comments_open(active);
        if active {
            if !p.get_comments_loaded() && !p.get_comments_busy() { p.invoke_load_comments(); }
        } else if p.get_comments_busy() {
            mail.cancel();
            interrupt_comments(&p);
        }
    });
    let weak = w.as_weak();
    let mail = mailbox.clone();
    w.global::<BrowsePages>().on_reset_comments(move || {
        mail.cancel();
        if let Some(w) = weak.upgrade() {
            let p = w.global::<BrowsePages>();
            p.set_comments(Default::default());
            p.set_comments_open(false);
            p.set_comments_busy(false);
            p.set_comments_loaded(false);
            p.set_comments_status("".into());
        }
    });
    Guard {
        mailbox,
        thread: Some(thread),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_only_allow_youtube_channels() {
        assert_eq!(
            channel_url("https://youtube.com/@duck/videos?sort=p").as_deref(),
            Some("https://www.youtube.com/@duck")
        );
        assert!(channel_url("https://youtube.com.evil.test/@duck").is_none());
        assert!(channel_url("https://youtube.com/watch?v=abcdefghijk").is_none());
        assert!(channel_url("https://youtube.com/@duck/search").is_none());
        assert!(playlist_url("https://www.youtube.com/playlist?list=PL123").is_some());
        assert!(playlist_url("https://evil.test/playlist?list=PL123").is_none());
    }
    #[test]
    fn comments_are_bounded_and_plain_text() {
        let info = serde_json::json!({"comments":(0..100).map(|_|serde_json::json!({"author":"A","text":"界".repeat(3000),"like_count":0})).collect::<Vec<_>>()});
        let rows = comments(&info);
        assert_eq!(rows.len(), 40);
        assert_eq!(rows[0].body.chars().count(), 2000);
        assert!(comments(&serde_json::json!({})).is_empty());
    }
    #[test]
    fn channel_results_deduplicate_and_cap_three() {
        let rows=(0..8).map(|n|serde_json::json!({"channelRenderer":{"channelId":format!("UC{n:022}"),"title":{"simpleText":"Creator"}}})).collect::<Vec<_>>();
        let mut found = Vec::new();
        collect_channels(&Value::Array(rows), &mut found, 0);
        assert_eq!(found.len(), 3);
    }
    #[test]
    fn local_filters_keep_unknown_dates_last() {
        let rows = vec![
            (
                PageRowData {
                    title: "Old".into(),
                    ..Default::default()
                },
                Some(Video {
                    published: Some(1),
                    duration: 120.,
                    ..Default::default()
                }),
            ),
            (
                PageRowData {
                    title: "New".into(),
                    ..Default::default()
                },
                Some(Video {
                    published: Some(2),
                    duration: 900.,
                    ..Default::default()
                }),
            ),
            (
                PageRowData {
                    title: "Unknown".into(),
                    ..Default::default()
                },
                Some(Video::default()),
            ),
        ];
        assert_eq!(visible_rows(&rows, "", 0, 1)[0].title, "New");
        assert_eq!(visible_rows(&rows, "", 0, 2)[2].title, "Unknown");
        assert_eq!(visible_rows(&rows, "", 1, 0).len(), 1);
    }
    #[test]
    fn back_restores_routes_without_creating_a_navigation_loop() {
        let mut nav = Navigation::default();
        nav.enter(Location::Home);
        nav.enter(Location::Search("asmongold".into()));
        let channel = Location::Channel {
            input: "https://www.youtube.com/@AsmongoldTV".into(),
            tab: 1,
            sort: 3,
            filter: "test".into(),
            duration: 1,
        };
        nav.enter(channel.clone());
        assert_eq!(nav.back(), Some(Location::Search("asmongold".into())));
        nav.enter(Location::Search("ignored restore placeholder".into()));
        assert_eq!(nav.current, Location::Search("asmongold".into()));
        assert_eq!(nav.previous.len(), 2);
        nav.back();
        nav.enter(Location::Home);
        assert_eq!(nav.current, Location::Home);
        nav.back();
        nav.enter(Location::Player);
        assert!(nav.previous.is_empty());
        assert!(nav.back().is_none());
        nav.enter(channel.clone());
        nav.enter(Location::Home);
        nav.back();
        assert_eq!(nav.enter(Location::Player), channel);
        for i in 0..100 {
            nav.enter(Location::Search(i.to_string()));
        }
        assert_eq!(nav.previous.len(), 16);
    }
    #[test]
    fn flat_channel_cards_inherit_the_opened_channel_identity() {
        let info = serde_json::json!({"entries":[
            {"id":"abcdefghijk","title":"Flat result","view_count":441000,"duration":120},
            {"id":"lmnopqrstuv","title":"Explicit author","channel":"Collaborator"}
        ]});
        let rows = channel_video_rows(&info, "Asmongold TV");
        assert_eq!(rows[0].0.subtitle, "Asmongold TV");
        assert_eq!(rows[0].1.as_ref().unwrap().channel, "Asmongold TV");
        assert_eq!(rows[0].0.stats, "441.0K views · 2:00");
        assert_eq!(rows[1].0.subtitle, "Collaborator");
    }
    #[test]
    fn channel_sorts_use_the_matching_server_token() {
        let v = serde_json::json!({"chips":[
            {"chipViewModel":{"text":"Latest","tapCommand":{"innertubeCommand":{"continuationCommand":{"token":"latest-feed"}}}}},
            {"chipViewModel":{"text":"Popular","tapCommand":{"innertubeCommand":{"continuationCommand":{"token":"popular-feed"}}}}},
            {"chipCloudChipRenderer":{"text":{"simpleText":"Oldest"},"navigationEndpoint":{"continuationCommand":{"token":"oldest-feed"}}}}
        ]});
        assert_eq!(channel_sort_token(&v, 1, 0).as_deref(), Some("latest-feed"));
        assert_eq!(channel_sort_token(&v, 2, 0).as_deref(), Some("oldest-feed"));
        assert_eq!(
            channel_sort_token(&v, 3, 0).as_deref(),
            Some("popular-feed")
        );
        assert!(channel_sort_token(&Value::Null, 3, 0).is_none());
    }
    #[test]
    fn comments_fill_the_existing_skeleton_model() {
        let old = ModelRc::new(VecModel::from(vec![
            CommentRow {
                loading: true,
                ..Default::default()
            };
            8
        ]));
        let next = reconcile_comments(
            old.clone(),
            vec![CommentRow {
                author: "Viewer".into(),
                body: "Text".into(),
                ..Default::default()
            }],
        );
        assert!(std::ptr::eq(old.as_any(), next.as_any()));
        assert_eq!(old.row_count(), 1);
        assert_eq!(old.row_data(0).unwrap().body, "Text");
        assert!(!old.row_data(0).unwrap().loading);
    }
    #[test]
    #[ignore = "Public YouTube network probe; run explicitly"]
    fn channel_sort_live() {
        let epoch = Arc::new(AtomicU64::new(0));
        let mut first = Vec::new();
        for sort in [1, 2, 3] {
            let info =
                sorted_channel("https://www.youtube.com/@YouTube/videos", sort, &epoch, 0).unwrap();
            let rows = info["entries"].as_array().unwrap();
            assert!(!rows.is_empty() && rows.len() <= 30);
            let id = rows[0]["id"].as_str().unwrap().to_owned();
            assert!(rows[0]["view_count"].as_u64().is_some());
            println!("channel sort {sort}: {} videos, first {id}", rows.len());
            first.push(id);
        }
        assert_ne!(first[0], first[1]);
        assert_ne!(first[0], first[2]);
        let shorts =
            sorted_channel("https://www.youtube.com/@YouTube/shorts", 3, &epoch, 0).unwrap();
        assert!(!shorts["entries"].as_array().unwrap().is_empty());
    }
    #[test]
    fn home_batches_and_history_are_bounded_without_personal_data() {
        assert!(recommendations::history_seeds(&[], false).is_empty());
        assert!(history_rows(&[]).is_empty());
        assert_eq!(home_target(0), 20);
        assert_eq!(home_target(110), 120);
        assert_eq!(home_target(usize::MAX), 120);
        let history: Vec<_> = (0..200).map(|i| Video { id: format!("{i:011}"), ..Default::default() }).collect();
        let rows = history_rows(&history);
        assert_eq!(rows.len(), HOME_LIMIT);
        assert_eq!(rows[0].0.uri, history[0].id);
        assert_eq!(rows[119].0.uri, history[119].id);
    }
    #[test]
    fn home_pending_and_duplicate_memory_are_bounded() {
        let candidates = || (0..200).map(|i| Video { id: format!("{i:011}"), ..Default::default() }).collect();
        let mut feed = HomeFeed::default();
        feed.accept(candidates());
        assert_eq!(feed.pending.len(), 40);
        assert_eq!(feed.seen.len(), 40);
        feed.pending.clear();
        feed.accept(candidates());
        assert_eq!(feed.pending.len(), 40);
        assert!(feed.pending.iter().all(|v| v.id.parse::<usize>().unwrap() >= 40));
        feed.pending.clear();
        feed.seen = (0..240).map(|i| format!("{i:011}")).collect();
        feed.accept(candidates());
        assert!(feed.pending.is_empty());
        assert_eq!(feed.seen.len(), 240);
    }
    #[test]
    fn home_cards_keep_metadata_below_channel() {
        let v = Video { id: "abcdefghijk".into(), channel: "Creator".into(), views: Some(1234), duration: 250., ..Default::default() };
        let row = page_video(&v, 0).0.row();
        assert_eq!(row.subtitle, "Creator");
        assert_eq!(row.stats, "1.2K views · 4:10");
        assert_eq!(row.duration, "4:10");
    }
    #[test]
    fn home_search_is_twenty_without_changing_player_search() {
        assert_eq!(home_search_target("ducks").unwrap(), "ytsearch20:ducks");
        assert_eq!(search_target("ducks").unwrap(), "ytsearch30:ducks");
        assert_eq!(
            home_search_target("https://youtu.be/abcdefghijk").unwrap(),
            "https://www.youtube.com/watch?v=abcdefghijk"
        );
        assert!(home_search_target("https://example.com").is_err());
    }
    #[test]
    fn channel_search_requires_explicit_prefix_and_avatar_urls_are_bounded() {
        assert_eq!(channel_search_query(" @Mochune "), Some("Mochune"));
        for query in ["Mochune", "email@Mochune", "@", "  "] {
            assert!(channel_search_query(query).is_none());
        }
        assert_eq!(
            safe_avatar_url("https://yt3.ggpht.com/portrait=s900-c-k-c0x00ffffff-no-rj").unwrap(),
            "https://yt3.ggpht.com/portrait=s128-c-k-c0x00ffffff-no-rj"
        );
        for url in [
            "http://yt3.ggpht.com/a",
            "https://evil.test/a",
            "https://user@yt3.ggpht.com/a",
            "https://yt3.ggpht.com:443/a",
        ] {
            // An explicit default HTTPS port is normalized safely by url::Url.
            if !url.ends_with(":443/a") {
                assert!(safe_avatar_url(url).is_none());
            }
        }
        assert!(channel_url("https://youtube.com/@duck/streams").is_none());
        assert!(channel_url("https://youtube.com/@duck/playlists").is_none());
    }
    #[test]
    fn channel_profile_uses_portrait_not_banner_and_only_known_subscribers() {
        let info = serde_json::json!({"channel":"Duck", "channel_follower_count":12345, "thumbnails":[
            {"url":"https://yt3.ggpht.com/banner=w2000", "width":2000,"height":330},
            {"id":"avatar_uncropped","url":"https://yt3.ggpht.com/face=s900-c", "width":900,"height":900}
        ]});
        let profile = channel_profile(&info, "https://www.youtube.com/@duck");
        assert_eq!(profile.avatar, "https://yt3.ggpht.com/face=s128-c");
        assert_eq!(profile.subtitle, "12.3K subscribers");
        assert!(channel_profile(&Value::Null, "").subtitle.is_empty());
        assert_eq!(
            header_subscribers(
                &serde_json::json!({"content":"1.2M subscribers • 56 videos"}),
                0
            )
            .as_deref(),
            Some("1.2M subscribers")
        );
        assert!(
            header_subscribers(&serde_json::json!({"content":"We love our subscribers"}), 0)
                .is_none()
        );
    }
    #[test]
    fn append_preserves_inflight_artwork_and_new_pages_cancel_it() {
        let mailbox = Arc::new(Mailbox {
            pending: Mutex::new(None), artwork: Mutex::new(VecDeque::new()),
            art_requests: Mutex::new(HashSet::new()), ready: Condvar::new(),
            epoch: Arc::new(AtomicU64::new(7)), stopped: AtomicBool::new(false),
        });
        mailbox.request_art("visible".into());
        mailbox.artwork.lock().unwrap().clear(); // Simulate the worker taking this job.
        let claim = ArtworkClaim { mailbox: mailbox.clone(), key: (7, "visible".into()) };
        mailbox.request_art("visible".into());
        assert!(mailbox.artwork.lock().unwrap().is_empty());
        mailbox.send(Job::HomeMore);
        assert_eq!(mailbox.epoch.load(Ordering::Acquire), 7);
        assert!(mailbox.art_requests.lock().unwrap().contains(&(7, "visible".into())));
        mailbox.request_art("obsolete".into());
        mailbox.prune_art(&HashSet::from(["visible".into()]));
        assert!(mailbox.artwork.lock().unwrap().is_empty());
        assert!(!mailbox.art_requests.lock().unwrap().contains(&(7, "obsolete".into())));
        drop(claim);
        mailbox.request_art("visible".into());
        mailbox.request_art("above".into());
        mailbox.prioritize_art(&["above".into(), "visible".into()]);
        assert_eq!(mailbox.artwork.lock().unwrap()[0].1, "above");
        mailbox.send(Job::HomeNew);
        assert_eq!(mailbox.epoch.load(Ordering::Acquire), 8);
        assert!(mailbox.art_requests.lock().unwrap().is_empty());
        assert!(mailbox.artwork.lock().unwrap().is_empty());
    }
    #[test]
    fn placeholders_are_reused_as_data_and_art_queue_is_bounded_cancelled() {
        let old = ModelRc::new(VecModel::from(placeholders(0, 20)));
        let identity = old.as_any() as *const dyn std::any::Any as *const ();
        let rows = reconcile_rows(
            old,
            vec![
                PageRow {
                    title: "Ready".into(),
                    uri: "abcdefghijk".into(),
                    ..Default::default()
                };
                20
            ],
        );
        assert_eq!(
            rows.as_any() as *const dyn std::any::Any as *const (),
            identity
        );
        assert!(!rows.row_data(0).unwrap().loading);
        let mailbox = Mailbox {
            pending: Mutex::new(None),
            artwork: Mutex::new(VecDeque::new()),
        art_requests: Mutex::new(HashSet::new()),
            ready: Condvar::new(),
            epoch: Arc::new(AtomicU64::new(0)),
            stopped: AtomicBool::new(false),
        };
        for i in 0..100 {
            mailbox.request_art(format!("{i}"));
            mailbox.request_art(format!("{i}"));
        }
        assert_eq!(mailbox.artwork.lock().unwrap().len(), 48);
        mailbox.send(Job::Home);
        assert!(mailbox.artwork.lock().unwrap().is_empty());
        mailbox.request_art("next".into());
        mailbox.cancel();
        assert!(mailbox.artwork.lock().unwrap().is_empty());
    }
}
