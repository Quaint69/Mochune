//! Completed compressed audio only. No decoded samples or whole media files in RAM.
use super::*;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Weak, atomic::AtomicBool},
};
const MAX_INDEX: u64 = 2 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    video: Video,
    bytes: u64,
    used: u64,
}
#[derive(Default, Serialize, Deserialize)]
struct Index {
    entries: BTreeMap<String, Entry>,
}
#[derive(Default)]
struct Inner {
    index: Index,
    pins: HashMap<String, usize>,
    retired: HashMap<String, u64>,
}
pub(crate) struct Store {
    root: PathBuf,
    enabled: AtomicBool,
    epoch: AtomicU64,
    limit: AtomicU64,
    inner: Mutex<Inner>,
    captures: Mutex<Vec<Weak<Mutex<Capture>>>>,
    window: Mutex<Option<slint::Weak<SetupWindow>>>,
}
fn valid_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl Store {
    pub(super) fn open(root: PathBuf) -> Arc<Self> {
        let mut index: Index = fs::File::open(root.join("index.json"))
            .ok()
            .and_then(|f| {
                let mut bytes = Vec::new();
                f.take(MAX_INDEX + 1).read_to_end(&mut bytes).ok()?;
                (bytes.len() as u64 <= MAX_INDEX)
                    .then(|| serde_json::from_slice(&bytes).ok())
                    .flatten()
            })
            .unwrap_or_default();
        index.entries.retain(|id, e| {
            valid_id(id)
                && e.video.id == *id
                && e.bytes > 0
                && fs::metadata(root.join(format!("{id}.audio")))
                    .is_ok_and(|m| m.is_file() && m.len() == e.bytes)
        });
        // Neither interrupted transfers nor orphaned files from a crash are published.
        if let Ok(files) = fs::read_dir(&root) {
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                if name.ends_with(".part")
                    || name
                        .strip_suffix(".audio")
                        .is_some_and(|id| !index.entries.contains_key(id))
                {
                    let _ = fs::remove_file(file.path());
                }
            }
        }
        Arc::new(Self {
            root,
            enabled: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            limit: AtomicU64::new(256 * 1024 * 1024),
            inner: Mutex::new(Inner {
                index,
                ..Inner::default()
            }),
            captures: Mutex::new(Vec::new()),
            window: Mutex::new(None),
        })
    }
    pub(super) fn install(&self, w: &SetupWindow) {
        *self.window.lock().unwrap() = Some(w.as_weak());
    }
    pub(super) fn notice(&self, text: String) {
        if let Some(w) = self.window.lock().unwrap().as_ref() {
            let count = self.count() as i32;
            ui(w, move |w| {
                w.set_downloaded_count(count);
                w.set_cache_status(
                    if w.get_audio_cache_enabled() || text.starts_with("Music cache:") {
                        text
                    } else {
                        "Offline music saving is off.".into()
                    }
                    .into(),
                );
            });
        }
    }
    fn media(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.audio"))
    }
    fn save(&self, inner: &Inner) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let bytes = serde_json::to_vec(&inner.index)?;
        anyhow::ensure!(bytes.len() as u64 <= MAX_INDEX, "Music cache index is full");
        crate::playlists::write_atomic(&self.root.join("index.json"), &bytes, false)
    }
    pub(super) fn configure(&self, enabled: bool, limit_mib: u64) -> Result<()> {
        self.enabled.store(enabled, Ordering::Release);
        self.limit
            .store(limit_mib.clamp(128, 1024) * 1024 * 1024, Ordering::Release);
        if !enabled {
            self.clear()?;
        } else {
            let mut inner = self.inner.lock().unwrap();
            self.trim(&mut inner, 0)?;
            self.save(&inner)?;
        }
        Ok(())
    }
    pub(super) fn count(&self) -> usize {
        self.inner.lock().unwrap().index.entries.len()
    }
    pub(super) fn contains(&self, id: &str) -> bool {
        self.inner.lock().unwrap().index.entries.contains_key(id)
    }
    pub(super) fn list(&self) -> Vec<Video> {
        let inner = self.inner.lock().unwrap();
        let mut entries = inner.index.entries.values().collect::<Vec<_>>();
        entries.sort_by_key(|e| std::cmp::Reverse(e.used));
        entries.into_iter().map(|e| e.video.clone()).collect()
    }
    pub(super) fn summary(&self) -> String {
        let i = self.inner.lock().unwrap();
        let bytes: u64 = i.index.entries.values().map(|e| e.bytes).sum();
        format!(
            "{} completed {} · {:.1} MiB saved",
            i.index.entries.len(),
            if i.index.entries.len() == 1 {
                "track"
            } else {
                "tracks"
            },
            bytes as f64 / 1048576.
        )
    }
    pub(super) fn lookup(self: &Arc<Self>, id: &str) -> Option<Lease> {
        let mut inner = self.inner.lock().unwrap();
        let e = inner.index.entries.get_mut(id)?;
        let path = self.media(id);
        if !fs::metadata(&path).is_ok_and(|m| m.len() == e.bytes) {
            inner.index.entries.remove(id);
            let _ = self.save(&inner);
            return None;
        }
        e.used = now();
        let video = e.video.clone();
        *inner.pins.entry(id.into()).or_default() += 1;
        Some(Lease {
            store: self.clone(),
            id: id.into(),
            path,
            video,
        })
    }
    pub(super) fn clear(&self) -> Result<()> {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        let captures = {
            let mut c = self.captures.lock().unwrap();
            let live = c.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
            c.clear();
            live
        };
        for c in captures {
            c.lock().unwrap().abort();
        }
        let mut inner = self.inner.lock().unwrap();
        let entries = std::mem::take(&mut inner.index.entries);
        for (id, e) in entries {
            if inner.pins.contains_key(&id) {
                inner.retired.insert(id, e.bytes);
            } else if let Err(error) = fs::remove_file(self.media(&id)) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    inner.retired.insert(id, e.bytes);
                }
            }
        }
        let retired = inner
            .retired
            .keys()
            .filter(|id| !inner.pins.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in retired {
            if fs::remove_file(self.media(&id)).is_ok() || !self.media(&id).exists() {
                inner.retired.remove(&id);
            }
        }
        self.save(&inner)?;
        anyhow::ensure!(
            inner.retired.keys().all(|id| inner.pins.contains_key(id)),
            "Some music files could not be deleted; try again after playback stops"
        );
        Ok(())
    }
    pub(super) fn invalidate(&self, id: &str) {
        let mut i = self.inner.lock().unwrap();
        if let Some(e) = i.index.entries.remove(id) {
            if i.pins.contains_key(id) {
                i.retired.insert(id.into(), e.bytes);
            } else {
                let _ = fs::remove_file(self.media(id));
            }
            let _ = self.save(&i);
        }
    }
    fn used(inner: &Inner) -> u64 {
        inner.index.entries.values().map(|e| e.bytes).sum::<u64>()
            + inner.retired.values().sum::<u64>()
    }
    fn trim(&self, inner: &mut Inner, reserve: u64) -> Result<()> {
        let mut old = inner
            .index
            .entries
            .iter()
            .filter(|(id, _)| !inner.pins.contains_key(*id))
            .map(|(id, e)| (id.clone(), e.used))
            .collect::<Vec<_>>();
        old.sort_by_key(|(_, t)| *t);
        for (id, _) in old {
            if Self::used(inner).saturating_add(reserve) <= self.limit.load(Ordering::Acquire) {
                break;
            }
            fs::remove_file(self.media(&id))?;
            inner.index.entries.remove(&id);
        }
        anyhow::ensure!(
            Self::used(inner).saturating_add(reserve) <= self.limit.load(Ordering::Acquire),
            "Music cache is full while a saved track is playing"
        );
        Ok(())
    }
    pub(super) fn begin(
        self: &Arc<Self>,
        video: Video,
        expected: Option<u64>,
    ) -> Option<Arc<Mutex<Capture>>> {
        if !self.enabled.load(Ordering::Acquire)
            || !valid_id(&video.id)
            || self.contains(&video.id)
            || self.inner.lock().unwrap().pins.contains_key(&video.id)
        {
            return None;
        }
        if expected.is_some_and(|n| n == 0 || n > self.limit.load(Ordering::Acquire)) {
            self.notice("Track exceeds the music cache limit; streaming only.".into());
            return None;
        }
        if std::env::var_os("MOCHUNE_CACHE_DEBUG").is_some() {
            eprintln!("Audio cache begin expected={expected:?}");
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        fs::create_dir_all(&self.root).ok()?;
        let part = self.root.join(format!(
            "{}-{}-{}.part",
            video.id,
            std::process::id(),
            NEXT_PROCESS.fetch_add(1, Ordering::Relaxed)
        ));
        let file = private_file(&part).ok()?;
        let capture = Arc::new(Mutex::new(Capture {
            store: self.clone(),
            video,
            part,
            file: Some(std::io::BufWriter::with_capacity(32 * 1024, file)),
            written: 0,
            expected,
            epoch,
            done: false,
            eof: false,
            playable: false,
        }));
        let mut captures = self.captures.lock().unwrap();
        captures.retain(|c| c.strong_count() > 0);
        captures.push(Arc::downgrade(&capture));
        Some(capture)
    }
}
pub(crate) struct Lease {
    store: Arc<Store>,
    id: String,
    pub(super) path: PathBuf,
    pub(super) video: Video,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut i = self.store.inner.lock().unwrap();
        if let Some(n) = i.pins.get_mut(&self.id) {
            *n -= 1;
            if *n == 0 {
                i.pins.remove(&self.id);
                if i.retired.remove(&self.id).is_some() {
                    let _ = fs::remove_file(self.store.media(&self.id));
                }
            }
        }
        let _ = self.store.save(&i);
    }
}
pub(crate) struct Capture {
    store: Arc<Store>,
    video: Video,
    part: PathBuf,
    file: Option<std::io::BufWriter<fs::File>>,
    written: u64,
    expected: Option<u64>,
    epoch: u64,
    done: bool,
    eof: bool,
    playable: bool,
}
impl Capture {
    pub(crate) fn append(&mut self, offset: u64, bytes: &[u8], total: Option<u64>) {
        if self.done || self.file.is_none() {
            return;
        }
        if self.written == 0 && std::env::var_os("MOCHUNE_CACHE_DEBUG").is_some() {
            eprintln!("Audio cache first offset={offset} total={total:?}");
        }
        if self.expected.is_none() {
            self.expected = total;
        }
        if !self.store.enabled.load(Ordering::Acquire)
            || self.epoch != self.store.epoch.load(Ordering::Acquire)
            || offset != self.written
            || self.written.saturating_add(bytes.len() as u64)
                > self
                    .expected
                    .unwrap_or(self.store.limit.load(Ordering::Acquire))
                    .min(self.store.limit.load(Ordering::Acquire))
        {
            self.abort();
            return;
        }
        if self.file.as_mut().unwrap().write_all(bytes).is_err() {
            self.abort();
            return;
        }
        self.written += bytes.len() as u64;
    }
    pub(crate) fn needs_size(&self) -> bool {
        self.expected.is_none() && !self.done
    }
    pub(crate) fn approve(&mut self) {
        self.playable = true;
        self.try_finish();
    }
    pub(crate) fn finish(&mut self) {
        self.eof = true;
        self.try_finish();
    }
    fn try_finish(&mut self) {
        if !self.eof {
            return;
        }
        if std::env::var_os("MOCHUNE_CACHE_DEBUG").is_some() {
            eprintln!(
                "Audio cache EOS written={} expected={:?} done={}",
                self.written, self.expected, self.done
            );
        }
        if self.done || self.file.is_none() {
            return;
        }
        if self.expected != Some(self.written) || self.written == 0 {
            self.abort();
            return;
        }
        if !self.playable {
            return;
        }
        let result: Result<()> = (|| {
            let mut file = self.file.take().unwrap();
            file.flush()?;
            file.get_ref().sync_all()?;
            drop(file);
            let mut inner = self.store.inner.lock().unwrap();
            anyhow::ensure!(
                self.store.enabled.load(Ordering::Acquire)
                    && self.epoch == self.store.epoch.load(Ordering::Acquire),
                "Cache cleared during transfer"
            );
            anyhow::ensure!(
                inner.index.entries.len() < 4096
                    && !inner.index.entries.contains_key(&self.video.id)
                    && !inner.retired.contains_key(&self.video.id),
                "Track already saved or index full"
            );
            self.store.trim(&mut inner, self.written)?;
            fs::rename(&self.part, self.store.media(&self.video.id))?;
            inner.index.entries.insert(
                self.video.id.clone(),
                Entry {
                    video: self.video.clone(),
                    bytes: self.written,
                    used: now(),
                },
            );
            if let Err(e) = self.store.save(&inner) {
                inner.index.entries.remove(&self.video.id);
                let _ = fs::remove_file(self.store.media(&self.video.id));
                return Err(e);
            }
            Ok(())
        })();
        self.done = true;
        if result.is_ok() {
            self.store
                .notice(format!("Available offline: {}", self.video.title));
            if let Some(w) = self.store.window.lock().unwrap().as_ref() {
                let id = self.video.id.clone();
                ui(w, move |w| {
                    let model = w.get_videos();
                    for n in 0..model.row_count() {
                        if let Some(mut row) = model.row_data(n)
                            && row.source_id == id
                        {
                            row.downloaded = true;
                            model.set_row_data(n, row);
                        }
                    }
                });
            }
        } else {
            let _ = fs::remove_file(&self.part);
            self.store
                .notice("Music could not be cached; streaming playback is unaffected.".into());
        }
    }
    pub(crate) fn abort(&mut self) {
        if std::env::var_os("MOCHUNE_CACHE_DEBUG").is_some() {
            eprintln!(
                "Audio cache aborted written={} expected={:?}",
                self.written, self.expected
            );
        }
        self.file.take();
        let _ = fs::remove_file(&self.part);
        self.done = true;
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        if !self.done {
            self.abort();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> Arc<Store> {
        let root = std::env::temp_dir().join(format!("audio-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let s = Store::open(root);
        s.configure(true, 128).unwrap();
        s
    }
    fn video() -> Video {
        Video {
            id: "abcdefghijk".into(),
            title: "Test song".into(),
            ..Video::default()
        }
    }
    #[test]
    fn only_complete_contiguous_files_are_published() {
        let s = fixture("complete");
        let c = s.begin(video(), Some(6)).unwrap();
        c.lock().unwrap().append(0, b"abc", None);
        assert!(!s.contains("abcdefghijk"));
        c.lock().unwrap().approve();
        c.lock().unwrap().finish();
        assert!(!s.contains("abcdefghijk"));
        drop(c);
        let c = s.begin(video(), Some(6)).unwrap();
        c.lock().unwrap().append(0, b"abcdef", None);
        c.lock().unwrap().approve();
        c.lock().unwrap().finish();
        assert_eq!(
            fs::read(s.lookup("abcdefghijk").unwrap().path.clone()).unwrap(),
            b"abcdef"
        );
        let reopened = Store::open(s.root.clone());
        assert_eq!(reopened.list().len(), 1);
        fs::remove_dir_all(&s.root).unwrap();
    }
    #[test]
    fn limit_evicts_old_files_but_preserves_active_playback() {
        let s = fixture("limit");
        s.limit.store(5, Ordering::Release);
        let c = s.begin(video(), Some(3)).unwrap();
        c.lock().unwrap().append(0, b"abc", None);
        c.lock().unwrap().approve();
        c.lock().unwrap().finish();
        let lease = s.lookup("abcdefghijk").unwrap();
        let mut other = video();
        other.id = "lmnopqrstuv".into();
        let c = s.begin(other.clone(), Some(3)).unwrap();
        c.lock().unwrap().append(0, b"def", None);
        c.lock().unwrap().approve();
        c.lock().unwrap().finish();
        assert!(s.contains("abcdefghijk"));
        assert!(!s.contains("lmnopqrstuv"));
        drop(lease);
        let c = s.begin(other, Some(3)).unwrap();
        c.lock().unwrap().append(0, b"def", None);
        c.lock().unwrap().approve();
        c.lock().unwrap().finish();
        assert!(!s.contains("abcdefghijk"));
        assert!(s.contains("lmnopqrstuv"));
        s.configure(false, 128).unwrap();
        assert_eq!(s.count(), 0);
        assert!(s.begin(video(), Some(3)).is_none());
        fs::remove_dir_all(&s.root).unwrap();
    }
    #[test]
    fn complete_transfer_waits_for_native_audio_validation() {
        let s = fixture("validation");
        let c = s.begin(video(), Some(3)).unwrap();
        c.lock().unwrap().append(0, b"abc", None);
        c.lock().unwrap().finish();
        assert!(!s.contains("abcdefghijk"));
        c.lock().unwrap().approve();
        assert!(s.contains("abcdefghijk"));
        fs::remove_dir_all(&s.root).unwrap();
    }
    #[test]
    fn seeks_interruptions_and_clear_never_publish_partial_music() {
        let s = fixture("partial");
        let c = s.begin(video(), Some(6)).unwrap();
        c.lock().unwrap().append(3, b"def", None);
        c.lock().unwrap().approve();
        c.lock().unwrap().finish();
        assert!(s.list().is_empty());
        drop(c);
        let c = s.begin(video(), Some(6)).unwrap();
        c.lock().unwrap().append(0, b"abc", None);
        s.clear().unwrap();
        c.lock().unwrap().append(3, b"def", None);
        c.lock().unwrap().approve();
        c.lock().unwrap().finish();
        assert!(s.list().is_empty());
        assert!(
            fs::read_dir(&s.root).unwrap().all(|p| !p
                .unwrap()
                .path()
                .to_string_lossy()
                .ends_with(".part"))
        );
        fs::remove_dir_all(&s.root).unwrap();
    }
    #[test]
    fn active_playback_survives_clear_and_truncated_files_are_rejected() {
        let s = fixture("lease");
        let c = s.begin(video(), Some(3)).unwrap();
        c.lock().unwrap().append(0, b"abc", None);
        c.lock().unwrap().approve();
        c.lock().unwrap().finish();
        let lease = s.lookup("abcdefghijk").unwrap();
        s.clear().unwrap();
        assert!(lease.path.exists());
        assert!(s.lookup("abcdefghijk").is_none());
        let p = lease.path.clone();
        drop(lease);
        assert!(!p.exists());
        drop(c);
        let c = s.begin(video(), Some(3)).unwrap();
        c.lock().unwrap().append(0, b"abc", None);
        c.lock().unwrap().approve();
        c.lock().unwrap().finish();
        fs::write(s.media("abcdefghijk"), b"a").unwrap();
        assert!(s.lookup("abcdefghijk").is_none());
        fs::remove_dir_all(&s.root).unwrap();
    }
}
