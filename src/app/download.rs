//! Explicit downloads are separate from the automatic completed-music cache.
use super::*;
use std::io::BufRead;
use std::sync::atomic::AtomicBool;
#[derive(Clone)]
struct Choice {
    label: String,
    selector: String,
    audio: bool,
}
struct Prepared {
    video: Video,
    choices: Vec<Choice>,
}
struct Shared {
    epoch: Arc<AtomicU64>,
    cancel: AtomicBool,
    busy: AtomicBool,
    prepared: Mutex<Option<Prepared>>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}
pub(super) struct Guard(Arc<Shared>);
impl Guard {
    pub(super) fn stop(&mut self) {
        self.0.cancel.store(true, Ordering::Release);
        self.0.epoch.fetch_add(1, Ordering::Relaxed);
        let worker = self.0.worker.lock().unwrap().take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
    }
}
pub(super) fn default_folder() -> PathBuf {
    let user = directories::UserDirs::new();
    user.as_ref()
        .and_then(|u| u.download_dir())
        .map(Path::to_path_buf)
        .or_else(|| user.map(|u| u.home_dir().join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("Downloads"))
        .join("Mochune")
}
fn folder(input: &str) -> Result<PathBuf> {
    let input = input.trim();
    let path = if input.is_empty() {
        default_folder()
    } else if input.starts_with("~/") || input.starts_with("~\\") {
        directories::UserDirs::new()
            .context("Cannot find your home folder")?
            .home_dir()
            .join(&input[2..])
    } else {
        PathBuf::from(input)
    };
    anyhow::ensure!(
        path.is_absolute(),
        "Choose an absolute download-folder path."
    );
    anyhow::ensure!(
        !path
            .as_os_str()
            .to_string_lossy()
            .contains(['\n', '\r', '\0']),
        "Invalid download folder."
    );
    Ok(path)
}
fn choices(options: &[FormatOption], s: &Settings) -> Vec<Choice> {
    options
        .iter()
        .map(|o| {
            let mut settings = s.clone();
            settings.format_override = None;
            settings.music = o.music;
            settings.quality = o.quality;
            Choice {
                label: o.label.clone(),
                audio: o.music,
                selector: if o.music {
                    "bestaudio".into()
                } else {
                    o.selector
                        .clone()
                        .unwrap_or_else(|| format_selector(&settings))
                },
            }
        })
        .collect()
}
fn executable(name: &str) -> Option<PathBuf> {
    let file = helper(name);
    if file.is_file() {
        return Some(file);
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(&file))
        .find(|p| p.is_file())
}
fn hidden(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}
fn terminate(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        hidden(&mut command);
        let _ = command
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}
fn post(
    weak: &slint::Weak<SetupWindow>,
    shared: &Arc<Shared>,
    epoch: u64,
    f: impl FnOnce(SetupWindow) + Send + 'static,
) {
    ui_generation(weak, &shared.epoch, epoch, f);
}
fn publish_choices(
    weak: &slint::Weak<SetupWindow>,
    shared: &Arc<Shared>,
    epoch: u64,
    prepared: Prepared,
    selected: usize,
) {
    let labels: Vec<slint::SharedString> = prepared
        .choices
        .iter()
        .map(|c| c.label.clone().into())
        .collect();
    *shared.prepared.lock().unwrap() = Some(prepared);
    shared.busy.store(false, Ordering::Release);
    post(weak, shared, epoch, move |w| {
        w.set_download_labels(ModelRc::new(VecModel::from(labels)));
        w.set_download_choice(selected as i32);
        w.set_download_preparing(false);
        w.set_download_ready(true);
        w.set_download_status("Choose a quality, then Download.".into());
    });
}
pub(super) fn install(w: &SetupWindow, state: Arc<Mutex<State>>, paths: Paths) -> Guard {
    let shared = Arc::new(Shared {
        epoch: Arc::new(AtomicU64::new(0)),
        cancel: AtomicBool::new(false),
        busy: AtomicBool::new(false),
        prepared: Mutex::new(None),
        worker: Mutex::new(None),
    });
    let weak = w.as_weak();
    let st = state.clone();
    let p = paths.clone();
    let control = shared.clone();
    w.on_open_download(move || {
        let Some(w) = weak.upgrade() else {
            return;
        };
        w.set_download_open(true);
        if control.busy.load(Ordering::Acquire) {
            return;
        }
        if w.global::<crate::Sources>().get_mode()!=0 {
            w.set_download_ready(false);w.set_download_preparing(false);w.set_download_title("Source downloads unavailable".into());w.set_download_labels(ModelRc::default());
            w.set_download_status("Downloads for Anime, Movies and Manga are not supported yet. YouTube downloads remain available.".into());return;
        }
        let (video, s, available) = {
            let st = st.lock().unwrap();
            (
                st.selected.clone(),
                st.settings.clone(),
                (st.formats_for == st.selected.as_ref().map(|v| v.id.clone()))
                    .then(|| st.format_options.clone()),
            )
        };
        let Some(video) = video else {
            w.set_download_status("Select a YouTube video first.".into());
            return;
        };
        if p.network.offline() {
            w.set_download_status("Downloads need an Internet connection.".into());
            return;
        }
        if crate::playlists::youtube_id_from_uri(&video.url()).is_none() {
            w.set_download_status("Downloads require a YouTube video.".into());
            return;
        }
        control.cancel.store(false, Ordering::Release);
        control.busy.store(true, Ordering::Release);
        let epoch = control.epoch.fetch_add(1, Ordering::Relaxed) + 1;
        w.set_download_title(video.title.clone().into());
        w.set_download_preparing(true);
        w.set_download_ready(false);
        w.set_download_progress(0.);
        w.set_download_status("Loading available qualities…".into());
        let selected = w.get_quality_choice().max(0) as usize;
        if let Some(options) = available.filter(|o| !o.is_empty()) {
            let list = choices(&options, &s);
            let selected = selected.min(list.len() - 1);
            publish_choices(
                &weak,
                &control,
                epoch,
                Prepared {
                    video,
                    choices: list,
                },
                selected,
            );
            return;
        }
        let control = control.clone();
        let weak = weak.clone();
        let p = p.clone();
        let worker_control = control.clone();
        let worker = thread::spawn(move || {
            let result = extract(
                &p,
                &s,
                &video.url(),
                false,
                Some((&worker_control.epoch, epoch)),
            );
            match result {
                Ok(info) if worker_control.epoch.load(Ordering::Relaxed) == epoch => {
                    if !info["formats"].as_array().is_some_and(|a| !a.is_empty()) {
                        worker_control.busy.store(false, Ordering::Release);
                        post(&weak, &worker_control, epoch, |w| {
                            w.set_download_preparing(false);
                            w.set_download_status(
                                "No downloadable qualities are available.".into(),
                            );
                        });
                        return;
                    }
                    let (options, selected) = playback_options(&info, &s);
                    let list = choices(&options, &s);
                    publish_choices(
                        &weak,
                        &worker_control,
                        epoch,
                        Prepared {
                            video,
                            choices: list,
                        },
                        selected,
                    );
                }
                Err(error) => {
                    worker_control.busy.store(false, Ordering::Release);
                    let text = error.to_string();
                    post(&weak, &worker_control, epoch, move |w| {
                        w.set_download_preparing(false);
                        w.set_download_status(format!("Could not load qualities: {text}").into());
                    });
                }
                _ => {
                    worker_control.busy.store(false, Ordering::Release);
                }
            }
        });
        *control.worker.lock().unwrap() = Some(worker);
    });
    let weak = w.as_weak();
    let control = shared.clone();
    let p = paths.clone();
    let st = state;
    w.on_start_download(move || {
        let Some(w) = weak.upgrade() else {
            return;
        };
        if control.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let prepared_guard = control.prepared.lock().unwrap();
        let Some(prepared) = prepared_guard.as_ref() else {
            control.busy.store(false, Ordering::Release);
            return;
        };
        let Some(choice) = prepared
            .choices
            .get(w.get_download_choice().max(0) as usize)
            .cloned()
        else {
            control.busy.store(false, Ordering::Release);
            return;
        };
        let video = prepared.video.clone();
        drop(prepared_guard);
        let s = st.lock().unwrap().settings.clone();
        control.cancel.store(false, Ordering::Release);
        let epoch = control.epoch.load(Ordering::Relaxed);
        w.set_download_open(false);
        w.set_download_running(true);
        w.set_download_status("Starting download…".into());
        w.set_download_progress(0.);
        let control = control.clone();
        let weak = weak.clone();
        let p = p.clone();
        let worker_control = control.clone();
        let worker = thread::spawn(move || {
            let result = run(&p, &s, &video, &choice, &worker_control, &weak, epoch);
            worker_control.busy.store(false, Ordering::Release);
            let message = match result {
                Ok(path) => format!("Saved: {}", path.display()),
                Err(error) => format!("Download failed: {error:#}"),
            };
            post(&weak, &worker_control, epoch, move |w| {
                w.set_download_running(false);
                w.set_download_status(message.into());
            });
        });
        *control.worker.lock().unwrap() = Some(worker);
    });
    let weak = w.as_weak();
    let control = shared.clone();
    w.on_cancel_download(move || {
        control.cancel.store(true, Ordering::Release);
        control.epoch.fetch_add(1, Ordering::Relaxed);
        if let Some(w) = weak.upgrade() {
            w.set_download_ready(false);
            w.set_download_preparing(false);
            w.set_download_running(false);
            w.set_download_status(
                "Cancelled. Incomplete files are removed; completed files are kept.".into(),
            );
        }
    });
    let weak = w.as_weak();
    w.on_pick_download_folder(move || {
        let weak = weak.clone();
        thread::spawn(move || {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Choose download folder")
                .pick_folder()
            {
                ui(&weak, move |w| {
                    w.set_download_folder(path.to_string_lossy().into_owned().into())
                });
            }
        });
    });
    Guard(shared)
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(
    paths: &Paths,
    s: &Settings,
    video: &Video,
    choice: &Choice,
    control: &Arc<Shared>,
    weak: &slint::Weak<SetupWindow>,
    epoch: u64,
) -> Result<PathBuf> {
    let dest = folder(&s.download_folder)?;
    anyhow::ensure!(
        wait_for_playback(Some((&control.epoch, epoch))),
        "Cancelled"
    );
    fs::create_dir_all(&dest).context("Cannot create download folder")?;
    let n = NEXT_PROCESS.fetch_add(1, Ordering::Relaxed);
    let scratch = Scratch(dest.join(format!(".mochune-download-{}-{n}", std::process::id())));
    fs::create_dir(&scratch.0)?;
    let ffmpeg = executable("ffmpeg");
    anyhow::ensure!(
        choice.audio || !choice.selector.contains('+') || ffmpeg.is_some(),
        "FFmpeg is required to join high-quality video and audio. The bundled helper is missing."
    );
    let mut cmd = Command::new(helper("yt-dlp"));
    hidden(&mut cmd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.env("DENO_DIR", scratch.0.join("deno"));
    cmd.env("TMPDIR", &scratch.0)
        .env("TMP", &scratch.0)
        .env("TEMP", &scratch.0);
    cmd.args(["--ignore-config","--no-warnings","--no-colors","--encoding","utf-8","--no-playlist","--no-cache-dir","--no-overwrites","--no-mtime","--restrict-filenames","--trim-filenames","150","--retries","3","--fragment-retries","3","--socket-timeout","15","--concurrent-fragments","1","--buffer-size","64K","--newline","--progress","--progress-delta","0.5","--no-simulate","--merge-output-format","mp4/mkv","--progress-template","download:progress:%(progress._percent_str)s | %(progress._speed_str)s | ETA %(progress._eta_str)s","--progress-template","postprocess:progress:Finishing…","--print","after_move:filepath:%(filepath)s","-f",&choice.selector]);
    cmd.arg("--paths")
        .arg(format!("home:{}", dest.display()))
        .arg("--paths")
        .arg(format!("temp:{}", scratch.0.display()));
    cmd.args(["-o", "%(title).120B [%(id)s].%(ext)s"]);
    if let Some(ffmpeg) = ffmpeg {
        cmd.arg("--ffmpeg-location").arg(ffmpeg);
    }
    let deno = helper("deno");
    if deno.is_file() {
        cmd.arg("--js-runtimes")
            .arg(format!("deno:{}", deno.display()));
    }
    let mut child = cmd
        .arg("--")
        .arg(video.url())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Cannot start the download helper")?;
    let (tx, rx) = mpsc::sync_channel::<String>(4);
    let stdout = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut input = std::io::BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match input.by_ref().take(8192).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let stderr = child.stderr.take().unwrap();
    let error = Arc::new(Mutex::new(Vec::<u8>::new()));
    let errors = error.clone();
    let err_reader = thread::spawn(move || {
        let mut input = stderr;
        let mut chunk = [0u8; 1024];
        while let Ok(n) = input.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let mut tail = errors.lock().unwrap();
            tail.extend_from_slice(&chunk[..n]);
            let excess = tail.len().saturating_sub(8192);
            tail.drain(..excess);
        }
    });
    let mut file = None;
    let status = loop {
        for line in rx.try_iter() {
            if let Some(path) = line.trim().strip_prefix("filepath:") {
                file = Some(PathBuf::from(path));
            }
            if let Some(text) = line.trim().strip_prefix("progress:") {
                let progress = text
                    .split('%')
                    .next()
                    .and_then(|p| p.trim().parse::<f32>().ok())
                    .unwrap_or(0.)
                    .clamp(0., 100.);
                let text: String = text.chars().take(240).collect();
                post(weak, control, epoch, move |w| {
                    w.set_download_progress(progress);
                    w.set_download_status(format!("Downloading · {text}").into());
                });
            }
        }
        if control.cancel.load(Ordering::Acquire) || control.epoch.load(Ordering::Relaxed) != epoch
        {
            terminate(&mut child);
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                terminate(&mut child);
                return Err(error.into());
            }
        }
        thread::sleep(Duration::from_millis(120));
    };
    // Drain the small final filepath message before joining the bounded reader.
    while !reader.is_finished() {
        for line in rx.try_iter() {
            if let Some(path) = line.trim().strip_prefix("filepath:") {
                file = Some(PathBuf::from(path));
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    for line in rx.try_iter() {
        if let Some(path) = line.trim().strip_prefix("filepath:") {
            file = Some(PathBuf::from(path));
        }
    }
    let _ = reader.join();
    let _ = err_reader.join();
    let status = status.context("Cancelled")?;
    if !status.success() {
        let error = error.lock().unwrap();
        let error = String::from_utf8_lossy(&error);
        let message = error
            .lines()
            .rev()
            .find(|s| s.contains("ERROR:"))
            .unwrap_or("Download helper failed");
        bail!("{}", message.chars().take(400).collect::<String>());
    }
    let file = file.context("Download ended without a completed output file")?;
    anyhow::ensure!(file.is_file(), "The completed download file is missing");
    let _ = paths;
    Ok(file)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_and_audio_are_explicit() {
        assert!(default_folder().ends_with("Mochune"));
        assert!(folder("relative/path").is_err());
        assert!(folder("~/Downloads/Mochune").unwrap().is_absolute());
        let s = Settings::default();
        let choices = choices(&default_format_options(), &s);
        assert_eq!(choices.last().unwrap().selector, "bestaudio");
        assert!(choices.last().unwrap().audio);
    }
}
