//! Portable playlist data. Store source identities, never expiring media URLs or credentials.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub const SCHEMA: &str = "io.mochune.playlists";
pub const VERSION: u32 = 1;
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn new_id() -> String {
    format!(
        "pl-{}-{}-{}",
        now_ms(),
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub provider: String,
    pub id: String,
    #[serde(default)]
    pub uri: String,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub id: String,
    pub source: Source,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub added_at_ms: u64,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}
impl Track {
    pub fn youtube(id: &str, title: &str, artist: &str, duration: Option<f64>) -> Result<Self> {
        ensure!(valid_youtube_id(id), "Invalid YouTube video identity");
        Ok(Self {
            id: format!("youtube:{id}"),
            source: Source {
                provider: "youtube".into(),
                id: id.into(),
                uri: format!("https://www.youtube.com/watch?v={id}"),
                extra: Map::new(),
            },
            title: title.into(),
            artist: artist.into(),
            duration_seconds: duration,
            added_at_ms: now_ms(),
            extra: Map::new(),
        })
    }
    pub fn youtube_id(&self) -> Option<&str> {
        (self.source.provider == "youtube" && valid_youtube_id(&self.source.id))
            .then_some(self.source.id.as_str())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub created_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Library {
    pub schema: String,
    pub version: u32,
    #[serde(default)]
    pub playlists: Vec<Playlist>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}
impl Library {
    pub fn empty() -> Self {
        Self {
            schema: SCHEMA.into(),
            version: VERSION,
            playlists: Vec::new(),
            extra: Map::new(),
        }
    }
}
impl Default for Library {
    fn default() -> Self {
        let mut lib = Self::empty();
        let id = lib.create("Liked songs").expect("built-in playlist name");
        lib.playlists.iter_mut().find(|p| p.id == id).unwrap().id = "liked-songs".into();
        lib
    }
}
impl Library {
    /// Restore the reserved music collection in older or imported libraries.
    pub fn ensure_liked_songs(&mut self) {
        let mut reserved =
            if let Some(i) = self.playlists.iter().position(|p| p.id == "liked-songs") {
                self.playlists.remove(i)
            } else {
                Library::default().playlists.remove(0)
            };
        reserved.name = "Liked songs".into();
        reserved
            .tracks
            .sort_by_key(|t| std::cmp::Reverse(t.added_at_ms));
        self.playlists.insert(0, reserved);
    }
    /// Local likes are ordinary portable playlist entries, with a protected collection.
    pub fn toggle_liked_song(&mut self, mut track: Track) -> Result<bool> {
        validate_track(&track)?;
        self.ensure_liked_songs();
        let position = self.get("liked-songs")?.tracks.iter().position(|old| {
            old.source.provider == track.source.provider && old.source.id == track.source.id
        });
        if let Some(i) = position {
            self.remove("liked-songs", i)?;
            Ok(false)
        } else {
            track.added_at_ms = now_ms();
            self.add("liked-songs", track)
        }
    }

    pub fn get(&self, id: &str) -> Result<&Playlist> {
        self.playlists
            .iter()
            .find(|p| p.id == id)
            .context("Playlist no longer exists")
    }
    fn get_mut(&mut self, id: &str) -> Result<&mut Playlist> {
        self.playlists
            .iter_mut()
            .find(|p| p.id == id)
            .context("Playlist no longer exists")
    }
    pub fn create(&mut self, name: &str) -> Result<String> {
        let name = checked_name(name)?;
        let id = new_id();
        let time = now_ms();
        self.playlists.push(Playlist {
            id: id.clone(),
            name,
            description: String::new(),
            created_at_ms: time,
            updated_at_ms: time,
            tracks: Vec::new(),
            extra: Map::new(),
        });
        Ok(id)
    }
    pub fn rename(&mut self, id: &str, name: &str) -> Result<()> {
        ensure!(id != "liked-songs", "Liked songs is a permanent playlist");
        let name = checked_name(name)?;
        let p = self.get_mut(id)?;
        p.name = name;
        p.updated_at_ms = now_ms();
        Ok(())
    }
    pub fn delete(&mut self, id: &str) -> Result<()> {
        ensure!(id != "liked-songs", "Liked songs is a permanent playlist");
        self.get(id)?;
        self.playlists.retain(|p| p.id != id);
        Ok(())
    }
    pub fn add(&mut self, id: &str, track: Track) -> Result<bool> {
        validate_track(&track)?;
        let p = self.get_mut(id)?;
        if p.tracks
            .iter()
            .any(|t| t.source.provider == track.source.provider && t.source.id == track.source.id)
        {
            return Ok(false);
        }
        if id == "liked-songs" {
            // Put a fresh like first even when timestamps share the same millisecond.
            p.tracks.insert(0, track);
            p.tracks.sort_by_key(|t| std::cmp::Reverse(t.added_at_ms));
        } else {
            p.tracks.push(track);
        }
        p.updated_at_ms = now_ms();
        Ok(true)
    }
    pub fn remove(&mut self, id: &str, index: usize) -> Result<()> {
        let p = self.get_mut(id)?;
        ensure!(index < p.tracks.len(), "Track no longer exists");
        p.tracks.remove(index);
        p.updated_at_ms = now_ms();
        Ok(())
    }
    pub fn move_track(&mut self, id: &str, index: usize, delta: i32) -> Result<()> {
        ensure!(id != "liked-songs", "Liked songs always sorts newest first");
        let p = self.get_mut(id)?;
        let target = index as i64 + i64::from(delta);
        ensure!(
            index < p.tracks.len() && target >= 0 && target < (p.tracks.len() as i64),
            "Track is already at the edge"
        );
        p.tracks.swap(index, target as usize);
        p.updated_at_ms = now_ms();
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (self.schema == SCHEMA || self.schema == "io.ducktube.playlists" || self.schema == "io.ytlite.playlists"),
            "This file is not a Mochune playlist library"
        );
        ensure!(
            self.version == VERSION,
            "Playlist version {} is unsupported; your existing library was kept",
            self.version
        );
        ensure!(self.playlists.len() <= 1000, "Too many playlists");
        let mut ids = std::collections::HashSet::new();
        let mut count = 0;
        for p in &self.playlists {
            ensure!(
                !p.id.is_empty() && p.id.len() <= 4096 && ids.insert(&p.id),
                "Invalid or duplicate playlist identity"
            );
            checked_name(&p.name)?;
            count += p.tracks.len();
            ensure!(count <= 100_000, "Too many tracks");
            let mut tracks = std::collections::HashSet::new();
            for t in &p.tracks {
                validate_track(t)?;
                ensure!(
                    tracks.insert((&t.source.provider, &t.source.id)),
                    "Duplicate track in playlist"
                );
            }
        }
        Ok(())
    }
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_BYTES, "Playlist file exceeds 8 MiB");
        let header: Value = serde_json::from_slice(bytes).context("Invalid playlist JSON")?;
        ensure!(
            (header["schema"] == SCHEMA || header["schema"] == "io.ducktube.playlists" || header["schema"] == "io.ytlite.playlists"),
            "This file is not a Mochune playlist library"
        );
        let version = header["version"]
            .as_u64()
            .context("Playlist version is missing")?;
        ensure!(
            version == u64::from(VERSION),
            "Playlist version {version} is unsupported; your existing library was kept"
        );
        let mut lib: Self = serde_json::from_value(header)?;
        lib.validate()?;
        lib.schema = SCHEMA.into();
        Ok(lib)
    }
    pub fn to_json(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self)?;
        ensure!(bytes.len() <= MAX_BYTES, "Library exceeds the 8 MiB limit");
        Ok(bytes)
    }
    pub fn selected(&self, id: &str) -> Result<Self> {
        Ok(Self {
            playlists: vec![self.get(id)?.clone()],
            ..self.clone()
        })
    }
    /// Import is additive and idempotent. Existing names/metadata win on conflicts.
    pub fn merge(&mut self, incoming: Self) -> Result<usize> {
        incoming.validate()?;
        let mut draft = self.clone();
        let mut added = 0;
        for (key, value) in incoming.extra {
            draft.extra.entry(key).or_insert(value);
        }
        for p in incoming.playlists {
            if let Some(existing) = draft.playlists.iter_mut().find(|old| old.id == p.id) {
                for (key, value) in p.extra {
                    existing.extra.entry(key).or_insert(value);
                }
                for track in p.tracks {
                    if let Some(old) = existing.tracks.iter_mut().find(|t| {
                        t.source.provider == track.source.provider && t.source.id == track.source.id
                    }) {
                        for (key, value) in track.extra {
                            old.extra.entry(key).or_insert(value);
                        }
                        for (key, value) in track.source.extra {
                            old.source.extra.entry(key).or_insert(value);
                        }
                    } else {
                        existing.tracks.push(track);
                        added += 1;
                    }
                }
            } else {
                added += p.tracks.len();
                draft.playlists.push(p);
            }
        }
        draft.to_json()?;
        *self = draft;
        Ok(added)
    }
}
fn checked_name(name: &str) -> Result<String> {
    let name = name.trim();
    ensure!(
        !name.is_empty() && name.chars().count() <= 120,
        "Playlist name must contain 1–120 characters"
    );
    Ok(name.into())
}
pub fn valid_youtube_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn validate_track(t: &Track) -> Result<()> {
    ensure!(
        !t.id.is_empty()
            && t.id.len() <= 4096
            && !t.source.provider.is_empty()
            && t.source.provider.len() <= 100
            && !t.source.id.is_empty()
            && t.source.id.len() <= 4096,
        "Invalid track identity"
    );
    ensure!(
        t.title.len() <= 4096
            && t.artist.len() <= 4096
            && t.source.uri.len() <= 8192
            && !t.source.uri.contains(['\n', '\r']),
        "Track metadata is too large or invalid"
    );
    ensure!(
        t.duration_seconds.is_none_or(|n| n.is_finite() && n >= 0.),
        "Invalid track duration"
    );
    if t.source.provider == "youtube" {
        ensure!(valid_youtube_id(&t.source.id), "Invalid YouTube identity");
    }
    Ok(())
}
/// Normalize pasted desktop/mobile/share links without accepting lookalike hosts.
pub fn normalize_youtube_uri(input: &str) -> Option<String> {
    let input = input.trim().trim_matches(['<', '>']);
    if input.len() > 4096 || input.chars().any(char::is_whitespace) {
        return None;
    }
    let candidate = if input.starts_with("//") {
        format!("https:{input}")
    } else if !input.contains("://") {
        format!("https://{input}")
    } else {
        input.to_owned()
    };
    let mut uri = url::Url::parse(&candidate).ok()?;
    if !matches!(uri.scheme(), "https" | "http")
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.port().is_some()
        || !matches!(
            uri.host_str()?,
            "youtube.com"
                | "www.youtube.com"
                | "m.youtube.com"
                | "music.youtube.com"
                | "youtu.be"
                | "www.youtu.be"
                | "youtube-nocookie.com"
                | "www.youtube-nocookie.com"
        )
    {
        return None;
    }
    uri.set_scheme("https").ok()?;
    Some(uri.to_string())
}

