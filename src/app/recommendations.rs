//! Bounded public watch-page suggestions. YouTube's page schema can change.
use super::{Settings, Video, metadata_agent, published};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::HashSet;
const PAGE_LIMIT: u64 = 4 * 1024 * 1024;
pub const MAX_RESULTS: usize = 50;
pub const VIDEO_LIMITS: [i32; 4] = [25, 30, 40, 50];
pub fn video_limit_choice(limit: i32) -> i32 {
    VIDEO_LIMITS.iter().position(|v| *v == limit).unwrap_or(1) as i32
}
pub fn video_limit(choice: i32) -> i32 {
    VIDEO_LIMITS[choice.clamp(0, 3) as usize]
}
const MAX_CANDIDATES: usize = 200;
pub fn count(s: &Settings) -> usize {
    if s.music {
        40
    } else {
        video_limit(video_limit_choice(s.recommendation_count)) as usize
    }
}
fn text(v: &Value) -> String {
    if let Some(s) = v
        .as_str()
        .or(v["simpleText"].as_str())
        .or(v["content"].as_str())
    {
        return s.chars().take(4096).collect();
    }
    v["runs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r["text"].as_str())
                .flat_map(str::chars)
                .take(4096)
                .collect()
        })
        .unwrap_or_default()
}

fn duration(s: &str) -> f64 {
    s.split(':')
        .try_fold(0u64, |n, p| {
            p.trim()
                .parse::<u64>()
                .ok()
                .and_then(|p| n.checked_mul(60)?.checked_add(p))
        })
        .unwrap_or(0) as f64
}
fn views(s: &str) -> Option<u64> {
    let s = s.replace(',', "").replace(' ', "");
    let n: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let n = n.parse::<f64>().ok()?;
    let multiplier = match s
        .get(s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len())..)
        .unwrap_or("")
        .chars()
        .next()
    {
        Some('K' | 'k') => 1e3,
        Some('M' | 'm') => 1e6,
        Some('B' | 'b') => 1e9,
        _ => 1.,
    };
    Some((n * multiplier) as u64)
}
pub(super) fn json_after(page: &str, marker: &str) -> Option<Value> {
    // A script can mention ytcfg.set before the actual JSON invocation.
    page.match_indices(marker).take(32).find_map(|(index, _)| {
        let tail = page[index + marker.len()..].trim_start();
        serde_json::Deserializer::from_str(tail)
            .into_iter::<Value>()
            .next()?
            .ok()
    })
}

