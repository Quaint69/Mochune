//! Mode-owned, bounded history. Legacy data has no reliable playback mode.
use super::*;

pub(super) const FILE: &str = "history-v2.json";
const LEGACY: &str = "history.json";
const BACKUP: &str = "history-legacy.json.bak";
pub(super) const LIMIT: usize = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u32,
    video: Vec<Video>,
    music: Vec<Video>,
}

fn bounded(rows: &mut Vec<Video>) {
    let mut seen = std::collections::HashSet::new();
    rows.retain(|v| seen.len() < LIMIT && seen.insert(v.id.clone()));
}

pub(super) fn load(paths: &Paths) -> Result<[Vec<Video>; 2]> {
    load_at(&paths.data)
}

fn load_at(root: &Path) -> Result<[Vec<Video>; 2]> {
    let path = root.join(FILE);
    let mut histories = if path.exists() {
        let saved: Saved = serde_json::from_slice(&fs::read(&path)?)
            .context("Cannot read separated history; original files have been preserved")?;
        anyhow::ensure!(saved.version == 2, "Unsupported history version");
        [saved.video, saved.music]
    } else if root.join(LEGACY).exists() {
        let bytes = fs::read(root.join(LEGACY))?;
        let mut video: Vec<Video> = serde_json::from_slice(&bytes)
            .context("Cannot migrate legacy history; original file has been preserved")?;
        let backup = root.join(BACKUP);
        if backup.exists() {
            anyhow::ensure!(
                fs::read(&backup)? == bytes,
                "Legacy history backup differs; refusing to overwrite it"
            );
        } else {
            crate::playlists::write_atomic(&backup, &bytes, false)?;
        }
        bounded(&mut video);
        let histories = [video, Vec::new()];
        save_at(root, &histories)?;
        // Keep history.json untouched as well. Once FILE exists it is never
        // imported again, even if either mode's history becomes empty.
        histories
    } else {
        Default::default()
    };
    for rows in &mut histories {
        bounded(rows);
    }
    Ok(histories)
}

pub(super) fn save(paths: &Paths, histories: &[Vec<Video>; 2]) -> Result<()> {
    save_at(&paths.data, histories)
}

fn save_at(root: &Path, histories: &[Vec<Video>; 2]) -> Result<()> {
    #[derive(Serialize)]
    struct Snapshot<'a> {
        version: u32,
        video: &'a [Video],
        music: &'a [Video],
    }
    let saved = Snapshot {
        version: 2,
        video: &histories[0],
        music: &histories[1],
    };
    crate::playlists::write_atomic(&root.join(FILE), &serde_json::to_vec_pretty(&saved)?, false)
}

pub(super) fn record(histories: &mut [Vec<Video>; 2], music: bool, video: Video) {
    let rows = &mut histories[usize::from(music)];
    rows.retain(|v| v.id != video.id);
    rows.insert(0, video);
    rows.truncate(LIMIT);
}

pub(super) fn clear(paths: &Paths) -> Result<()> {
    clear_at(&paths.data)
}

fn clear_at(root: &Path) -> Result<()> {
    // Write empty versioned data first so a partial deletion never reimports
    // legacy entries on the next launch. Explicit opt-out erases backups too.
    save_at(root, &Default::default())?;
    for name in [LEGACY, BACKUP] {
        match fs::remove_file(root.join(name)) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mochune-history-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn video(id: &str) -> Video {
        Video {
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn migration_preserves_exact_backup_and_never_reimports() {
        let dir = Temp::new();
        let bytes = serde_json::to_vec(&vec![video("old")]).unwrap();
        fs::write(dir.0.join(LEGACY), &bytes).unwrap();
        let rows = load_at(&dir.0).unwrap();
        assert_eq!(rows[0][0].id, "old");
        assert!(rows[1].is_empty());
        assert_eq!(fs::read(dir.0.join(BACKUP)).unwrap(), bytes);
        assert_eq!(fs::read(dir.0.join(LEGACY)).unwrap(), bytes);
        save_at(&dir.0, &Default::default()).unwrap();
        assert!(load_at(&dir.0).unwrap().iter().all(Vec::is_empty));
    }
    #[test]
    fn mode_limits_and_duplicate_ids_are_independent() {
        let mut rows = Default::default();
        record(&mut rows, false, video("shared"));
        record(&mut rows, true, video("shared"));
        for n in 0..105 {
            record(&mut rows, true, video(&format!("m{n}")));
        }
        assert_eq!(rows[0][0].id, "shared");
        assert_eq!(rows[1].len(), LIMIT);
        assert_eq!(rows[1][0].id, "m104");
        record(&mut rows, true, video("m104"));
        assert_eq!(rows[1].len(), LIMIT);
    }
    #[test]
    fn malformed_or_conflicting_legacy_is_not_overwritten() {
        let dir = Temp::new();
        fs::write(dir.0.join(LEGACY), b"broken").unwrap();
        assert!(load_at(&dir.0).is_err());
        assert!(!dir.0.join(FILE).exists());
        fs::write(dir.0.join(LEGACY), b"[]").unwrap();
        fs::write(dir.0.join(BACKUP), b"different").unwrap();
        assert!(load_at(&dir.0).is_err());
        assert_eq!(fs::read(dir.0.join(BACKUP)).unwrap(), b"different");
    }
    #[test]
    fn roundtrip_and_removal_keep_other_mode() {
        let dir = Temp::new();
        let mut rows = [vec![video("same")], vec![video("same")]];
        rows[0].clear();
        save_at(&dir.0, &rows).unwrap();
        let rows = load_at(&dir.0).unwrap();
        assert!(rows[0].is_empty());
        assert_eq!(rows[1][0].id, "same");
    }

    #[test]
    fn explicit_clear_erases_both_modes_and_legacy_backup() {
        let dir = Temp::new();
        fs::write(dir.0.join(LEGACY), b"[]").unwrap();
        fs::write(dir.0.join(BACKUP), b"[]").unwrap();
        save_at(&dir.0, &[vec![video("video")], vec![video("music")]]).unwrap();
        clear_at(&dir.0).unwrap();
        assert!(!dir.0.join(LEGACY).exists());
        assert!(!dir.0.join(BACKUP).exists());
        assert!(load_at(&dir.0).unwrap().iter().all(Vec::is_empty));
    }

    #[test]
    fn invalid_versioned_file_never_falls_back_to_legacy() {
        let dir = Temp::new();
        fs::write(dir.0.join(LEGACY), b"[]").unwrap();
        for bytes in [
            b"broken".as_slice(),
            b"{\"version\":3,\"video\":[],\"music\":[]}",
        ] {
            fs::write(dir.0.join(FILE), bytes).unwrap();
            assert!(load_at(&dir.0).is_err());
            assert_eq!(fs::read(dir.0.join(FILE)).unwrap(), bytes);
            assert!(!dir.0.join(BACKUP).exists());
        }
    }

    #[test]
    fn loaded_histories_are_bounded_and_deduplicated_independently() {
        let dir = Temp::new();
        let mut rows = vec![video("duplicate"), video("duplicate")];
        rows.extend((0..120).map(|n| video(&format!("row{n}"))));
        save_at(&dir.0, &[rows.clone(), rows]).unwrap();
        let loaded = load_at(&dir.0).unwrap();
        for rows in loaded {
            assert_eq!(rows.len(), LIMIT);
            assert_eq!(rows[0].id, "duplicate");
            assert_eq!(rows[1].id, "row0");
            assert_eq!(rows[99].id, "row98");
        }
    }
}
