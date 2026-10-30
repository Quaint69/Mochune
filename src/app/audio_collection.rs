//! Optional, disk-backed IRS catalog. No network activity or impulse decoding
//! until requested; only metadata is held while the settings picker is open.
use super::*;
use std::{io, path::Component, sync::atomic::AtomicBool};
const URL: &str = "https://codeload.github.com/acosonic/convolver_signals/tar.gz/refs/heads/main";
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;
const MAX_UNPACKED: u64 = 128 * 1024 * 1024;
const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_FILES: usize = 2048;
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    name: String,
    filename: String,
}
fn valid_filename(s: &str) -> bool {
    s.starts_with("ir-") && !s.contains(['/', '\\', ':']) && Path::new(s).components().count() == 1
}
fn catalog(root: &Path) -> Result<Vec<Entry>> {
    let file = fs::File::open(root.join("index.json"))?;
    anyhow::ensure!(
        file.metadata()?.len() <= 2 * 1024 * 1024,
        "Invalid collection index"
    );
    let entries: Vec<Entry> =
        serde_json::from_reader(io::BufReader::new(file.take(2 * 1024 * 1024)))?;
    anyhow::ensure!(entries.len() <= MAX_FILES, "Collection index is too large");
    // Existence and WAV validation happen on selection. Avoid hundreds of
    // filesystem probes every time the settings page opens on a slow drive.
    Ok(entries
        .into_iter()
        .filter(|e| valid_filename(&e.filename) && e.name.len() <= 4096)
        .collect())
}
fn stored_name(source: &str) -> String {
    // Stable across archive ordering and platforms, without trusting path names.
    let mut hash = 0xcbf29ce484222325u64;
    for b in source.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let mut name = String::new();
    for c in Path::new(source)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
    {
        let c = if c.is_control() || "<>:\"/\\|?*".contains(c) {
            '_'
        } else {
            c
        };
        if name.len() + c.len_utf8() > 150 {
            break;
        }
        name.push(c);
    }
    format!("ir-{hash:016x}-{}.irs", name.trim_end_matches([' ', '.']))
}
fn extract(
    archive_file: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(String),
) -> Result<Vec<Entry>> {
    fs::create_dir_all(destination)?;
    let mut archive =
        tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(archive_file)?));
    let mut entries = Vec::new();
    let mut total = 0u64;
    let mut count = 0;
    let mut names = std::collections::HashSet::new();
    for entry in archive.entries()? {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
        let mut entry = entry?;
        count += 1;
        anyhow::ensure!(count <= MAX_FILES * 2, "Too many archive entries");
        total = total
            .checked_add(entry.size())
            .context("Archive size overflow")?;
        anyhow::ensure!(total <= MAX_UNPACKED, "Collection exceeds 128 MiB unpacked");
        let path = entry.path()?.into_owned();
        anyhow::ensure!(
            path.components().all(|c| matches!(c, Component::Normal(_))),
            "Unsafe collection archive path"
        );
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if path.file_name().is_some_and(|n| n == "README.md") && entry.size() <= 65536 {
            let mut out = fs::File::create(destination.join("SOURCE-README.md"))?;
            io::copy(&mut entry, &mut out)?;
            continue;
        }
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("irs") || e.eq_ignore_ascii_case("wav"))
        {
            continue;
        }
        if entry.size() > MAX_FILE {
            continue;
        }
        let source = path.to_string_lossy();
        let filename = stored_name(&source);
        anyhow::ensure!(names.insert(filename.clone()), "Duplicate impulse filename");
        let target = destination.join(&filename);
        {
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            io::copy(&mut entry.by_ref().take(MAX_FILE + 1), &mut output)?;
        }
        if crate::audio_effects::validate(target.to_str().context("Invalid collection path")?)
            .is_err()
        {
            fs::remove_file(target)?;
            continue;
        }
        let relative: PathBuf = path.components().skip(1).collect();
        let relative = relative.strip_prefix("signals").unwrap_or(&relative);
        let name = relative.with_extension("").to_string_lossy().into_owned();
        entries.push(Entry { name, filename });
        anyhow::ensure!(entries.len() <= MAX_FILES, "Too many impulses");
        if entries.len() % 25 == 0 {
            progress(format!("Checking impulses… {} ready", entries.len()));
        }
    }
    // Consume gzip's trailer so a damaged download cannot be installed as valid.
    let mut decoder = archive.into_inner();
    anyhow::ensure!(
        io::copy(
            &mut Read::by_ref(&mut decoder).take(1024 * 1024 + 1),
            &mut io::sink()
        )? <= 1024 * 1024,
        "Unexpected archive trailer"
    );
    anyhow::ensure!(
        !entries.is_empty(),
        "The collection has no compatible impulses"
    );
    entries.sort_by_cached_key(|e| e.name.to_lowercase());
    fs::write(
        destination.join("index.json"),
        serde_json::to_vec(&entries)?,
    )?;
    fs::write(
        destination.join("SOURCE.txt"),
        "IRS collection: https://github.com/acosonic/convolver_signals\nFiles are downloaded on demand, not distributed with Mochune.\n",
    )?;
    Ok(entries)
}
struct Work(PathBuf);
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn download(
    root: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(String),
) -> Result<Vec<Entry>> {
    let parent = root.parent().context("Missing collection folder")?;
    fs::create_dir_all(parent)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join("irs-collection-download.lock"))?;
    lock.try_lock()
        .map_err(|_| anyhow::anyhow!("Another collection download is already running"))?;
    let backup = parent.join(".irs-collection-old");
    if !root.exists() && backup.is_dir() {
        fs::rename(&backup, root)?;
    }
    for item in fs::read_dir(parent)? {
        let item = item?;
        if item
            .file_name()
            .to_string_lossy()
            .starts_with(".irs-collection-work-")
        {
            let _ = fs::remove_dir_all(item.path());
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let work = Work(parent.join(format!(
        ".irs-collection-work-{}-{nonce}",
        std::process::id()
    )));
    fs::create_dir(&work.0)?;
    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
    let archive = work.0.join("collection.tar.gz");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .max_idle_connections(0)
        .build()
        .into();
    let mut response = agent
        .get(URL)
        .header("User-Agent", "Mochune")
        .call()
        .context("Could not download the IRS collection")?;
    let length = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    anyhow::ensure!(
        length.is_none_or(|n| n <= MAX_DOWNLOAD),
        "Collection download exceeds 64 MiB"
    );
    let mut reader = response.body_mut().as_reader();
    let mut output = fs::File::create(&archive)?;
    let mut chunk = [0u8; 65536];
    let mut bytes = 0u64;
    let mut updated = Instant::now();
    loop {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        anyhow::ensure!(bytes <= MAX_DOWNLOAD, "Collection download exceeds 64 MiB");
        output.write_all(&chunk[..n])?;
        if updated.elapsed() >= Duration::from_millis(350) {
            progress(format!("Downloading… {:.1} MiB", bytes as f64 / 1048576.));
            updated = Instant::now();
        }
    }
    drop(output);
    progress("Checking downloaded impulses…".into());
    let entries = extract(&archive, &work.0.join("content"), cancel, &mut progress)?;
    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    if root.exists() {
        fs::rename(root, &backup)?;
    }
    if let Err(error) = fs::rename(work.0.join("content"), root) {
        if !root.exists() && backup.exists() {
            let _ = fs::rename(&backup, root);
        }
        return Err(error.into());
    }
    if backup.exists() {
        let _ = fs::remove_dir_all(backup);
    }
    Ok(entries)
}
#[derive(Default)]
struct Picker {
    loaded: bool,
    entries: Vec<Entry>,
    visible: Vec<Entry>,
}
fn refresh(w: &SetupWindow, root: &Path, state: &Arc<Mutex<Picker>>) {
    let mut state = state.lock().unwrap();
    if !state.loaded {
        state.entries = catalog(root).unwrap_or_default();
        state.loaded = true;
    }
    let filter = w.get_audio_collection_filter().to_lowercase();
    state.visible = state
        .entries
        .iter()
        .filter(|e| e.name.to_lowercase().contains(&filter))
        .cloned()
        .collect();
    let selected = PathBuf::from(w.get_audio_impulse_path().as_str());
    let choice = state
        .visible
        .iter()
        .position(|e| root.join(&e.filename) == selected)
        .map_or(-1, |i| i as i32);
    let titles: Vec<slint::SharedString> = state
        .visible
        .iter()
        .map(|e| e.name.clone().into())
        .collect();
    w.set_audio_collection_titles(ModelRc::new(VecModel::from(titles)));
    w.set_audio_collection_choice(choice);
    if !w.get_audio_collection_downloading() {
        w.set_audio_collection_status(if state.entries.is_empty() {
            "Collection not downloaded".into()
        } else {
            format!("{} compatible impulses", state.entries.len()).into()
        });
    }
}
pub(super) fn install(w: &SetupWindow, root: PathBuf) {
    let state = Arc::new(Mutex::new(Picker::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    let weak = w.as_weak();
    let s = state.clone();
    let r = root.clone();
    w.on_refresh_audio_collection(move || {
        if let Some(w) = weak.upgrade() {
            refresh(&w, &r, &s);
        }
    });
    let weak = w.as_weak();
    let s = state.clone();
    w.on_clear_audio_collection(move || {
        *s.lock().unwrap() = Picker::default();
        if let Some(w) = weak.upgrade() {
            w.set_audio_collection_titles(ModelRc::new(VecModel::<slint::SharedString>::default()));
            w.set_audio_collection_choice(-1);
        }
    });
    let weak = w.as_weak();
    let s = state.clone();
    let r = root.clone();
    w.on_audio_source_change(move |source| {
        if let Some(w) = weak.upgrade() {
            w.set_audio_impulse_source(source.clamp(0, 2));
            w.set_audio_impulse_path("".into());
            w.set_audio_impulse_status(
                if source == 0 {
                    "Bundled Oxy · stereo · 48 kHz"
                } else {
                    "Choose an impulse"
                }
                .into(),
            );
            if source == 2 {
                refresh(&w, &r, &s);
            } else {
                *s.lock().unwrap() = Picker::default();
                w.set_audio_collection_titles(ModelRc::new(
                    VecModel::<slint::SharedString>::default(),
                ));
            }
        }
    });
    let weak = w.as_weak();
    let s = state.clone();
    let r = root.clone();
    w.on_select_audio_collection(move |index| {
        if let Some(w) = weak.upgrade() {
            let entry = s
                .lock()
                .unwrap()
                .visible
                .get(index.max(0) as usize)
                .cloned();
            if index < 0 {
                return;
            }
            if let Some(entry) = entry {
                let path = r.join(entry.filename);
                let path = path.to_string_lossy().into_owned();
                w.set_audio_impulse_path(path.clone().into());
                match crate::audio_effects::validate(&path) {
                    Ok(info) => w.set_audio_impulse_status(info.into()),
                    Err(e) => {
                        w.set_audio_convolver_enabled(false);
                        w.set_audio_impulse_status(format!("Convolution off: {e:#}").into());
                    }
                }
            }
        }
    });
    let c = cancel.clone();
    w.on_cancel_audio_collection(move || {
        c.store(true, Ordering::Relaxed);
    });
    let weak = w.as_weak();
    let s = state;
    let r = root;
    w.on_download_audio_collection(move || {
        let Some(w) = weak.upgrade() else {
            return;
        };
        if w.get_audio_collection_downloading() {
            return;
        }
        w.set_audio_collection_downloading(true);
        w.set_audio_collection_status("Starting download…".into());
        cancel.store(false, Ordering::Relaxed);
        let weak = weak.clone();
        let s = s.clone();
        let r = r.clone();
        let cancel = cancel.clone();
        thread::spawn(move || {
            let updates = weak.clone();
            let result = download(&r, &cancel, move |message| {
                ui(&updates, move |w| {
                    w.set_audio_collection_status(message.into())
                })
            });
            ui(&weak, move |w| {
                w.set_audio_collection_downloading(false);
                match result {
                    Ok(entries) => {
                        *s.lock().unwrap() = Picker {
                            loaded: true,
                            entries,
                            visible: Vec::new(),
                        };
                        if w.get_settings_open() && w.get_audio_impulse_source() == 2 {
                            refresh(&w, &r, &s);
                        } else {
                            *s.lock().unwrap() = Picker::default();
                            w.set_audio_collection_status("Collection downloaded".into());
                        }
                    }
                    Err(e) => w.set_audio_collection_status(format!("Collection: {e:#}").into()),
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> Work {
        let p = std::env::temp_dir().join(format!(
            "mochune-collection-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        Work(p)
    }
    fn wave() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = hound::WavWriter::new(
                io::Cursor::new(&mut bytes),
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 48000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            for v in [32767i16, 0, 0] {
                writer.write_sample(v).unwrap();
            }
            writer.finalize().unwrap();
        }
        bytes
    }
    fn archive(path: &Path, files: &[(&str, Vec<u8>)], unsafe_path: bool) {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            fs::File::create(path).unwrap(),
            flate2::Compression::fast(),
        ));
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_mode(0o644);
            header.set_size(bytes.len() as u64);
            if unsafe_path {
                let b = header.as_mut_bytes();
                b[..100].fill(0);
                b[..13].copy_from_slice(b"../escape.irs");
            }
            header.set_cksum();
            builder.append(&header, io::Cursor::new(bytes)).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }
    #[test]
    fn extraction_keeps_only_valid_files_with_portable_stable_names() {
        let temp = temp();
        let file = temp.0.join("archive.gz");
        let out = temp.0.join("content");
        archive(
            &file,
            &[
                ("repo/signals/valid:name?.irs", wave()),
                ("repo/signals/broken.irs", b"broken".to_vec()),
            ],
            false,
        );
        let entries = extract(&file, &out, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "valid:name?");
        assert!(valid_filename(&entries[0].filename));
        assert!(!entries[0].filename.contains([':', '?']));
        assert_eq!(catalog(&out).unwrap().len(), 1);
        assert_eq!(
            stored_name("repo/signals/a.irs"),
            stored_name("repo/signals/a.irs")
        );
        assert_ne!(
            stored_name("repo/signals/a.irs"),
            stored_name("repo/other/a.irs")
        );
    }
    #[test]
    fn unsafe_paths_cancellation_and_untrusted_index_cannot_escape() {
        let temp = temp();
        let file = temp.0.join("archive.gz");
        archive(&file, &[("placeholder.irs", wave())], true);
        assert!(
            extract(
                &file,
                &temp.0.join("bad"),
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .is_err()
        );
        assert!(!temp.0.join("escape.irs").exists());
        archive(&file, &[("repo/signals/valid.irs", wave())], false);
        assert!(
            extract(
                &file,
                &temp.0.join("cancelled"),
                &AtomicBool::new(true),
                &mut |_| {}
            )
            .is_err()
        );
        fs::write(
            temp.0.join("index.json"),
            r#"[{"name":"bad","filename":"../escape.irs"}]"#,
        )
        .unwrap();
        assert!(catalog(&temp.0).unwrap().is_empty());
    }
    #[test]
    #[ignore = "explicit network download verification"]
    fn real_collection_download() {
        let temp = temp();
        let entries = download(
            &temp.0.join("irs-collection"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        eprintln!(
            "Downloaded and validated {} compatible impulses",
            entries.len()
        );
        assert!(entries.len() > 500);
        assert_eq!(
            entries.len(),
            catalog(&temp.0.join("irs-collection")).unwrap().len()
        );
    }
}