fn push_candidate(out: &mut Vec<Video>, video: Video) {
    if !video.id.is_empty() && !out.iter().any(|v| v.id == video.id) {
        out.push(video);
    }
}
fn collect(node: &Value, out: &mut Vec<Video>, token: &mut Option<String>, depth: usize) {
    if depth > 40 {
        return;
    }
    if let Some(v) = node
        .get("compactVideoRenderer")
        .or_else(|| node.get("videoRenderer"))
        .or_else(|| node.get("gridVideoRenderer"))
    {
        if out.len() >= MAX_CANDIDATES {
            return;
        }
        push_candidate(
            out,
            Video {
                id: text(&v["videoId"]),
                title: text(&v["title"]),
                channel: text(&v["shortBylineText"]),
                duration: duration(&text(&v["lengthText"])),
                views: views(&text(&v["viewCountText"])),
                published: published::relative(&text(&v["publishedTimeText"])),
                unavailable: if v["isPlayable"] == false {
                    "Unavailable".into()
                } else {
                    String::new()
                },
                ..Video::default()
            },
        );
        return;
    }
    if let Some(v) = node.get("lockupViewModel") {
        if v["contentType"] == "LOCKUP_CONTENT_TYPE_VIDEO" && out.len() < MAX_CANDIDATES {
            let m = &v["metadata"]["lockupMetadataViewModel"];
            let rows = &m["metadata"]["contentMetadataViewModel"]["metadataRows"];
            let time = &v["contentImage"]["thumbnailViewModel"]["overlays"][0]["thumbnailBottomOverlayViewModel"]
                ["badges"][0]["thumbnailBadgeViewModel"]["text"];
            push_candidate(
                out,
                Video {
                    id: text(&v["contentId"]),
                    title: text(&m["title"]),
                    channel: text(&rows[0]["metadataParts"][0]["text"]),
                    duration: duration(&text(time)),
                    views: rows
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|r| r["metadataParts"].as_array().into_iter().flatten())
                        .find(|p| {
                            text(&p["text"]).contains("view")
                                || text(&p["accessibilityLabel"]).contains("view")
                                || p["leadingIcon"]["name"] == "PLAY_ARROW_OUTLINED"
                        })
                        .and_then(|p| views(&text(&p["text"]))),
                    published: rows
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|r| r["metadataParts"].as_array().into_iter().flatten())
                        .find_map(|p| {
                            published::relative(&text(&p["accessibilityLabel"]))
                                .or_else(|| published::relative(&text(&p["text"])))
                        }),
                    ..Video::default()
                },
            );
        }
        return;
    }
    if let Some(t) = node
        .pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
        .and_then(Value::as_str)
    {
        if t.len() <= 16384 {
            *token = Some(t.to_owned());
        }
        return;
    }
    match node {
        Value::Object(m) => {
            for (key, v) in m {
                // Skip ads, Shorts shelves, action menus and unrelated page furniture.
                if !matches!(
                    key.as_str(),
                    "reelShelfRenderer"
                        | "adSlotRenderer"
                        | "menu"
                        | "menuButton"
                        | "onTap"
                        | "rendererContext"
                ) {
                    collect(v, out, token, depth + 1);
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                collect(v, out, token, depth + 1);
            }
        }
        _ => {}
    }
}
// Shorts are accepted only in explicit channel browsing, never watch suggestions.
fn collect_channel_shorts(node: &Value, out: &mut Vec<Video>, depth: usize) {
    if depth > 40 || out.len() >= 30 {
        return;
    }
    if let Some(v) = node.get("shortsLockupViewModel") {
        push_candidate(
            out,
            Video {
                id: text(&v["onTap"]["innertubeCommand"]["reelWatchEndpoint"]["videoId"]),
                title: text(&v["overlayMetadata"]["primaryText"]),
                views: views(&text(&v["overlayMetadata"]["secondaryText"])),
                ..Default::default()
            },
        );
        return;
    }
    if let Some(v) = node.get("reelItemRenderer") {
        push_candidate(
            out,
            Video {
                id: text(&v["videoId"]),
                title: text(&v["headline"]),
                views: views(&text(&v["viewCountText"])),
                ..Default::default()
            },
        );
        return;
    }
    match node {
        Value::Object(m) => {
            for (key, v) in m {
                if !matches!(key.as_str(), "menu" | "onTap" | "rendererContext") {
                    collect_channel_shorts(v, out, depth + 1);
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                collect_channel_shorts(v, out, depth + 1);
            }
        }
        _ => {}
    }
}
pub(super) fn browse_candidates(node: &Value) -> (Vec<Video>, Option<String>) {
    let mut videos = Vec::new();
    let mut token = None;
    collect(node, &mut videos, &mut token, 0);
    collect_channel_shorts(node, &mut videos, 0);
    videos.retain(|v| {
        !v.title.is_empty()
            && crate::playlists::youtube_id_from_uri(&format!("https://youtu.be/{}", v.id))
                .is_some()
    });
    videos.truncate(30);
    (videos, token)
}
pub struct Suggestions {
    pub videos: Vec<Video>,
    pub music: Option<bool>,
    pub classified: bool,
}
/// Compact continuation state for the explicitly bounded Home feed. No media URLs.
#[derive(Clone)]
pub(super) struct HomeCursor {
    token: String,
    context: Value,
    music: Option<bool>,
    classified: bool,
    visited: Vec<u64>,
}
fn continuation_key(token: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut hash);
    hash.finish()
}
pub(super) fn home_page(
    id: &str,
    cursor: Option<HomeCursor>,
) -> Result<(Suggestions, Option<HomeCursor>)> {
    let continuation = cursor.is_some();
    let started = std::time::Instant::now();
    let result = request_page(id, cursor);
    crate::diagnostics::record(
        "recommendation_page",
        serde_json::json!({
            "seed":id,"continuation":continuation,"elapsed_ms":started.elapsed().as_millis(),
            "rows":result.as_ref().ok().map(|(p,_)|p.videos.len()),
            "has_more":result.as_ref().ok().map(|(_,c)|c.is_some()),
            "error":result.as_ref().err().map(|e|format!("{e:#}"))
        }),
    );
    result
}
pub(super) fn request_blocked(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ureq::Error>(),
            Some(ureq::Error::StatusCode(403 | 429))
        )
    })
}
fn request_page(id: &str, cursor: Option<HomeCursor>) -> Result<(Suggestions, Option<HomeCursor>)> {
    let (node, context, music, classified, visited) = if let Some(cursor) = cursor {
        let mut visited = cursor.visited;
        let key = continuation_key(&cursor.token);
        ensure!(
            visited.len() < 9 && !visited.contains(&key),
            "Repeated or exhausted suggestion continuation"
        );
        visited.push(key);
        let mut response = metadata_agent()
            .post("https://www.youtube.com/youtubei/v1/next")
            .send_json(serde_json::json!({"context":cursor.context,"continuation":cursor.token}))?;
        let body = response
            .body_mut()
            .with_config()
            .limit(PAGE_LIMIT)
            .read_to_vec()?;
        (
            serde_json::from_slice::<Value>(&body)?,
            Some(cursor.context),
            cursor.music,
            cursor.classified,
            visited,
        )
    } else {
        ensure!(
            crate::playlists::youtube_id_from_uri(&format!("https://youtu.be/{id}")).as_deref()
                == Some(id),
            "Invalid video identity"
        );
        let mut response = metadata_agent()
            .get(format!("https://www.youtube.com/watch?v={id}&hl=en"))
            .header("Accept-Language", "en-US,en;q=0.8")
            .call()?;
        let body = response
            .body_mut()
            .with_config()
            .limit(PAGE_LIMIT)
            .read_to_vec()?;
        let page = std::str::from_utf8(&body)?;
        let mut initial = json_after(page, "var ytInitialData = ")
            .or_else(|| json_after(page, "ytInitialData = "))
            .context("YouTube suggestions are unavailable")?;
        // Move the sidebar out instead of duplicating the parsed watch page.
        let sidebar = initial
            .pointer_mut("/contents/twoColumnWatchNextResults/secondaryResults")
            .map(Value::take)
            .context("YouTube returned no suggestions")?;
        let category = json_after(page, "var ytInitialPlayerResponse = ")
            .or_else(|| json_after(page, "ytInitialPlayerResponse = "))
            .and_then(|v| {
                v.pointer("/microformat/playerMicroformatRenderer/category")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        let context = json_after(page, "ytcfg.set(")
            .and_then(|v| v.get("INNERTUBE_CONTEXT").cloned())
            .filter(|v| v.to_string().len() <= 65536);
        (
            sidebar,
            context,
            category.as_deref().and_then(category_music),
            category.is_some(),
            Vec::new(),
        )
    };
    let mut videos = Vec::new();
    let mut token = None;
    collect(&node, &mut videos, &mut token, 0);
    let next = token
        .filter(|t| visited.len() < 9 && !visited.contains(&continuation_key(t)))
        .zip(context)
        .map(|(token, context)| HomeCursor {
            token,
            context,
            music,
            classified,
            visited,
        });
    videos.retain(|v| {
        v.id != id
            && !v.title.is_empty()
            && crate::playlists::youtube_id_from_uri(&format!("https://youtu.be/{}", v.id))
                .as_deref()
                == Some(v.id.as_str())
    });
    Ok((
        Suggestions {
            videos,
            music,
            classified,
        },
        next,
    ))
}
pub fn watch_suggestions(id: &str, continuation: bool) -> Result<Suggestions> {
    let mut page = watch_pages(
        id,
        if continuation { 2 } else { 0 },
        || true,
        |videos, _, _| videos.len() >= MAX_RESULTS,
    )?;
    page.videos.truncate(MAX_RESULTS);
    Ok(page)
}
pub(super) fn filtered_suggestions(
    id: &str,
    s: &Settings,
    history: &[Video],
    seed: &Video,
    target: usize,
    generation: &std::sync::atomic::AtomicU64,
    epoch: u64,
) -> Result<Suggestions> {
    watch_pages(
        id,
        if target <= 8 { 2 } else { 9 },
        || super::wait_for_playback(Some((generation, epoch))),
        |videos, music, classified| {
            // Retain eligible metadata only: rejected rows must not consume the
            // bounded candidate pool and stop paging before the requested count.
            *videos = filter_page(std::mem::take(videos), s, history, seed, music, classified);
            videos.len() >= target.min(count(s))
        },
    )
}
// Count the rows that can actually be displayed, using the same category and
// relevance filters as the caller. Raw page length is not a completion signal.
fn filter_page(
    videos: Vec<Video>,
    s: &Settings,
    history: &[Video],
    seed: &Video,
    music: Option<bool>,
    classified: bool,
) -> Vec<Video> {
    let mut seed = seed.clone();
    if classified {
        seed.music = music;
    }
    filter(videos, s, history, Some(&seed))
}
#[cfg(test)]
fn enough_filtered(
    videos: &[Video],
    s: &Settings,
    history: &[Video],
    seed: &Video,
    target: usize,
    music: Option<bool>,
    classified: bool,
) -> bool {
    filter_page(videos.to_vec(), s, history, seed, music, classified).len() >= target.min(count(s))
}

fn watch_pages(
    id: &str,
    extra_pages: usize,
    ready: impl Fn() -> bool,
    enough: impl Fn(&mut Vec<Video>, Option<bool>, bool) -> bool,
) -> Result<Suggestions> {
    let mut out = Vec::with_capacity(MAX_RESULTS);
    let mut cursor = None;
    let mut music = None;
    let mut classified = false;
    for index in 0..=extra_pages.min(9) {
        ensure!(ready(), "Request cancelled");
        let page = home_page(id, cursor.take());
        let (page, next) = match page {
            Ok(page) => page,
            Err(error) if index == 0 => return Err(error),
            Err(_) => break, // Logged by home_page; never retry a failed page.
        };
        music = page.music;
        classified = page.classified;
        for row in page.videos {
            if out.len() >= MAX_CANDIDATES {
                break;
            }
            push_candidate(&mut out, row);
        }
        cursor = next;
        if enough(&mut out, music, classified) || out.len() >= MAX_CANDIDATES || cursor.is_none() {
            break;
        }
    }
    ensure!(!out.is_empty(), "YouTube returned no playable suggestions");
    Ok(Suggestions {
        videos: out,
        music,
        classified,
    })
}

fn category_music(category: &str) -> Option<bool> {
    match category.trim().to_ascii_lowercase().as_str() {
        "music" => Some(true),
        "" | "entertainment" | "people & blogs" => None,
        _ => Some(false),
    }
}
/// Broad entertainment categories do not prove that a video is nonmusic.
pub fn music_metadata(info: &Value) -> Option<bool> {
    if let Some(categories) = info["categories"].as_array().filter(|a| !a.is_empty()) {
        if categories
            .iter()
            .any(|v| v.as_str().and_then(category_music) == Some(true))
        {
            return Some(true);
        }
        if categories
            .iter()
            .any(|v| v.as_str().and_then(category_music) == Some(false))
        {
            return Some(false);
        }
    }
    let named = |key: &str| info[key].as_str().is_some_and(|v| !v.trim().is_empty());
    (named("track")
        && (named("artist") || info["artists"].as_array().is_some_and(|a| !a.is_empty())))
    .then_some(true)
}
pub fn music_hint(video: &Video) -> bool {
    if let Some(known) = video.music {
        return known;
    }
    let channel = video.channel.to_lowercase();
    let title = video.title.to_lowercase();
    let words: Vec<_> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    channel.ends_with(" - topic")
        || channel.ends_with("vevo")
        || title.contains(" - ")
        || title.contains(" – ")
        || title.contains(" — ")
        || [
            "lyrics",
            "lyric video",
            "official music",
            "music video",
            "official audio",
            "remix",
            "cover",
            "acoustic",
            "instrumental",
            "soundtrack",
            "visualizer",
        ]
        .iter()
        .any(|term| {
            let term: Vec<_> = term.split_whitespace().collect();
            words
                .windows(term.len())
                .any(|window| window == term.as_slice())
        })
}
pub fn history_seeds(history: &[Video], music: bool) -> Vec<Video> {
    let mut seen = HashSet::new();
    history
        .iter()
        .filter(|v| {
            v.playable(music)
                && crate::playlists::youtube_id_from_uri(&format!("https://youtu.be/{}", v.id))
                    .is_some()
                && seen.insert(v.id.clone())
        })
        .take(10)
        .cloned()
        .collect()
}
pub fn mix(groups: &[Vec<Video>], limit: usize) -> Vec<Video> {
    if limit == 0 {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(limit.min(MAX_RESULTS));
    for i in 0..MAX_RESULTS {
        for group in groups {
            if let Some(v) = group.get(i) {
                if seen.insert(v.id.clone()) {
                    out.push(v.clone());
                    if out.len() >= limit.min(MAX_RESULTS) {
                        return out;
                    }
                }
            }
        }
    }
    out
}
pub fn song_duration(v: &Video) -> bool {
    v.duration.is_finite() && (60. ..=600.).contains(&v.duration)
}
pub fn music_candidates(videos: &mut Vec<Video>, history: &[Video]) {
    for v in videos.iter_mut() {
        if v.music.is_none() {
            v.music = history.iter().find(|h| h.id == v.id).and_then(|h| h.music);
        }
    }
    videos.retain(|v| v.music != Some(false) && song_duration(v));
    videos.sort_by_key(|v| {
        std::cmp::Reverse(if v.music == Some(true) {
            2
        } else if music_hint(v) {
            1
        } else {
            0
        })
    });
}
fn content_words(title: &str) -> Vec<String> {
    const NOISE: &[&str] = &[
        "the",
        "a",
        "an",
        "and",
        "of",
        "in",
        "to",
        "for",
        "with",
        "feat",
        "ft",
        "official",
        "music",
        "video",
        "audio",
        "lyrics",
        "lyric",
        "visualizer",
        "hd",
        "hq",
        "4k",
        "8k",
        "60fps",
        "1080p",
        "720p",
        "remastered",
        "remaster",
        "cover",
        "remix",
        "slowed",
        "sped",
        "up",
        "nightcore",
        "karaoke",
        "instrumental",
        "version",
        "extended",
        "mix",
        "edit",
        "live",
        "short",
        "film",
        "foundation",
        "collaboration",
        "full",
        "ver",
        "part",
        "song",
        "songs",
        "dance",
        "different",
        "languages",
        "new",
        "hits",
        "collection",
    ];
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1 && !NOISE.contains(w) && !w.chars().all(|c| c.is_ascii_digit()))
        .take(64)
        .map(str::to_owned)
        .collect()
}
fn alternate_version(seed: &Video, candidate: &Video) -> bool {
    // Japanese music titles commonly quote the actual song before anime/collaboration
    // suffixes. Match that song, rather than diluting it with every suffix token.
    if music_hint(seed) {
        for (open, close) in [('｢', '｣'), ('「', '」'), ('『', '』')] {
            if let Some((_, tail)) = seed.title.split_once(open)
                && let Some((song, _)) = tail.split_once(close)
            {
                let main = content_words(song);
                let other = content_words(&candidate.title);
                if !main.is_empty()
                    && (main.len() > 1 || main[0].chars().count() >= 5)
                    && main.iter().all(|word| other.contains(word))
                {
                    return true;
                }
                break;
            }
        }
    }
    let author = content_words(&seed.channel);
    let base: Vec<_> = content_words(&seed.title)
        .into_iter()
        .filter(|w| !author.contains(w))
        .collect();
    let other: Vec<_> = content_words(&candidate.title)
        .into_iter()
        .filter(|w| !author.contains(w))
        .collect();
    if base.is_empty() || other.is_empty() {
        return false;
    }
    let common = base.iter().filter(|w| other.contains(w)).count();
    if common >= 2 && common * 5 >= base.len().max(other.len()) * 4 {
        return true;
    }
    let lower = candidate.title.to_lowercase();
    let variant = [
        "cover",
        "remix",
        "remaster",
        "slowed",
        "sped up",
        "nightcore",
        "karaoke",
        "instrumental",
        "8d audio",
        "extended version",
    ]
    .iter()
    .any(|v| lower.contains(v));
    let main = seed
        .title
        .split_once(" - ")
        .or_else(|| seed.title.split_once(" – "))
        .or_else(|| seed.title.split_once(" — "))
        .map(|(_, title)| title)
        .unwrap_or(&seed.title);
    let main = main.split(['(', '[']).next().unwrap_or(main);
    let main = content_words(main);
    variant
        && !main.is_empty()
        && (main.len() > 1 || main[0].chars().count() >= 5)
        && main.iter().all(|w| other.contains(w))
        || base.len() == 1 && other == base && base[0].chars().count() >= 5
}
fn same_channel(a: &Video, b: &Video) -> bool {
    !a.channel.trim().is_empty() && a.channel.trim().eq_ignore_ascii_case(b.channel.trim())
}
fn japanese(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c as u32,0x3040..=0x30ff|0x4e00..=0x9fff))
}
fn clearly_music(v: &Video) -> bool {
    if let Some(music) = v.music {
        return music;
    }
    let title = v.title.to_lowercase();
    let channel = v.channel.to_lowercase();
    channel.ends_with("vevo")
        || channel.ends_with(" - topic")
        || [
            "music video",
            "official music",
            "official audio",
            "lyrics",
            "soundtrack",
            "anime opening",
            "anime ending",
        ]
        .iter()
        .any(|hint| title.contains(hint))
}
fn topical_overlap(anchor: &[String], candidate: &[String]) -> usize {
    let matches: HashSet<_> = anchor
        .iter()
        .filter(|word| candidate.contains(word))
        .collect();
    if matches.len() >= 2 || matches.iter().any(|word| word.chars().count() >= 5) {
        matches.len()
    } else {
        0
    }
}
fn focus(videos: &mut Vec<Video>, seed: &Video, history: &[Video]) {
    let seed_music = music_hint(seed);
    let short_music = seed_music && seed.duration > 0. && seed.duration <= 600.;
    let anchor_words = content_words(&seed.title);
    for video in videos.iter_mut() {
        // Reuse only known classification metadata; history creators are not relevance signals.
        if video.music.is_none() {
            video.music = history
                .iter()
                .find(|h| h.id == video.id)
                .and_then(|h| h.music);
        }
    }
    videos.retain(|v| {
        if same_channel(v, seed) {
            return true;
        }
        let words = content_words(&v.title);
        let topical = topical_overlap(&anchor_words, &words) > 0;
        if !topical && seed.music == Some(false) && clearly_music(v) {
            return false;
        }
        if !topical && seed_music && v.music == Some(false) {
            return false;
        }
        if v.duration <= 900. {
            return true;
        }
        // Language is a weak music-context hint, never a reason to mix songs into commentary.
        if topical || (seed_music && japanese(&seed.title) && japanese(&v.title)) {
            return true;
        }
        let lower = v.title.to_lowercase();
        let tokens: Vec<_> = lower.split(|c: char| !c.is_alphanumeric()).collect();
        let compilation = [
            "playlist",
            "compilation",
            "medley",
            "openings",
            "spotify",
            "chill",
            "relax",
            "sleep",
            "hits",
            "mix",
            "songs",
        ]
        .iter()
        .any(|word| tokens.contains(word))
            || ["作業用", "メドレー", "詰め合わせ"]
                .iter()
                .any(|word| lower.contains(word));
        !compilation && (!short_music || music_hint(v))
    });
    let seed_japanese = seed_music && japanese(&seed.title);
    // Stable, seed-only relevance. History powers For you through its separate per-seed calls.
    videos.sort_by_cached_key(|v| {
        let mut score = if same_channel(v, seed) { 80 } else { 0 };
        let words = content_words(&v.title);
        score += topical_overlap(&anchor_words, &words).min(4) as i32 * 6;
        if seed_japanese && japanese(&v.title) {
            score += 12;
        }
        if seed_music && music_hint(v) {
            score += 4;
        }
        std::cmp::Reverse(score)
    });
}
pub(super) fn recent_ids(history: &[Video]) -> Vec<String> {
    history.iter().take(20).map(|v| v.id.clone()).collect()
}
pub fn filter(
    videos: Vec<Video>,
    s: &Settings,
    history: &[Video],
    current: Option<&Video>,
) -> Vec<Video> {
    let terms = |s: &str| {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    let channels = terms(&s.blocked_channels);
    let keywords = terms(&s.blocked_keywords);
    let mut ids = HashSet::new();
    let mut videos: Vec<_> = videos
        .into_iter()
        .filter(|v| {
            !v.id.is_empty()
                && (!s.music || song_duration(v))
                && v.views
                    .is_none_or(|views| views >= s.minimum_recommendation_views)
                && !s.recommendation_avoid.contains(&v.id)
                && !s.recommendation_defer.contains(&v.id)
                && current.is_none_or(|current| current.id != v.id)
                && !(s.hide_alternate_versions
                    && current.is_some_and(|current| alternate_version(current, v)))
                && ids.insert(v.id.clone())
                && !(s.hide_watched && history.iter().any(|h| h.id == v.id))
                && !channels
                    .iter()
                    .any(|c| v.channel.to_lowercase().contains(c))
                && !keywords.iter().any(|k| v.title.to_lowercase().contains(k))
        })
        .take(MAX_CANDIDATES)
        .collect();
    if let Some(seed) = current {
        focus(&mut videos, seed, history);
        if !s.music && music_hint(seed) {
            super::music_taste::Taste::from_song(seed).filter(&mut videos);
        }
    }
    if s.music {
        music_candidates(&mut videos, history);
        let taste = current
            .filter(|_| s.seed_recommendations)
            .map(super::music_taste::Taste::from_song)
            .unwrap_or_else(|| super::music_taste::Taste::from_history(history));
        taste.filter(&mut videos);
    }
    videos.truncate(count(s));
    videos
}
pub fn fallback_query(
    s: &Settings,
    history: &[Video],
    seed: Option<&Video>,
    rotation: usize,
) -> String {
    let interests: Vec<_> = s
        .interests
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect();
    let interest = interests
        .get(rotation % interests.len().max(1))
        .copied()
        .unwrap_or(if s.music { "music" } else { "documentary" });
    let seed = seed.or_else(|| {
        if s.use_history && s.recommendation_mode != 2 {
            history.get(rotation % history.len().max(1))
        } else {
            None
        }
    });
    if let Some(seed) = seed {
        let channel = seed.channel.trim();
        let topic = if music_hint(seed) && !channel.is_empty() {
            "music".to_string()
        } else {
            content_words(&seed.title)
                .into_iter()
                .take(6)
                .collect::<Vec<_>>()
                .join(" ")
        };
        return format!("ytsearch{}:{} {}", count(s), channel, topic)
            .trim_end()
            .to_string();
    }
    let topic = seed
        .map(|v| {
            if s.recommendation_mode == 2 {
                interest.to_owned()
            } else if s.hide_alternate_versions && !v.channel.trim().is_empty() {
                format!("{} {}", v.channel, interest)
            } else {
                format!(
                    "{} {}",
                    v.channel,
                    v.title.chars().take(100).collect::<String>()
                )
            }
        })
        .unwrap_or_else(|| interest.into());
    format!(
        "ytsearch{}:{topic} {interest}{}",
        count(s),
        if s.music { " music" } else { "" }
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn channel_feed_reads_accessible_counts_and_explicit_shorts() {
        let data = serde_json::json!({"contents":[
            {"lockupViewModel":{"contentType":"LOCKUP_CONTENT_TYPE_VIDEO","contentId":"abcdefghijk","metadata":{"lockupMetadataViewModel":{"title":{"content":"Video"},"metadata":{"contentMetadataViewModel":{"metadataRows":[{"metadataParts":[{"text":{"content":"4.7M"},"accessibilityLabel":"4.7 million views","leadingIcon":{"name":"PLAY_ARROW_OUTLINED"}},{"text":{"content":"2y ago"},"accessibilityLabel":"2 years ago"}]}]}}}}}},
            {"shortsLockupViewModel":{"onTap":{"innertubeCommand":{"reelWatchEndpoint":{"videoId":"lmnopqrstuv"}}},"overlayMetadata":{"primaryText":{"content":"Short"},"secondaryText":{"content":"4.4K views"}}}}
        ]});
        let (rows, _) = browse_candidates(&data);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].views, Some(4_700_000));
        assert!(rows[0].published.is_some());
        assert_eq!(rows[1].views, Some(4400));
        let mut watch = Vec::new();
        collect(&data, &mut watch, &mut None, 0);
        assert_eq!(watch.len(), 1);
    }
    #[test]
    fn repeated_or_over_budget_continuations_are_rejected_before_network_access() {
        for visited in [vec![continuation_key("seen-page")], vec![0; 9]] {
            let cursor = HomeCursor {
                token: "seen-page".into(),
                context: Value::Null,
                music: Some(true),
                classified: true,
                visited,
            };
            let error = request_page("abcdefghijk", Some(cursor)).err().unwrap();
            assert!(error.to_string().contains("Repeated or exhausted"));
        }
    }
    #[test]
    fn rejected_pages_cannot_fill_the_retained_candidate_budget() {
        let settings = Settings {
            music: true,
            hide_alternate_versions: false,
            ..Settings::default()
        };
        let seed = video("seed", "Artist - Song", "Artist");
        let mut retained = Vec::new();
        for page in 0..6 {
            retained.extend((0..50).map(|i| {
                let mut row = video(&format!("song{page}-{i}"), "Song", "Artist");
                row.duration = if i < 42 { 3600. } else { 180. };
                row
            }));
            retained = filter_page(retained, &settings, &[], &seed, Some(true), true);
        }
        assert_eq!(
            retained.len(),
            40,
            "more than 200 rejected candidates must not stop refill"
        );
    }
    #[test]
    fn music_paging_counts_only_displayable_songs() {
        let settings = Settings {
            music: true,
            hide_alternate_versions: false,
            ..Settings::default()
        };
        let mut seed = video("seed", "Artist - Song", "Artist");
        seed.music = Some(true);
        let mut rows: Vec<_> = (0..40)
            .map(|i| {
                let mut row = video(&format!("song{i}"), "Song", "Artist");
                row.music = Some(false); // Even same-channel nonmusic must not count toward 40.
                row
            })
            .collect();
        assert!(!enough_filtered(
            &rows,
            &settings,
            &[],
            &seed,
            40,
            Some(true),
            true
        ));
        rows.extend((40..80).map(|i| {
            let mut row = video(&format!("song{i}"), "Song", "Artist");
            row.music = Some(true);
            row
        }));
        assert!(enough_filtered(
            &rows,
            &settings,
            &[],
            &seed,
            40,
            Some(true),
            true
        ));
        assert_eq!(filter(rows, &settings, &[], Some(&seed)).len(), 40);
    }
    #[test]
    fn raw_fifty_is_not_enough_when_only_ten_pass_filters() {
        let s = Settings {
            minimum_recommendation_views: 10_000,
            ..Settings::default()
        };
        let seed = video("seed", "Rendering tutorial", "Tutor");
        let mut rows: Vec<_> = (0..50)
            .map(|i| {
                let mut row = video(&format!("v{i}"), "Desktop tutorial", "Other");
                row.views = Some(if i < 10 { 20_000 } else { 500 });
                row
            })
            .collect();
        assert!(!enough_filtered(
            &rows,
            &s,
            &[],
            &seed,
            30,
            Some(false),
            true
        ));
        rows.extend((50..70).map(|i| {
            let mut row = video(&format!("v{i}"), "Desktop tutorial", "Other");
            row.views = Some(20_000);
            row
        }));
        assert!(enough_filtered(
            &rows,
            &s,
            &[],
            &seed,
            30,
            Some(false),
            true
        ));
        assert_eq!(filter(rows, &s, &[], Some(&seed)).len(), 30);
    }
    #[test]
    fn pagination_uses_watch_page_classification_for_relevance() {
        let s = Settings {
            minimum_recommendation_views: 0,
            ..Settings::default()
        };
        let mut seed = video("seed", "Platform - development", "Tutor");
        seed.music = Some(true);
        let mut row = video("song", "Artist - Official Music Video", "VEVO");
        row.music = Some(true);
        assert!(enough_filtered(
            &[row.clone()],
            &s,
            &[],
            &seed,
            1,
            Some(true),
            true
        ));
        assert!(!enough_filtered(
            &[row],
            &s,
            &[],
            &seed,
            1,
            Some(false),
            true
        ));
    }
    fn video(id: &str, title: &str, channel: &str) -> Video {
        Video {
            id: id.into(),
            title: title.into(),
            channel: channel.into(),
            duration: 180.,
            views: None,
            ..Video::default()
        }
    }
    #[test]
    fn fresh_recommendations_exclude_recent_twenty_and_previous_page() {
        let history: Vec<_> = (0..30)
            .map(|i| Video {
                id: format!("song{i}"),
                music: Some(true),
                duration: 180.,
                ..Default::default()
            })
            .collect();
        let s = Settings {
            music: true,
            minimum_recommendation_views: 0,
            recommendation_avoid: recent_ids(&history),
            recommendation_defer: vec!["song20".into()],
            ..Default::default()
        };
        assert_eq!(s.recommendation_avoid.len(), 20);
        let filtered = filter(history.clone(), &s, &history, None);
        assert_eq!(filtered.len(), 9);
        assert_eq!(filtered[0].id, "song21");
        assert!(filtered.iter().all(|v| v.id != "song20"));
        assert!(
            filtered
                .iter()
                .all(|v| !s.recommendation_avoid.contains(&v.id))
        );
        assert!(
            serde_json::to_value(&s)
                .unwrap()
                .get("recommendation_avoid")
                .is_none()
        );
    }
    #[test]
    fn video_limits_round_trip_and_music_keeps_its_own_limit() {
        for limit in VIDEO_LIMITS {
            assert_eq!(video_limit(video_limit_choice(limit)), limit);
            let settings = Settings {
                recommendation_count: limit,
                ..Settings::default()
            };
            assert_eq!(count(&settings), limit as usize);
            assert_eq!(
                count(&Settings {
                    music: true,
                    ..settings
                }),
                40
            );
        }
        assert_eq!(video_limit(video_limit_choice(999)), 30);
        let settings = Settings {
            recommendation_count: 50,
            minimum_recommendation_views: 0,
            ..Settings::default()
        };
        assert_eq!(
            filter(
                (0..80).map(|i| video(&i.to_string(), "a", "b")).collect(),
                &settings,
                &[],
                None
            )
            .len(),
            50
        );
    }
    #[test]
    fn music_feed_rejects_compilations_unknown_lengths_and_clips() {
        let durations = [
            0.,
            59.,
            60.,
            540.,
            600.,
            601.,
            1800.,
            3600.,
            f64::NAN,
            f64::INFINITY,
        ];
        let rows = durations
            .iter()
            .enumerate()
            .map(|(i, duration)| Video {
                id: format!("item{i}"),
                duration: *duration,
                music: Some(true),
                ..Video::default()
            })
            .collect::<Vec<_>>();
        let music = Settings {
            music: true,
            minimum_recommendation_views: 0,
            ..Settings::default()
        };
        let filtered = filter(rows.clone(), &music, &[], None);
        assert_eq!(
            filtered.iter().map(|v| v.duration).collect::<Vec<_>>(),
            vec![60., 540., 600.]
        );
        let mut candidates = rows.clone();
        music_candidates(&mut candidates, &[]);
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            filter(rows, &Settings::default(), &[], None).len(),
            durations.len()
        );
    }
    #[test]
    fn paging_keeps_continuation_after_candidate_limit() {
        let mut items = (0..MAX_CANDIDATES + 10).map(|i| serde_json::json!({"compactVideoRenderer":{"videoId":format!("v{i}"),"title":"Song"}})).collect::<Vec<_>>();
        items.push(serde_json::json!({"continuationItemRenderer":{"continuationEndpoint":{"continuationCommand":{"token":"next-page"}}}}));
        let mut out = Vec::new();
        let mut token = None;
        collect(&Value::Array(items), &mut out, &mut token, 0);
        assert_eq!(out.len(), MAX_CANDIDATES);
        assert_eq!(token.as_deref(), Some("next-page"));
    }
    #[test]
    fn sidebar_shapes_and_continuation() {
        let value = serde_json::json!({"results": [{"compactVideoRenderer": {"videoId": "abcdefghijk", "title": {"simpleText": "Compact"}, "shortBylineText": {"runs": [{"text": "Channel"}]}, "lengthText": {"simpleText": "1:02"}, "viewCountText": {"simpleText": "1.2M views"}}}, {"lockupViewModel": {"contentId": "lmnopqrstuv", "contentType": "LOCKUP_CONTENT_TYPE_VIDEO", "metadata": {"lockupMetadataViewModel": {"title": {"content": "Modern"}, "metadata": {"contentMetadataViewModel": {"metadataRows": [{"metadataParts": [{"text": {"content": "Creator"}}]}]}}}}}}, {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "next"}}}}]});
        let mut out = Vec::new();
        let mut token = None;
        collect(&value, &mut out, &mut token, 0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].duration, 62.);
        assert_eq!(out[0].views, Some(1_200_000));
        assert_eq!(out[1].channel, "Creator");
        assert_eq!(token.as_deref(), Some("next"));
        collect(&value, &mut out, &mut token, 0);
        assert_eq!(
            out.len(),
            2,
            "paging must not consume the pool with duplicate IDs"
        );
        assert!(
            json_after(
                "var ytInitialData = {\"ok\":true}; extra",
                "var ytInitialData = "
            )
            .unwrap()["ok"]
                .as_bool()
                .unwrap()
        );
    }
    #[test]
    fn short_music_relevance_prioritizes_artists_and_history_over_generic_mix_fillers() {
        let mut seed = video(
            "seed",
            "Creepy Nuts Bling Bang Born Anime マッシュル",
            "Creepy Nuts",
        );
        seed.duration = 172.;
        seed.music = Some(true);
        let mut playlist = video("mix", "Spotify Acoustic Chill Playlist", "Mix channel");
        playlist.duration = 7200.;
        let mut artist = video("artist", "Creepy Nuts Otonoke ダンダダン", "Creepy Nuts");
        artist.duration = 190.;
        let mut recent = video("recent", "稲葉曇 New song", "INABAKUMORI");
        recent.duration = 240.;
        let mut other = video("other", "Post Malone - Sunflower", "Post Malone");
        other.duration = 162.;
        let mut kitchen = video(
            "kitchen",
            "Big Medium Small Plate Challenge",
            "Fun Challenge",
        );
        kitchen.duration = 1394.;
        let history = vec![video("old", "稲葉曇 - ラグトレイン", "inabakumori")];
        let out = filter(
            vec![
                playlist.clone(),
                other.clone(),
                recent.clone(),
                artist.clone(),
                kitchen,
            ],
            &Settings::default(),
            &history,
            Some(&seed),
        );
        assert_eq!(
            out.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            vec!["artist", "recent", "other"]
        );
        let mut anime = video("anime", "Anime openings collection", "Anime");
        anime.duration = 7200.;
        let mut jp = video("jp", "アニソン メドレー", "Japanese songs");
        jp.duration = 7200.;
        let out = filter(
            vec![anime, jp, playlist],
            &Settings::default(),
            &history,
            Some(&seed),
        );
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|v| v.id == "anime") && out.iter().any(|v| v.id == "jp"));
    }
    #[test]
    fn a_shared_short_word_is_not_a_regional_playlist_connection() {
        let mut seed = video(
            "seed",
            "Creepy Nuts｢Bling-Bang-Bang-Born｣ × TV Anime｢マッシュル-MASHLE-｣ Collaboration Music Video",
            "Creepy Nuts",
        );
        seed.music = Some(true);
        seed.duration = 172.;
        let mut mix = video(
            "mix",
            "Mariah Deborah – Na Para Bang OPM Indie Pop Hits 2026 Tagalog Love Songs Philippines",
            "Mix",
        );
        mix.duration = 5814.;
        let mut phonk = video("phonk", "TOP 26 BRAZILIAN FUNK HITS 2026", "Phonk Planet");
        phonk.duration = 6812.;
        let mut spotify = video(
            "spotify",
            "Spotify Acoustic Chill English 2026",
            "Servio Jazz",
        );
        spotify.duration = 4341.;
        assert!(
            filter(
                vec![mix, phonk, spotify],
                &Settings::default(),
                &[],
                Some(&seed)
            )
            .is_empty()
        );
    }
    #[test]
    fn similar_music_uses_song_genre_instead_of_history_genre() {
        let mut seed = video("seed", "Electronic phonk", "Seed Artist");
        seed.music = Some(true);
        seed.duration = 180.;
        seed.genres = vec!["phonk".into()];
        let mut phonk = video("candidate", "Fresh phonk", "Other Artist");
        phonk.music = Some(true);
        phonk.duration = 180.;
        phonk.genres = vec!["phonk".into()];
        let mut rock = video("rock", "New rock song", "Band");
        rock.music = Some(true);
        rock.duration = 180.;
        rock.genres = vec!["rock".into()];
        let history = vec![rock.clone(), rock.clone(), rock.clone()];
        let settings = Settings {
            music: true,
            seed_recommendations: true,
            ..Settings::default()
        };
        let rows = filter(
            vec![phonk.clone(), rock.clone()],
            &settings,
            &history,
            Some(&seed),
        );
        assert_eq!(
            rows.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["candidate"]
        );
        let settings = Settings {
            seed_recommendations: false,
            ..settings
        };
        let rows = filter(vec![phonk, rock], &settings, &history, Some(&seed));
        assert_eq!(
            rows.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["rock"]
        );
    }
    #[test]
    fn nonmusic_related_does_not_import_music_from_history() {
        let mut seed = video("politics", "Election policy analysis", "Commentator");
        seed.music = Some(false);
        seed.duration = 1200.;
        let mut song = video("song", "Japanese anime song Official Music Video", "Artist");
        song.music = Some(true);
        song.duration = 180.;
        let mut playlist = video("mix", "Spotify Filipino songs playlist", "Mixes");
        playlist.duration = 7200.;
        let mut related = video("related", "Election policy debate", "News");
        related.duration = 900.;
        let out = filter(
            vec![song.clone(), playlist, related],
            &Settings::default(),
            &[song],
            Some(&seed),
        );
        assert_eq!(
            out.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            vec!["related"]
        );
        let mut tech = video("tech", "Rust memory and performance", "Developer");
        tech.music = Some(false);
        let mut song = video("song2", "Anime opening lyrics", "Artist");
        song.duration = 180.;
        assert!(filter(vec![song], &Settings::default(), &[], Some(&tech)).is_empty());
    }
    #[test]
    fn related_fallback_keeps_the_seed_without_rotating_unrelated_interests() {
        let s = Settings {
            interests: "anime, technology".into(),
            ..Settings::default()
        };
        let seed = video("seed", "Artist - Song", "Artist");
        assert_eq!(
            fallback_query(&s, &[], Some(&seed), 0),
            "ytsearch30:Artist music"
        );
        assert_eq!(
            fallback_query(&s, &[], Some(&seed), 1),
            "ytsearch30:Artist music"
        );
    }
    #[test]
    fn minimum_views_filters_recommendations_without_guessing_unknown_counts() {
        let candidates = || {
            [Some(0), Some(273), Some(9_999), Some(10_000), None]
                .into_iter()
                .enumerate()
                .map(|(i, views)| {
                    let mut v = video(&i.to_string(), "video", "channel");
                    v.views = views;
                    v
                })
                .collect()
        };
        let settings = Settings::default();
        assert_eq!(
            filter(candidates(), &settings, &[], None)
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
            vec!["3", "4"]
        );
        let settings = Settings {
            minimum_recommendation_views: 0,
            ..settings
        };
        assert_eq!(filter(candidates(), &settings, &[], None).len(), 5);
    }
    #[test]
    fn controls_filter_deduplicate_and_bound() {
        let s = Settings {
            blocked_channels: "blocked".into(),
            blocked_keywords: "spoiler".into(),
            hide_watched: true,
            ..Settings::default()
        };
        let history = vec![video("seen", "seen", "ok")];
        let candidates = vec![
            video("current", "a", "ok"),
            video("seen", "b", "ok"),
            video("x", "SPOILER", "ok"),
            video("y", "c", "Blocked channel"),
            video("z", "good", "ok"),
            video("z", "duplicate", "ok"),
        ];
        let out = filter(
            candidates,
            &s,
            &history,
            Some(&video("current", "Current video", "Original")),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "z");
        assert_eq!(count(&Settings::default()), 30);
        assert_eq!(
            count(&Settings {
                music: true,
                ..Settings::default()
            }),
            40
        );
        let s = Settings {
            recommendation_count: 25,
            ..Settings::default()
        };
        assert_eq!(
            filter(
                (0..50).map(|i| video(&i.to_string(), "a", "b")).collect(),
                &s,
                &[],
                None
            )
            .len(),
            25
        );
    }
    #[test]
    fn quoted_song_title_still_excludes_first_take_and_remix_variants() {
        let seed = video(
            "seed",
            "Creepy Nuts｢Bling-Bang-Bang-Born｣ × TV Anime｢マッシュル-MASHLE-｣ Collaboration Music Video",
            "Creepy Nuts",
        );
        assert!(alternate_version(
            &seed,
            &video(
                "a",
                "Creepy Nuts - Bling‐Bang‐Bang‐Born / THE FIRST TAKE",
                "THE FIRST TAKE"
            )
        ));
        assert!(alternate_version(
            &seed,
            &video("b", "Bling Bang Bang Born remix", "Remixes")
        ));
        assert!(!alternate_version(
            &seed,
            &video(
                "c",
                "Creepy Nuts｢Otonoke｣ × Anime DAN DA DAN Collaboration Music Video",
                "Creepy Nuts"
            )
        ));
    }
    #[test]
    fn related_can_hide_covers_remixes_and_duplicate_uploads() {
        let seed = video(
            "seed",
            "Papa Roach - Scars (Official Music Video)",
            "Papa Roach",
        );
        assert!(alternate_version(
            &seed,
            &video("a", "Papa Roach - Scars HD", "Upload")
        ));
        assert!(alternate_version(
            &seed,
            &video("b", "Scars - acoustic cover", "Guitarist")
        ));
        assert!(alternate_version(
            &seed,
            &video("c", "Papa Roach Scars slowed remix", "Remixes")
        ));
        assert!(!alternate_version(
            &seed,
            &video("d", "Papa Roach - Last Resort", "Papa Roach")
        ));
        assert!(!alternate_version(
            &seed,
            &video("e", "Linkin Park - Numb", "Linkin Park")
        ));
        let s = Settings::default();
        let list = vec![
            video("a", "Scars remix", "Remixes"),
            video("b", "Last Resort", "Papa Roach"),
        ];
        assert_eq!(filter(list.clone(), &s, &[], Some(&seed)).len(), 1);
        let s = Settings {
            hide_alternate_versions: false,
            ..s
        };
        assert_eq!(filter(list, &s, &[], Some(&seed)).len(), 2);
    }
    #[test]
    fn history_can_be_excluded_from_fallback() {
        let history = vec![video("x", "Private title", "Private channel")];
        let s = Settings {
            interests: "science".into(),
            use_history: false,
            ..Settings::default()
        };
        assert!(!fallback_query(&s, &history, None, 0).contains("Private"));
    }
    #[test]
    fn mode_history_seeds_do_not_require_music_classification() {
        let history = (0..30)
            .map(|i| {
                let mut v = video(&format!("v{i:010}"), "A track", "Channel");
                v.music = Some(i % 2 == 0);
                v
            })
            .collect::<Vec<_>>();
        let videos = history_seeds(&history, false);
        assert_eq!(videos.len(), 10);
        assert_eq!(videos[9].id, history[9].id);
        let music = history_seeds(&history, true);
        assert_eq!(music.len(), 10);
        assert_eq!(music[9].id, history[9].id);
        assert_eq!(music[0].id, history[0].id);
        assert_eq!(recent_ids(&history).len(), 20);
        assert_eq!(recent_ids(&history)[1], history[1].id);
        assert_eq!(
            history_seeds(&history, true)
                .iter()
                .map(|v| &v.id)
                .collect::<Vec<_>>(),
            music.iter().map(|v| &v.id).collect::<Vec<_>>()
        );
        let legacy:Video=serde_json::from_value(serde_json::json!({"id":"abcdefghijk","title":"Song (Lyrics)","channel":"Artist","duration":200})).unwrap();
        assert_eq!(legacy.music, None);
        assert!(music_hint(&legacy));
    }
    #[test]
    fn music_classification_includes_variants_but_respects_known_nonmusic() {
        assert_eq!(
            music_metadata(&serde_json::json!({"categories":["Music"]})),
            Some(true)
        );
        assert_eq!(
            music_metadata(&serde_json::json!({"track":"Song","artists":["Artist"]})),
            Some(true)
        );
        assert_eq!(
            music_metadata(&serde_json::json!({"title":"Unknown"})),
            None
        );
        for title in ["Song lyrics", "Song acoustic cover", "Song remix"] {
            assert!(music_hint(&video("abcdefghijk", title, "Artist")));
        }
        let mut v = video("abcdefghijk", "Game - cover system tutorial", "Gaming");
        v.music = Some(false);
        assert!(!music_hint(&v));
        assert!(!music_hint(&video(
            "abcdefghijk",
            "How bridges work",
            "Engineering"
        )));
    }
    #[test]
    fn broad_categories_allow_music_hints_but_tech_does_not() {
        let mut v = video(
            "mLW35YMzELE",
            "Creepy Nuts Collaboration Music Video",
            "Creepy Nuts",
        );
        for category in ["Entertainment", "People & Blogs"] {
            v.music = music_metadata(&serde_json::json!({"categories":[category]}));
            assert_eq!(v.music, None);
            assert!(music_hint(&v));
        }
        for category in ["Science & Technology", "News & Politics"] {
            v.music = music_metadata(&serde_json::json!({"categories":[category]}));
            assert_eq!(v.music, Some(false));
            assert!(!music_hint(&v));
        }
    }
    #[test]
    fn feed_mix_shares_slots_and_deduplicates() {
        let a = vec![
            video("a", "a", "a"),
            video("shared", "s", "a"),
            video("c", "c", "a"),
        ];
        let b = vec![
            video("b", "b", "b"),
            video("shared", "s", "b"),
            video("d", "d", "b"),
        ];
        let out = mix(&[a, b], 4);
        assert_eq!(
            out.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "shared", "c"]
        );
        assert_eq!(mix(&[], 30).len(), 0);
    }
    #[test]
    fn video_music_recommendations_reject_known_genre_conflicts_only() {
        let seed = Video {
            id: "seed0000000".into(),
            title: "Anime opening アニメ".into(),
            music: Some(true),
            duration: 180.,
            ..Default::default()
        };
        let candidates = ["Japanese anime OST", "Brazilian phonk", "Unknown song"]
            .iter()
            .enumerate()
            .map(|(i, title)| Video {
                id: format!("video{i:06}"),
                title: (*title).into(),
                duration: 180.,
                ..Default::default()
            })
            .collect();
        let result = filter(
            candidates,
            &Settings {
                minimum_recommendation_views: 0,
                ..Default::default()
            },
            &[],
            Some(&seed),
        );
        assert!(result.iter().any(|v| v.title == "Japanese anime OST"));
        assert!(result.iter().any(|v| v.title == "Unknown song"));
        assert!(!result.iter().any(|v| v.title == "Brazilian phonk"));
    }
}
