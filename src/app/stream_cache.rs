//! Small session-only cache. Playback links never enter saved playlists or disk caches.
use super::*;
use std::{collections::VecDeque, time::SystemTime};
const MAX_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 4;
const TTL: Duration = Duration::from_secs(180);
#[derive(PartialEq)]
struct Identity {
    video: String,
    hardware: bool,
}
impl Identity {
    fn new(video: &str, s: &Settings) -> Self {
        Self {
            video: video.into(),
            hardware: s.hardware,
        }
    }
}
struct Entry {
    identity: Identity,
    selector: String,
    bytes: Vec<u8>,
    resolved_at: Instant,
}
pub(super) struct Hit {
    pub bytes: Vec<u8>,
    pub resolved_at: Instant,
    pub exact: bool,
}
pub(super) struct StreamCache {
    enabled: bool,
    entries: VecDeque<Entry>,
    epoch: u64,
}
impl Default for StreamCache {
    fn default() -> Self {
        Self {
            enabled: true,
            entries: VecDeque::new(),
            epoch: 0,
        }
    }
}
impl StreamCache {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn configure(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.clear(); // Invalidate pending inserts when the preference changes.
            self.enabled = enabled;
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.epoch = self.epoch.wrapping_add(1);
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    fn prune(&mut self) {
        self.entries.retain(|e| e.resolved_at.elapsed() < TTL);
    }
    pub fn lookup(&mut self, video: &str, s: &Settings) -> Option<Hit> {
        if !self.enabled {
            return None;
        }
        self.prune();
        let key = Identity::new(video, s);
        let selector = format_selector(s);
        let entry = self
            .entries
            .iter()
            .rev()
            .find(|e| e.identity == key && e.selector == selector)
            .or_else(|| self.entries.iter().rev().find(|e| e.identity == key))?;
        let mut info: Value = serde_json::from_slice(&entry.bytes).ok()?;
        if !valid_until(&info, SystemTime::now()) {
            return None;
        }
        let reselected = entry.selector != selector && select_cached_format(&mut info, &selector);
        Some(Hit {
            bytes: if reselected {
                serde_json::to_vec(&info).ok()?
            } else {
                entry.bytes.clone()
            },
            resolved_at: entry.resolved_at,
            exact: entry.selector == selector || reselected,
        })
    }
    pub fn insert(&mut self, video: &str, s: &Settings, info: &Value, resolved_at: Instant) {
        self.store(video, s, info, resolved_at, format_selector(s));
    }
    pub fn seed(&mut self, video: &str, s: &Settings, info: &Value) {
        // A pasted URL was already fully extracted while browsing. Its default
        // selected format may differ, so reuse only its unselected format list.
        self.store(video, s, info, Instant::now(), "<browse metadata>".into());
    }
    fn store(
        &mut self,
        video: &str,
        s: &Settings,
        info: &Value,
        resolved_at: Instant,
        selector: String,
    ) {
        if !self.enabled {
            return;
        }
        self.prune();
        if info["is_live"] == true
            || info["live_status"]
                .as_str()
                .is_some_and(|s| s != "not_live")
            || !valid_until(info, SystemTime::now())
        {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(info) else {
            return;
        };
        if bytes.len() > MAX_BYTES {
            return;
        }
        let identity = Identity::new(video, s);
        self.entries
            .retain(|e| e.identity != identity || e.selector != selector);
        self.entries.push_back(Entry {
            identity,
            selector,
            bytes,
            resolved_at,
        });
        while self.entries.len() > MAX_ENTRIES
            || self.entries.iter().map(|e| e.bytes.len()).sum::<usize>() > MAX_BYTES
        {
            self.entries.pop_front();
        }
    }
    pub fn invalidate(&mut self, video: &str) {
        self.entries.retain(|e| e.identity.video != video);
    }
}
// Resolve only exact format IDs emitted by our own quality menu. Arbitrary
// yt-dlp expressions still go through yt-dlp; no requests or probes happen here.
fn select_cached_format(info: &mut Value, selector: &str) -> bool {
    let ids = selector.split('+').collect::<Vec<_>>();
    if ids.is_empty() || ids.len() > 2 {
        return false;
    }
    let direct = |f: &Value| {
        f["has_drm"] != true
            && matches!(f["protocol"].as_str(), Some("https" | "http"))
            && f["url"]
                .as_str()
                .is_some_and(|u| u.starts_with("https://") || u.starts_with("http://"))
    };
    let audio = info["requested_formats"]
        .as_array()
        .and_then(|fs| fs.iter().find(|f| f["vcodec"] == "none" && direct(f)))
        .or_else(|| (info["vcodec"] == "none" && direct(info)).then_some(&*info));
    let Some(formats) = info["formats"].as_array() else {
        return false;
    };
    let mut picked = Vec::with_capacity(2);
    for id in ids {
        let format = if id == "bestaudio" {
            audio
        } else {
            formats
                .iter()
                .find(|f| f["format_id"].as_str() == Some(id) && direct(f))
        };
        let Some(format) = format else {
            return false;
        };
        picked.push(format.clone());
    }
    if picked.len() == 2
        && (picked[0]["vcodec"].as_str().is_none_or(|v| v == "none")
            || picked[1]["vcodec"] != "none")
    {
        return false;
    }
    let Some(root) = info.as_object_mut() else {
        return false;
    };
    // Remove stale primary-stream fields before installing this selection.
    for key in [
        "url",
        "format_id",
        "vcodec",
        "acodec",
        "width",
        "height",
        "fps",
        "protocol",
        "ext",
    ] {
        root.remove(key);
    }
    if picked.len() == 1 {
        root.remove("requested_formats");
        root.extend(picked.remove(0).as_object().unwrap().clone());
    } else {
        root.insert(
            "format_id".into(),
            Value::String(
                picked
                    .iter()
                    .filter_map(|f| f["format_id"].as_str())
                    .collect::<Vec<_>>()
                    .join("+"),
            ),
        );
        root.insert("requested_formats".into(), Value::Array(picked));
    }
    true
}

fn valid_until(info: &Value, now: SystemTime) -> bool {
    let deadline = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 60;
    let valid = |v: &Value| {
        v["url"].as_str().is_none_or(|url| {
            url.split_once('?')
                .and_then(|(_, q)| q.split('&').find_map(|part| part.strip_prefix("expire=")))
                .and_then(|v| v.parse::<u64>().ok())
                .is_none_or(|expiry| expiry > deadline)
        })
    };
    valid(info)
        && ["requested_formats", "formats"]
            .iter()
            .all(|key| info[*key].as_array().is_none_or(|fs| fs.iter().all(valid)))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn info() -> Value {
        serde_json::json!({"is_live":false,"url":"https://example.org/media","formats":[{"format_id":"251","url":"https://example.org/audio"}]})
    }
    #[test]
    fn quality_switch_uses_existing_exact_links_without_helper() {
        let mut cache = StreamCache::default();
        let s = Settings::default();
        let info = serde_json::json!({"title":"keep me","http_headers":{"Referer":"https://www.youtube.com/"},
            "formats":[
                {"format_id":"137","height":1080,"fps":60,"vcodec":"avc1","acodec":"none","protocol":"https","url":"https://example.org/1080"},
                {"format_id":"hls","height":1080,"protocol":"m3u8_native","url":"https://example.org/list"}],
            "requested_formats":[{"format_id":"140","vcodec":"none","acodec":"aac","protocol":"https","url":"https://example.org/audio"}]});
        cache.insert("video", &s, &info, Instant::now());
        let changed = Settings {
            format_override: Some("137+bestaudio".into()),
            ..s.clone()
        };
        let hit = cache.lookup("video", &changed).unwrap();
        assert!(hit.exact);
        let selected: Value = serde_json::from_slice(&hit.bytes).unwrap();
        assert_eq!(selected["requested_formats"][0]["height"], 1080);
        assert_eq!(selected["requested_formats"][0]["fps"], 60);
        assert_eq!(selected["requested_formats"][1]["format_id"], "140");
        assert_eq!(selected["title"], "keep me");
        assert_eq!(
            selected["http_headers"]["Referer"],
            "https://www.youtube.com/"
        );
        for selector in [
            "missing+bestaudio",
            "hls+bestaudio",
            "bestvideo[height<=360]+bestaudio",
        ] {
            let changed = Settings {
                format_override: Some(selector.into()),
                ..s.clone()
            };
            assert!(!cache.lookup("video", &changed).unwrap().exact);
        }
    }
    #[test]
    fn link_cache_works_without_thumbnails_and_rejects_writes_after_turning_off() {
        let s = Settings {
            cache_enabled: false,
            stream_cache_enabled: true,
            ..Settings::default()
        };
        let mut cache = StreamCache::default();
        cache.configure(s.stream_cache_enabled);
        cache.insert("video", &s, &info(), Instant::now());
        assert!(cache.lookup("video", &s).is_some());
        let old_epoch = cache.epoch();
        cache.configure(false);
        assert_ne!(cache.epoch(), old_epoch);
        cache.insert("video", &s, &info(), Instant::now());
        assert!(cache.lookup("video", &s).is_none());
        assert!(cache.entries.is_empty());
        cache.configure(true);
        assert!(cache.lookup("video", &s).is_none());
        cache.insert("video", &s, &info(), Instant::now());
        assert!(cache.lookup("video", &s).is_some());
    }
    #[test]
    fn category_clear_changes_epoch_and_removes_cached_links() {
        let mut cache = StreamCache::default();
        let settings = Settings::default();
        cache.insert("video", &settings, &info(), Instant::now());
        let epoch = cache.epoch();
        cache.clear();
        assert_ne!(cache.epoch(), epoch);
        assert!(cache.lookup("video", &settings).is_none());
    }
    #[test]
    fn cache_is_bounded_expiring_and_selector_aware() {
        let mut cache = StreamCache::default();
        let s = Settings::default();
        let started = Instant::now();
        cache.insert("video", &s, &info(), started);
        assert!(cache.lookup("video", &s).unwrap().exact);
        cache.seed("pasted", &s, &info());
        assert!(!cache.lookup("pasted", &s).unwrap().exact);
        let music = Settings {
            music: true,
            ..s.clone()
        };
        assert!(!cache.lookup("video", &music).unwrap().exact);
        cache.insert("video", &music, &info(), started);
        assert!(cache.lookup("video", &music).unwrap().exact);
        let other = Settings {
            hardware: false,
            ..s.clone()
        };
        assert!(cache.lookup("video", &other).is_none());
        cache.invalidate("video");
        assert!(cache.lookup("video", &s).is_none());
        cache.insert("old", &s, &info(), Instant::now() - TTL);
        assert!(cache.lookup("old", &s).is_none());
        for n in 0..10 {
            cache.insert(&n.to_string(), &s, &info(), Instant::now());
        }
        assert!(cache.entries.len() <= MAX_ENTRIES);
        assert!(cache.lookup("0", &s).is_none());
        let mut huge = info();
        huge["description"] = Value::String("x".repeat(MAX_BYTES));
        cache.insert("large", &s, &huge, Instant::now());
        assert!(cache.lookup("large", &s).is_none());
    }
    #[test]
    fn aggregate_limit_evicts_old_entries() {
        let s = Settings::default();
        let mut cache = StreamCache::default();
        let mut payload = info();
        payload["description"] = "x".repeat(MAX_BYTES * 3 / 5).into();
        cache.insert("a", &s, &payload, Instant::now());
        cache.insert("b", &s, &payload, Instant::now());
        assert!(cache.lookup("a", &s).is_none());
        assert!(cache.lookup("b", &s).is_some());
    }
    #[test]
    fn live_and_expiring_links_are_not_reused() {
        let mut cache = StreamCache::default();
        let s = Settings::default();
        let mut v = info();
        v["is_live"] = true.into();
        cache.insert("live", &s, &v, Instant::now());
        assert!(cache.lookup("live", &s).is_none());
        v = info();
        v["url"] = "https://example.org/media?expire=1".into();
        cache.insert("expired", &s, &v, Instant::now());
        assert!(cache.lookup("expired", &s).is_none());
        let mut different = s.clone();
        different.hardware = !s.hardware;
        cache.insert("video", &s, &info(), Instant::now());
        assert!(cache.lookup("video", &different).is_none());
    }
}
