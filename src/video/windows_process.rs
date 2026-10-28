//! Disposable native-video worker. Same executable/identity as Mochune.
//! IPC carries small commands, status and input; no media or controls bitmaps.
use super::{
    process_protocol::{self as wire, Event, Request, Status},
    *,
};
use std::os::windows::{io::AsRawHandle, process::CommandExt};
use std::{
    collections::VecDeque,
    io::{BufReader, BufWriter, Read},
    process::{Child, Command, Stdio},
    sync::{Condvar, atomic::AtomicU64},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    },
};

const CREATE_NO_WINDOW: u32 = 0x08000000;
#[derive(Default)]
struct Outbox {
    commands: VecDeque<Request>,
}
pub(super) struct Transport {
    job: Arc<Job>,
    terminated: AtomicBool,
    output: Mutex<Outbox>,
    wake: Condvar,
    closed: AtomicBool,
    ready: AtomicBool,
    epoch: AtomicU64,
    revision: AtomicU64,
    pub status: Mutex<Status>,
    events: Mutex<VecDeque<Event>>,
    inputs: Mutex<VecDeque<wire::Input>>,
}
impl Transport {
    fn new(job: Arc<Job>) -> Self {
        Self {
            job,
            terminated: AtomicBool::new(false),
            output: Mutex::new(Outbox::default()),
            wake: Condvar::new(),
            closed: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            revision: AtomicU64::new(0),
            status: Mutex::new(Status::default()),
            events: Mutex::new(VecDeque::new()),
            inputs: Mutex::new(VecDeque::new()),
        }
    }
    fn send(&self, r: Request) -> Result<()> {
        anyhow::ensure!(
            !self.closed.load(Ordering::Acquire),
            "Video process disconnected"
        );
        let mut out = self.output.lock().unwrap();
        // High-frequency seek/volume commands are replaceable; preserve all load/stop edges.
        if matches!(
            r,
            Request::Volume(_) | Request::Seek { .. } | Request::Pause(_)
        ) {
            if let Some(last) = out.commands.back_mut() {
                if std::mem::discriminant(last) == std::mem::discriminant(&r) {
                    *last = r;
                    self.wake.notify_one();
                    return Ok(());
                }
            }
        }
        anyhow::ensure!(
            out.commands.len() < 32,
            "Video command queue is full; retry playback"
        );
        out.commands.push_back(r);
        self.wake.notify_one();
        Ok(())
    }
    pub fn inputs(&self) -> Vec<wire::Input> {
        self.inputs.lock().unwrap().drain(..).collect()
    }
    pub fn terminate(&self) {
        self.close();
        // A mode switch must not wait for the playback worker's URL resolver.
        // Kernel job handles are thread-safe; Arc keeps the handle alive here.
        if !self.terminated.swap(true, Ordering::AcqRel) {
            unsafe { TerminateJobObject(self.job.0, 0) };
        }
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.wake.notify_all();
    }
    fn event(&self, e: Event) {
        match e {
            Event::Ready => self.ready.store(true, Ordering::Release),
            Event::Status(s) => {
                if s.epoch == self.epoch.load(Ordering::Acquire)
                    && s.revision >= self.revision.load(Ordering::Acquire)
                {
                    let mut old = self.status.lock().unwrap();
                    if s.recovery_event != old.recovery_event
                        && let Some(event) = &s.recovery_event
                    {
                        crate::diagnostics::record(
                            event["event"].as_str().unwrap_or("video_recovery"),
                            serde_json::json!({"epoch":s.epoch,"worker":event["data"]}),
                        );
                    }
                    *old = s;
                }
            }
            Event::Input(i) => {
                let mut q = self.inputs.lock().unwrap();
                if matches!(i, wire::Input::Move(..))
                    && matches!(q.back(), Some(wire::Input::Move(..)))
                {
                    q.pop_back();
                }
                if q.len() >= 64 {
                    q.pop_front();
                }
                q.push_back(i);
            }
            other => {
                let mut q = self.events.lock().unwrap();
                if q.len() >= 32 {
                    q.pop_front();
                }
                q.push_back(other);
            }
        }
    }
}
struct Job(HANDLE);
unsafe impl Send for Job {}
unsafe impl Sync for Job {}
impl Job {
    fn attach(child: &Child) -> Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        anyhow::ensure!(
            !handle.is_null(),
            "Create video job: {}",
            std::io::Error::last_os_error()
        );
        let job = Self(handle);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        anyhow::ensure!(
            ok != 0,
            "Configure video job: {}",
            std::io::Error::last_os_error()
        );
        anyhow::ensure!(
            unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } != 0,
            "Attach video job: {}",
            std::io::Error::last_os_error()
        );
        Ok(job)
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
struct Session {
    child: Child,
    _job: Arc<Job>,
    transport: Arc<Transport>,
    threads: Vec<JoinHandle<()>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.transport.close();
        let natural = self.child.try_wait().ok().flatten();
        let exited_before_cleanup = natural.is_some();
        if !exited_before_cleanup {
            let _ = self.child.kill();
        }
        let status = natural.or_else(|| self.child.wait().ok());
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        crate::diagnostics::record(
            "video_process_exited",
            serde_json::json!({"pid":self.child.id(),"gpu_cache_reclaimed_by_process_exit":true,
                "exited_before_cleanup":exited_before_cleanup,"exit_code":status.as_ref().and_then(|s|s.code()),
                "exit_hex":status.as_ref().and_then(|s|s.code()).map(|c|format!("0x{:08X}",c as u32))}),
        );
    }
}
pub(super) struct Client {
    controller: windows_native::Controller,
    session: Option<Session>,
    epoch: u64,
    active: bool,
    revision: u64,
    was_seeking: bool,
}
impl Client {
    pub fn new(controller: windows_native::Controller) -> Self {
        Self {
            controller,
            session: None,
            epoch: 0,
            active: false,
            revision: 0,
            was_seeking: false,
        }
    }
    fn start(&mut self) -> Result<()> {
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.transport.closed.load(Ordering::Acquire))
        {
            self.shutdown();
        }
        if self.session.is_some() {
            return Ok(());
        }
        let hwnd = self.controller.prepare_process_host()?;
        let mut child = Command::new(std::env::current_exe()?)
            .args(["--video-worker", "--parent-window", &hwnd.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("Start Mochune video process")?;
        let job = match Job::attach(&child) {
            Ok(j) => Arc::new(j),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                self.controller.stop();
                return Err(e);
            }
        };
        let input = child.stdin.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let output = child.stdout.take().unwrap();
        let transport = Arc::new(Transport::new(job.clone()));
        self.controller
            .set_process_transport(Arc::downgrade(&transport));
        let tx = transport.clone();
        let writer = thread::spawn(move || {
            let mut pipe = BufWriter::new(input);
            loop {
                let command = {
                    let mut out = tx.output.lock().unwrap();
                    while out.commands.is_empty() && !tx.closed.load(Ordering::Acquire) {
                        out = tx.wake.wait(out).unwrap();
                    }
                    if tx.closed.load(Ordering::Acquire) {
                        break;
                    }
                    out.commands.pop_front().unwrap()
                };
                let result = wire::write_packet(&mut pipe, &command, &[]);
                if result.is_err() {
                    tx.close();
                    break;
                }
            }
        });
        let tx = transport.clone();
        let reader = thread::spawn(move || {
            let mut pipe = BufReader::new(output);
            let failure = loop {
                match wire::read_packet::<Event>(&mut pipe) {
                    Ok((event, bytes)) if bytes.is_empty() => tx.event(event),
                    Ok(_) => break "Unexpected raw payload in video status".to_owned(),
                    Err(error) => break format!("{error:#}"),
                }
            };
            if !tx.closed.load(Ordering::Acquire) {
                crate::diagnostics::record("video_worker_connection_failed", serde_json::json!({"reason":failure}));
                tx.event(Event::Error {
                    epoch: tx.epoch.load(Ordering::Acquire),
                    message: "Mochune video process disconnected. Retry playback.".into(),
                    debug: failure,
                    element: "video-worker".into(),
                });
            }
            tx.close();
        });
        // Drain native diagnostics continuously without retaining an unbounded
        // line or letting a full stderr pipe stall the decoder. Keep the tail too.
        let error_reader = thread::spawn(move || {
            let mut bytes = [0u8; 2048];
            let mut tail = Vec::new();
            let mut chunks = 0usize;
            while let Ok(n) = stderr.read(&mut bytes) {
                if n == 0 { break; }
                chunks += 1;
                tail.clear();
                tail.extend_from_slice(&bytes[..n]);
                if chunks <= 4 {
                    crate::diagnostics::record("video_worker_stderr", serde_json::json!({"text":String::from_utf8_lossy(&tail)}));
                }
            }
            if chunks > 4 {
                crate::diagnostics::record("video_worker_stderr_tail", serde_json::json!({"text":String::from_utf8_lossy(&tail),"suppressed_chunks":chunks-4}));
            }
        });
        self.session = Some(Session {
            child,
            _job: job,
            transport: transport.clone(),
            threads: vec![writer, reader, error_reader],
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !transport.ready.load(Ordering::Acquire) {
            anyhow::ensure!(
                !transport.closed.load(Ordering::Acquire) && Instant::now() < deadline,
                "Mochune video process did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
        crate::diagnostics::record(
            "video_process_started",
            serde_json::json!({"pid":self.session.as_ref().unwrap().child.id(),"parent_pid":std::process::id(),"executable":"mochune.exe","decoded_video_ipc":false}),
        );
        Ok(())
    }
    pub fn load(
        &mut self,
        uri: &str,
        audio: Option<&str>,
        hardware: bool,
        volume: f64,
        headers: &[(&str, &str)],
        audio_headers: &[(&str, &str)],
        start: (f64, bool),
        buffer_hints: buffering::Hints,
    ) -> Result<()> {
        if let Err(e) = self.start() {
            self.shutdown();
            return Err(e);
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.revision = 0;
        self.was_seeking = false;
        let t = &self.session.as_ref().unwrap().transport;
        t.epoch.store(self.epoch, Ordering::Release);
        t.revision.store(0, Ordering::Release);
        *t.status.lock().unwrap() = Status {
            epoch: self.epoch,
            ..Default::default()
        };
        t.events.lock().unwrap().clear();
        t.send(Request::Load {
            epoch: self.epoch,
            uri: uri.into(),
            audio: audio.map(Into::into),
            hardware,
            volume,
            start_position: start.0,
            paused: start.1,
            buffer_hints,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            audio_headers: audio_headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        })?;
        self.active = true;
        Ok(())
    }
    pub fn active(&self) -> bool {
        self.active
    }
    pub fn status(&self) -> Status {
        self.session
            .as_ref()
            .map(|s| s.transport.status.lock().unwrap().clone())
            .unwrap_or_default()
    }
    pub fn send(&self, r: Request) -> Result<()> {
        self.session
            .as_ref()
            .context("Video process is not running")?
            .transport
            .send(r)
    }
    pub fn stop(&mut self) -> Result<()> {
        self.active = false;
        self.was_seeking = false;
        self.epoch = self.epoch.wrapping_add(1);
        if let Some(s) = &self.session {
            s.transport.epoch.store(self.epoch, Ordering::Release);
            s.transport.events.lock().unwrap().clear();
            *s.transport.status.lock().unwrap() = Status {
                epoch: self.epoch,
                ..Default::default()
            };
            if s.transport.send(Request::Stop).is_err() {
                self.shutdown();
            }
        }
        Ok(())
    }
    pub fn shutdown(&mut self) {
        self.active = false;
        self.was_seeking = false;
        self.session.take();
        self.controller.stop();
    }
    pub fn next_event(&self) -> Option<Event> {
        let session = self.session.as_ref()?;
        let mut queue = session.transport.events.lock().unwrap();
        while let Some(e) = queue.pop_front() {
            if match &e {
                Event::State { epoch, .. }
                | Event::Started { epoch }
                | Event::Buffering { epoch, .. }
                | Event::Eos { epoch }
                | Event::Error { epoch, .. } => *epoch == self.epoch,
                _ => false,
            } {
                return Some(e);
            }
        }
        None
    }
    pub fn seek(&mut self, seconds: f64, paused: bool) -> Result<()> {
        self.revision = self.revision.wrapping_add(1);
        self.was_seeking = true;
        if let Some(s) = &self.session {
            s.transport.revision.store(self.revision, Ordering::Release);
            s.transport.status.lock().unwrap().seeking = true;
        }
        let snapshot = self.status();
        crate::diagnostics::record(
            "video_seek_requested",
            serde_json::json!({
                "epoch":self.epoch,"revision":self.revision,"target":seconds,"paused":paused,"from":snapshot.position,"buffered_ranges":snapshot.buffered_ranges,"buffered_estimated":snapshot.buffered_estimated
            }),
        );
        self.send(Request::Seek {
            seconds,
            paused,
            revision: self.revision,
        })
    }
    pub fn poll_seek(&mut self) -> bool {
        let status = self.status();
        let now = status.seeking;
        let done = self.was_seeking && !now;
        if done {
            crate::diagnostics::record(
                "video_seek_ready",
                serde_json::json!({
                    "epoch":status.epoch,"revision":status.revision,"position":status.position,
                    "video_pts":status.pts,"rendered":status.rendered,"worker":status.seek_details
                }),
            );
        }
        self.was_seeking = now;
        done
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// The helper owns the video HWND and pumps its messages on its main thread.
// The streaming worker owns GStreamer; no Slint backend or desktop app is created.
pub(crate) fn run_worker(parent: isize) -> Result<()> {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
    gst::init()?;
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.ok();
    let parent = HWND(parent as *mut std::ffi::c_void);
    anyhow::ensure!(
        unsafe { IsWindow(Some(parent)) }.as_bool(),
        "Invalid video host window"
    );
    let mut child = windows_native::ProcessWindow::new(parent)?;
    let hwnd = child.hwnd();
    let engine = super::install_native_worker(hwnd)?;
    let controller = engine.native.as_ref().unwrap().clone();
    let (control_tx, control_rx) = mpsc::sync_channel(32);
    let (event_tx, event_rx) = mpsc::sync_channel::<Event>(64);
    let done = Arc::new(AtomicBool::new(false));
    let reader_done = done.clone();
    let rx_tx = control_tx;
    let artwork_controller = controller.clone();
    thread::spawn(move || {
        let mut pipe = BufReader::new(std::io::stdin());
        while let Ok((request, bytes)) = wire::read_packet::<Request>(&mut pipe) {
            if let Request::Artwork {
                version,
                canvas,
                rect,
                present,
            } = request
            {
                if present && wire::valid_artwork(canvas, rect, bytes.len()) {
                    artwork_controller.process_artwork(Some(Arc::new(native_overlay::Artwork {
                        version,
                        canvas,
                        rect,
                        bgra: bytes,
                    })));
                } else if !present && bytes.is_empty() {
                    artwork_controller.process_artwork(None);
                } else {
                    break;
                }
            } else {
                if !bytes.is_empty() {
                    break;
                }
                if matches!(request, Request::Shutdown) {
                    reader_done.store(true, Ordering::Release);
                    break;
                }
                // Only this background pipe reader may wait on a full control queue.
                if rx_tx.send(request).is_err() {
                    break;
                }
            }
        }
        reader_done.store(true, Ordering::Release);
    });
    let writer_done = done.clone();
    thread::spawn(move || {
        let mut pipe = BufWriter::new(std::io::stdout());
        while let Ok(e) = event_rx.recv() {
            if wire::write_packet(&mut pipe, &e, &[]).is_err() {
                writer_done.store(true, Ordering::Release);
                break;
            }
        }
    });
    let worker_done = done.clone();
    let events = event_tx.clone();
    let worker = thread::spawn(move || worker_loop(engine, control_rx, events, worker_done));
    event_tx.send(Event::Ready)?;
    while !done.load(Ordering::Acquire) && unsafe { IsWindow(Some(parent)) }.as_bool() {
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                done.store(true, Ordering::Release);
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        child.fit_parent(parent);
        for i in child.inputs() {
            if event_tx.try_send(Event::Input(i)).is_err() {
                done.store(true, Ordering::Release);
                break;
            }
        }
        thread::sleep(Duration::from_millis(8));
    }
    done.store(true, Ordering::Release);
    let _ = worker.join();
    drop(child);
    Ok(())
}
fn worker_loop(
    mut engine: Engine,
    rx: mpsc::Receiver<Request>,
    tx: mpsc::SyncSender<Event>,
    done: Arc<AtomicBool>,
) {
    let mut load_started = Instant::now();
    let mut epoch = 0;
    let mut revision = 0;
    let mut terminal = true;
    let mut loaded = false;
    let mut intent_paused = false;
    let mut startup: Option<super::playback_state::Start> = None;
    let mut buffering = super::playback_state::Buffering::default();
    let mut last = Instant::now();
    let emit = |event| {
        if tx.try_send(event).is_err() {
            done.store(true, Ordering::Release);
        }
    };
    while !done.load(Ordering::Acquire) {
        if let Ok(request) = rx.recv_timeout(Duration::from_millis(20)) {
            let result: Result<()> = (|| {
                match request {
                    Request::Load {
                        epoch: n,
                        uri,
                        audio,
                        hardware,
                        volume,
                        start_position,
                        paused,
                        headers,
                        audio_headers,
                        buffer_hints,
                    } => {
                        load_started = Instant::now();
                        epoch = n;
                        revision = 0;
                        terminal = false;
                        loaded = true;
                        buffering.clear();
                        let headers = headers
                            .iter()
                            .map(|(k, v)| (k.as_str(), v.as_str()))
                            .collect::<Vec<_>>();
                        let ah = audio_headers
                            .iter()
                            .map(|(k, v)| (k.as_str(), v.as_str()))
                            .collect::<Vec<_>>();
                        engine.load_buffered(
                            &uri,
                            audio.as_deref(),
                            false,
                            hardware,
                            volume,
                            &headers,
                            &ah,
                            None,
                            Some((start_position, paused)),
                            buffer_hints,
                        )?;
                        intent_paused = paused;
                        startup = Some(super::playback_state::Start::new(start_position, paused));
                        crate::diagnostics::record(
                            "video_worker_start_requested",
                            serde_json::json!({"epoch":epoch,"position":start_position,"paused":paused,"pipeline_setup_ms":load_started.elapsed().as_secs_f64()*1000.}),
                        );
                    }
                    Request::Stop => {
                        startup = None;
                        terminal = true;
                        loaded = false;
                        buffering.clear();
                        engine.stop()?;
                    }
                    Request::Seek {
                        seconds,
                        paused,
                        revision: n,
                    } => {
                        if !loaded {
                            return Ok(());
                        }
                        let Some(seconds) = playback_state::seek_target(
                            seconds,
                            engine.duration().map(|d| d.seconds_f64()),
                        ) else {
                            return Ok(());
                        };
                        intent_paused = paused;
                        revision = n;
                        terminal = false;
                        if let Some(start) = startup.as_mut() {
                            start.position = seconds;
                            start.paused = paused;
                            if start.seek_sent {
                                engine.seek_with_state(seconds, paused)?;
                            }
                        } else {
                            engine.seek_with_state(seconds, paused)?;
                        }
                    }
                    Request::Pause(paused) => {
                        if !loaded || terminal {
                            return Ok(());
                        }
                        intent_paused = paused;
                        if let Some(start) = startup.as_mut() {
                            start.paused = paused;
                            if start.seek_sent {
                                engine.set_paused(paused)?;
                            }
                        } else {
                            engine.set_paused(paused)?;
                        }
                    }
                    Request::Volume(v) => engine.set_volume(v),
                    _ => {}
                }
                Ok(())
            })();
            if let Err(e) = result {
                startup = None;
                terminal = true;
                loaded = false;
                emit(Event::Error {
                    epoch,
                    message: format!("{e:#}"),
                    debug: String::new(),
                    element: "video-worker-command".into(),
                });
            }
        }
        while let Ok(message) = engine.messages.try_recv() {
            match message.view() {
                gst::MessageView::Error(e) if !terminal => {
                    terminal = true;
                    loaded = false;
                    startup = None;
                    crate::diagnostics::record(
                        "video_worker_error",
                        serde_json::json!({"error":e.error().to_string(),"debug":e.debug().map(|d|d.to_string()),"element":e.src().map(|s|s.path_string().to_string())}),
                    );
                    emit(Event::Error {
                        epoch,
                        message: e.error().to_string(),
                        debug: e
                            .debug()
                            .map(|s| s.chars().take(8192).collect())
                            .unwrap_or_default(),
                        element: e
                            .src()
                            .map(|s| s.path_string().chars().take(1024).collect())
                            .unwrap_or_default(),
                    });
                }
                gst::MessageView::Buffering(b) if !terminal => {
                    let previous = buffering.waiting();
                    buffering.update(b.src().map_or(0, |s| s.as_ptr() as usize), b.percent());
                    if previous != buffering.waiting() {
                        if let Some(suppressed) = buffering.diagnostic_transition() {
                            crate::diagnostics::record(
                                "worker_buffering_transition",
                                serde_json::json!({
                                    "epoch":epoch,"waiting":buffering.waiting(),"suppressed_transitions":suppressed,
                                    "elapsed_ms":load_started.elapsed().as_secs_f64()*1000.
                                }),
                            );
                        }
                        emit(Event::Buffering {
                            epoch,
                            percent: if buffering.waiting() { 0 } else { 100 },
                        });
                    }
                }
                gst::MessageView::Eos(_) if !terminal => {
                    terminal = true;
                    startup = None;
                    crate::diagnostics::record(
                        "video_worker_eos",
                        serde_json::json!({"epoch":epoch}),
                    );
                    emit(Event::Eos { epoch });
                }
                gst::MessageView::StateChanged(s)
                    if message
                        .src()
                        .is_some_and(|v| v == engine.pipeline.upcast_ref::<gst::Object>())
                        && startup.is_none()
                        && !terminal
                        && matches!(s.current(), gst::State::Playing | gst::State::Paused) =>
                {
                    emit(Event::State {
                        epoch,
                        playing: s.current() == gst::State::Playing,
                    })
                }
                _ => {}
            }
        }
        let progress = (|| -> Result<bool> {
            if terminal {
                return Ok(false);
            }
            engine.poll_seek()?;
            match startup.as_mut() {
                Some(start) => engine.poll_start(start),
                None => Ok(false),
            }
        })();
        if let Ok(true) = progress {
            if let Some(start) = startup.take() {
                crate::diagnostics::record(
                    "video_worker_start_ready",
                    serde_json::json!({"epoch":epoch,"position":start.position,"paused":start.paused,"ready_ms":load_started.elapsed().as_secs_f64()*1000.}),
                );
                emit(Event::Started { epoch });
            }
        }
        if let Err(e) = progress {
            startup = None;
            terminal = true;
            loaded = false;
            emit(Event::Error {
                epoch,
                message: e.to_string(),
                debug: String::new(),
                element: "video-worker-seek".into(),
            });
        }
        if last.elapsed() >= Duration::from_millis(100) {
            last = Instant::now();
            let monitor_started = Instant::now();
            let (rendered, pts, has_frame, dropped) =
                engine.native.as_ref().unwrap().process_sample();
            let sink_sample_ms = monitor_started.elapsed().as_secs_f64() * 1000.;
            let rebuffering = engine
                .poll_rebuffer(!terminal && startup.is_none(), intent_paused)
                .unwrap_or(false);
            if !terminal && startup.is_none() && !intent_paused && !engine.seeking() {
                engine.recovery.observe_drops(rendered, dropped);
            }
            let report = engine.acceleration_report(true, false);
            let buffer_started = Instant::now();
            let (buffered_ranges, buffered_estimated) = engine.buffered_ranges();
            let buffered_confirmed = engine.confirmed_buffered_ranges();
            let buffer_sample_ms = buffer_started.elapsed().as_secs_f64() * 1000.;
            let mut seek_details = engine.seek_diagnostics();
            if let Some(details) = seek_details.as_object_mut() {
                // Piggyback on bounded health snapshots, never log every frame.
                details.insert(
                    "compressed_ranges".into(),
                    serde_json::json!(engine.buffer_state.lock().unwrap().diagnostics),
                );
                details.insert(
                    "display_ranges".into(),
                    serde_json::json!({"ranges":buffered_ranges,"estimated":buffered_estimated}),
                );
                details.insert(
                    "monitor_ms".into(),
                    serde_json::json!({
                        "sink":sink_sample_ms,"buffer":buffer_sample_ms,
                        "total":monitor_started.elapsed().as_secs_f64()*1000.
                    }),
                );
            }
            emit(Event::Status(Status {
                epoch,
                revision,
                position: if startup.is_some() {
                    None
                } else {
                    engine
                        .pipeline
                        .query_position::<gst::ClockTime>()
                        .map(|t| t.seconds_f64())
                },
                duration: engine
                    .pipeline
                    .query_duration::<gst::ClockTime>()
                    .map(|t| t.seconds_f64()),
                paused: intent_paused,
                rebuffering,
                recovery_event: engine.recovery.last_event.clone(),
                seeking: engine.seeking() || startup.is_some(),
                rendered: if startup.is_some() { 0 } else { rendered },
                has_frame: !terminal && startup.is_none() && has_frame,
                pts,
                decoder: engine.decoder.lock().unwrap().clone(),
                memory: engine.memory_mode(),
                verified: report.verified,
                acceleration_status: report.status,
                acceleration_details: report.details,
                process_memory: crate::diagnostics::process_memory(),
                seek_details,
                buffered_ranges,
                buffered_estimated,
                buffered_confirmed,
            }));
        }
    }
    let _ = engine.stop();
}