pub fn youtube_id_from_uri(uri: &str) -> Option<String> {
    let uri = url::Url::parse(&normalize_youtube_uri(uri)?).ok()?;
    let id = match uri.host_str()? {
        "youtu.be" | "www.youtu.be" => uri.path_segments()?.next()?.to_owned(),
        _ if uri.path() == "/watch" => uri
            .query_pairs()
            .find(|(key, _)| key == "v")?
            .1
            .into_owned(),
        _ => {
            let mut segments = uri.path_segments()?;
            if !matches!(segments.next()?, "shorts" | "embed" | "live" | "v") {
                return None;
            }
            segments.next()?.to_owned()
        }
    };
    valid_youtube_id(&id).then_some(id)
}
pub fn import_file(path: &Path) -> Result<Library> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "Playlist file exceeds 8 MiB");
    if path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("m3u") || s.eq_ignore_ascii_case("m3u8"))
    {
        let text = std::str::from_utf8(&bytes).context("M3U playlists must be UTF-8")?;
        from_m3u(
            text,
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Imported playlist"),
        )
    } else {
        Library::from_json(&bytes)
    }
}
pub fn from_m3u(text: &str, name: &str) -> Result<Library> {
    ensure!(text.len() <= MAX_BYTES, "Playlist file exceeds 8 MiB");
    let mut lib = Library {
        playlists: Vec::new(),
        ..Library::default()
    };
    let id = lib.create(name)?;
    let mut title = None;
    let mut duration = None;
    for line in text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            let (seconds, label) = info.split_once(',').unwrap_or((info, ""));
            title = Some(label.to_owned());
            duration = seconds
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n >= 0.);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let label = title
            .take()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| line.into());
        let track = if let Some(video) = youtube_id_from_uri(line) {
            Track::youtube(&video, &label, "", duration.take())?
        } else {
            Track {
                id: format!("uri:{line}"),
                source: Source {
                    provider: "uri".into(),
                    id: line.into(),
                    uri: line.into(),
                    extra: Map::new(),
                },
                title: label,
                artist: String::new(),
                duration_seconds: duration.take(),
                added_at_ms: now_ms(),
                extra: Map::new(),
            }
        };
        lib.add(&id, track)?;
    }
    lib.validate()?;
    Ok(lib)
}
pub fn to_m3u(p: &Playlist) -> Result<String> {
    let mut text = String::from("#EXTM3U\n");
    for t in &p.tracks {
        validate_track(t)?;
        ensure!(
            !t.source.uri.is_empty(),
            "Track has no portable URI; export JSON instead"
        );
        let title = if t.artist.is_empty() {
            t.title.clone()
        } else {
            format!("{} — {}", t.artist, t.title)
        }
        .replace(['\r', '\n'], " ");
        text.push_str(&format!(
            "#EXTINF:{},{}\n{}\n",
            t.duration_seconds.map(|n| n.round() as i64).unwrap_or(-1),
            title,
            t.source.uri
        ));
    }
    Ok(text)
}
#[derive(Clone)]
pub struct Store {
    pub path: PathBuf,
}
impl Store {
    pub fn load(&self) -> Result<Library> {
        self.load_or(Library::default)
    }
    pub fn load_or(&self, default: impl FnOnce() -> Library) -> Result<Library> {
        if self.path.exists() {
            return import_file(&self.path);
        }
        let backup = self.path.with_extension("json.bak");
        if backup.exists() {
            return import_file(&backup).context("Backup library could not be read");
        }
        Ok(default())
    }
    pub fn save(&self, library: &Library) -> Result<()> {
        write_atomic(&self.path, &library.to_json()?, true)
    }
}
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub fn write_atomic(path: &Path, bytes: &[u8], backup: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let temp = TempFile(parent.join(format!(
        ".mochune-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp.0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if backup && path.exists() {
        let saved = TempFile(parent.join(format!(
            ".mochune-backup-{}-{}.tmp",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )));
        fs::copy(path, &saved.0)?;
        fs::OpenOptions::new()
            .write(true)
            .open(&saved.0)?
            .sync_all()
            .context("Cannot flush playlist backup")?;
        replace(&saved.0, &path.with_extension("json.bak"))?;
    }
    replace(&temp.0, path)?;
    #[cfg(unix)]
    {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
#[cfg(not(target_os = "windows"))]
fn replace(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)?;
    Ok(())
}
#[cfg(target_os = "windows")]
fn replace(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both paths are NUL-terminated and remain alive for this synchronous call.
    ensure!(
        unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0,
        "Cannot replace playlist file: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn liked_songs_stays_newest_first_after_add_and_legacy_restore() {
        let mut lib = Library::default();
        for (id, time) in [
            ("aaaaaaaaaaa", 10),
            ("bbbbbbbbbbb", 30),
            ("ccccccccccc", 20),
        ] {
            let mut track = Track::youtube(id, id, "Artist", Some(120.)).unwrap();
            track.added_at_ms = time;
            lib.add("liked-songs", track).unwrap();
        }
        let ids = |lib: &Library| {
            lib.get("liked-songs")
                .unwrap()
                .tracks
                .iter()
                .map(|t| t.source.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&lib), ["bbbbbbbbbbb", "ccccccccccc", "aaaaaaaaaaa"]);
        assert!(lib.move_track("liked-songs", 0, 1).is_err());
        lib.get_mut("liked-songs").unwrap().tracks.reverse();
        lib.ensure_liked_songs();
        assert_eq!(ids(&lib), ["bbbbbbbbbbb", "ccccccccccc", "aaaaaaaaaaa"]);
        let mut same_time = Track::youtube("ddddddddddd", "New", "Artist", Some(120.)).unwrap();
        same_time.added_at_ms = 30;
        lib.add("liked-songs", same_time).unwrap();
        assert_eq!(ids(&lib)[0], "ddddddddddd");
    }

    #[test]
    fn liking_an_old_playlist_entry_uses_time_of_like() {
        let mut lib = Library::default();
        let mut old = Track::youtube("aaaaaaaaaaa", "Old", "Artist", Some(120.)).unwrap();
        old.added_at_ms = 1;
        let mut recent = old.clone();
        recent.source.id = "bbbbbbbbbbb".into();
        recent.added_at_ms = now_ms().saturating_sub(1000);
        lib.add("liked-songs", recent).unwrap();
        lib.toggle_liked_song(old).unwrap();
        assert_eq!(
            lib.get("liked-songs").unwrap().tracks[0].source.id,
            "aaaaaaaaaaa"
        );
        lib.ensure_liked_songs();
        assert_eq!(
            lib.get("liked-songs").unwrap().tracks[0].source.id,
            "aaaaaaaaaaa"
        );
    }
    #[test]
    fn repeated_library_edits_keep_the_last_complete_backup() {
        let root =
            std::env::temp_dir().join(format!("mochune-writable-backup-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let store = super::Store {
            path: root.join("library.json"),
        };
        let mut library = super::Library::default();
        store.save(&library).unwrap();
        let id = library.create("New playlist").unwrap();
        store.save(&library).unwrap();
        library.delete(&id).unwrap();
        store.save(&library).unwrap();
        assert_eq!(store.load().unwrap().playlists.len(), 1);
        let backup = std::fs::read(root.join("library.json.bak")).unwrap();
        assert_eq!(
            super::Library::from_json(&backup).unwrap().playlists.len(),
            2
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_library_import_keeps_tracks_and_exports_new_brand() {
        let original = super::Library::default();
        let mut old = serde_json::to_value(&original).unwrap();
        old["schema"] = "io.ytlite.playlists".into();
        let loaded = super::Library::from_json(&serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(loaded.playlists, original.playlists);
        let exported: serde_json::Value =
            serde_json::from_slice(&loaded.to_json().unwrap()).unwrap();
        assert_eq!(exported["schema"], super::SCHEMA);
    }

    use super::*;
    fn song(id: &str) -> Track {
        Track::youtube(id, "Song 🎵", "Artist", Some(123.)).unwrap()
    }
    #[test]
    fn json_roundtrip_preserves_extensions_and_order() {
        let mut lib = Library::default();
        lib.extra
            .insert("future".into(), serde_json::json!({"v":2}));
        let id = lib.create("Ordered playlist").unwrap();
        let mut track = song("abcdefghijk");
        track.extra.insert("album".into(), "Future album".into());
        track.source.extra.insert("region".into(), "future".into());
        lib.add(&id, track).unwrap();
        lib.add(&id, song("lmnopqrstuv")).unwrap();
        lib.move_track(&id, 1, -1).unwrap();
        let decoded = Library::from_json(&lib.to_json().unwrap()).unwrap();
        assert_eq!(lib, decoded);
        assert_eq!(decoded.get(&id).unwrap().tracks[0].source.id, "lmnopqrstuv");
    }
    #[test]
    fn import_is_additive_idempotent_and_rejects_future_versions() {
        let mut lib = Library::default();
        let id = lib.playlists[0].id.clone();
        lib.add(&id, song("abcdefghijk")).unwrap();
        let mut imported = lib.clone();
        imported.add(&id, song("lmnopqrstuv")).unwrap();
        assert_eq!(lib.merge(imported.clone()).unwrap(), 1);
        assert_eq!(lib.merge(imported).unwrap(), 0);
        let saved = lib.clone();
        let bad = serde_json::json!({"schema":SCHEMA,"version":2,"playlists":[]});
        assert!(Library::from_json(&serde_json::to_vec(&bad).unwrap()).is_err());
        assert_eq!(lib, saved);
    }
    #[test]
    fn pasted_youtube_links_share_one_identity() {
        for uri in [
            " https://youtu.be/abcdefghijk?si=share ",
            "youtube.com/watch?v=abcdefghijk&t=30s",
            "www.youtube.com/watch?list=PL123&v=abcdefghijk#details",
            "HTTPS://M.YouTube.com/shorts/abcdefghijk",
            "music.youtube.com/watch?v=abcdefghijk",
            "//www.youtube.com/live/abcdefghijk",
            "https://www.youtube-nocookie.com/embed/abcdefghijk",
            "<https://www.youtube.com/watch?%76=abcdefghijk>",
        ] {
            assert_eq!(
                youtube_id_from_uri(uri).as_deref(),
                Some("abcdefghijk"),
                "{uri}"
            );
        }
        for uri in [
            "https://youtube.com.evil.test/watch?v=abcdefghijk",
            "https://youtube.com@evil.test/watch?v=abcdefghijk",
            "https://evil.test@youtube.com/watch?v=abcdefghijk",
            "ftp://youtube.com/watch?v=abcdefghijk",
            "https://youtube.com:123/watch?v=abcdefghijk",
            "https://youtu.be/short",
            "https://youtube.com/watch?v=abcdefghijk extra",
        ] {
            assert!(youtube_id_from_uri(uri).is_none(), "{uri}");
        }
    }
    #[test]
    fn m3u_is_portable_and_keeps_unknown_sources() {
        let lib=from_m3u("#EXTM3U\n#EXTINF:120,A song\nhttps://youtu.be/abcdefghijk\n#EXTINF:-1,Other\nhttps://example.org/music.ogg\n","Imported").unwrap();
        assert_eq!(lib.playlists[0].tracks[0].youtube_id(), Some("abcdefghijk"));
        assert!(lib.playlists[0].tracks[1].youtube_id().is_none());
        let text = to_m3u(&lib.playlists[0]).unwrap();
        assert!(text.contains("https://www.youtube.com/watch?v=abcdefghijk"));
        assert!(text.contains("https://example.org/music.ogg"));
        assert!(youtube_id_from_uri("https://youtube.com.evil.test/watch?v=abcdefghijk").is_none());
    }
    #[test]
    fn liked_songs_survives_migration_and_cannot_be_deleted_or_renamed() {
        let mut lib = Library::empty();
        lib.create("Custom").unwrap();
        lib.ensure_liked_songs();
        assert_eq!(lib.playlists[0].id, "liked-songs");
        assert!(lib.delete("liked-songs").is_err());
        assert!(lib.rename("liked-songs", "Other").is_err());
        let track = Track::youtube("abcdefghijk", "Song", "Artist", Some(120.)).unwrap();
        assert!(lib.toggle_liked_song(track.clone()).unwrap());
        lib.ensure_liked_songs();
        assert_eq!(lib.get("liked-songs").unwrap().tracks.len(), 1);
        let imported = Library::from_json(&lib.to_json().unwrap()).unwrap();
        assert_eq!(
            imported.get("liked-songs").unwrap().tracks,
            lib.get("liked-songs").unwrap().tracks
        );
        assert!(!lib.toggle_liked_song(track).unwrap());
        assert!(lib.get("liked-songs").unwrap().tracks.is_empty());
        assert_eq!(lib.playlists.len(), 2);
    }
    #[test]
    fn persistence_has_backup_and_failure_keeps_original() {
        let dir = std::env::temp_dir().join(new_id());
        fs::create_dir_all(&dir).unwrap();
        let store = Store {
            path: dir.join("library.json"),
        };
        let mut lib = Library::default();
        store.save(&lib).unwrap();
        let original = fs::read(&store.path).unwrap();
        lib.create("Another playlist").unwrap();
        store.save(&lib).unwrap();
        assert_eq!(
            fs::read(store.path.with_extension("json.bak")).unwrap(),
            original
        );
        assert_eq!(store.load().unwrap(), lib);
        let mut future = lib.clone();
        future.version = 2;
        assert!(store.save(&future).is_err());
        assert_eq!(store.load().unwrap(), lib);
        fs::remove_file(&store.path).unwrap();
        assert_eq!(
            store.load().unwrap(),
            Library::from_json(&original).unwrap()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
