//! Opt-in real HTTP + two-output seek check. Supply a moving MP4 with audio.
use super::*;
#[test]
#[ignore = "requires local Range HTTP fixture via MOCHUNE_TEST_SEEK_URI"]
fn retained_http_seek_and_quality_reload() {
    gst::init().unwrap();
    let uri = std::env::var("MOCHUNE_TEST_SEEK_URI").expect("Set a local HTTP fixture URI");
    let pipeline = gst::Pipeline::new();
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", true)
        .build()
        .unwrap();
    let progress = Arc::new(Mutex::new(SeekProgress::default()));
    watch_seek_output(&sink, progress.clone(), 2);
    let (_, messages) = mpsc::channel();
    let mut e = Engine {
        audio_effects: Default::default(),
        interpolation_mode:0,
        interpolation_auto_fallback:true,
        pipeline,
        messages,
        decoder: Arc::new(Mutex::new(String::new())),
        evidence: Arc::new(acceleration::Evidence::default()),
        video_sink: sink,
        transient: Vec::new(),
        buffer_state: Mutex::new(buffered::Snapshot::default()),
        recovery: rebuffer::Recovery::default(),
        source_hls: source_hls::Gate::default(),
        ready: Arc::new(AtomicBool::new(true)),
        setup_error: Arc::new(Mutex::new(None)),
        volume: None,
        frames: Arc::new(Mutex::new(FrameMailbox::default())),
        presentation: Arc::new(AtomicBool::new(false)),
        pending_seek: None,
        seek_progress: progress,
        #[cfg(target_os = "windows")]
        native: None,
        #[cfg(target_os = "windows")]
        remote: None,
        seek_watch: None,
        queued_seek: None,
        cache_capture: None,
    };
    // Compare the source ring's pull output byte-for-byte before any parser or
    // decoder. This distinguishes corrupt retention from decoder seek bugs.
    if let Ok(path) = std::env::var("MOCHUNE_TEST_SOURCE_BYTES") {
        let expected = Arc::new(std::fs::read(path).unwrap());
        e.pipeline.connect_deep_element_added(move |_, _, q| {
            if q.factory().is_none_or(|f| f.name() != "queue2") {
                return;
            }
            let expected = expected.clone();
            q.static_pad("src").unwrap().add_probe(
                gst::PadProbeType::BUFFER | gst::PadProbeType::PULL,
                move |_, info| {
                    if let Some(b) = info.buffer() {
                        let off = if info.offset != u64::MAX {
                            info.offset
                        } else {
                            b.offset()
                        };
                        if off != u64::MAX {
                            let bytes = b.map_readable().unwrap();
                            assert!(expected.get(off as usize..off as usize+bytes.len()) == Some(bytes.as_slice()),
                                "Native compressed ring returned incorrect bytes at {off} ({} bytes)",bytes.len());
                        }
                    }
                    gst::PadProbeReturn::Ok
                },
            );
        });
    }
    // Intentionally underestimate bitrate so this fixture wraps the ring;
    // the normal metadata-aware budget is exercised separately below.
    e.load_buffered(
        &uri,
        None,
        false,
        false,
        0.,
        &[],
        &[],
        None,
        None,
        buffering::Hints {
            duration: 0.,
            primary_bps: 2_048_000.,
            audio_bps: 0.,
        },
    )
    .unwrap();
    let started = std::time::Instant::now();
    let mut next_sample = started;
    let mut sampled = 0;
    let mut slowest_sample = std::time::Duration::ZERO;
    while e.position().is_none_or(|p| p.seconds_f64() < 4.) {
        // Exercise monitoring continuously during playback, not only once after
        // startup. Sampling must not steal the streaming task or stall output.
        if std::time::Instant::now() >= next_sample {
            let at = std::time::Instant::now();
            if let Some(pts) = e.seek_progress.lock().unwrap().video_pts {
                LAST_VIDEO_PTS.store(pts.nseconds(), Ordering::Release);
            }
            e.poll_rebuffer(true, false).unwrap();
            e.buffered_ranges();
            slowest_sample = slowest_sample.max(at.elapsed());
            sampled += 1;
            next_sample = std::time::Instant::now() + std::time::Duration::from_millis(100);
        }
        if let Some(m) = e
            .pipeline
            .bus()
            .unwrap()
            .pop_filtered(&[gst::MessageType::Error])
        {
            panic!("{m:?}");
        }
        assert!(
            started.elapsed().as_secs() < 10,
            "fixture startup/playback failed"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(sampled >= 15, "Playback monitoring did not run: {sampled}");
    println!(
        "{}",
        serde_json::json!({"monitor_samples":sampled,"max_buffer_query_ms":slowest_sample.as_secs_f64()*1000.})
    );
    let (ranges, estimated) = e.buffered_ranges();
    assert!(
        !ranges.is_empty(),
        "Native retained buffer was not reported"
    );
    let position = e.position().unwrap().seconds_f64();
    let duration = e.duration().unwrap().seconds_f64();
    assert!(
        ranges.iter().any(|(_, b)| b * duration >= position - 0.35),
        "Native compressed retention contains no forward media: {ranges:?} at {position}"
    );
    let stats = uri.rsplit_once('/').unwrap().0.to_string() + "/stats";
    let requests = || {
        ureq::get(&stats)
            .call()
            .unwrap()
            .body_mut()
            .read_json::<serde_json::Value>()
            .unwrap()["requests"]
            .as_u64()
            .unwrap()
    };
    let mut elapsed = Vec::new();
    let mut retained_checks = 0;
    for target in [1., 12., 11., 0.5] {
        e.buffer_state.lock().unwrap().prepare_seek();
        e.buffered_ranges();
        let cached = target < e.position().unwrap().seconds_f64()
            && e.buffer_state.lock().unwrap().seek_retained(target) == Some(true);
        let before = requests();
        eprintln!(
            "seek case target={target} cached={cached} ranges={:?}",
            e.buffered_ranges().0
        );
        let at = std::time::Instant::now();
        e.seek_with_state(target, false).unwrap();
        while !e.poll_seek().unwrap() {
            if let Some(m) = e
                .pipeline
                .bus()
                .unwrap()
                .pop_filtered(&[gst::MessageType::Error])
            {
                panic!("{m:?}");
            }
            assert!(at.elapsed().as_secs() < 5, "HTTP seek stalled");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let pts = e
            .video_sink
            .property::<Option<gst::Sample>>("last-sample")
            .unwrap()
            .buffer()
            .unwrap()
            .pts()
            .unwrap()
            .seconds_f64();
        let clock = e.position().unwrap().seconds_f64();
        assert!(
            (pts - target).abs() < 0.75 && (clock - target).abs() < 0.75,
            "A/V target mismatch: {target}, video {pts}, clock {clock}"
        );
        let after = requests();
        if cached {
            retained_checks += 1;
            assert_eq!(
                before, after,
                "Retained backward seek unexpectedly fetched a new HTTP range"
            );
        }
        elapsed.push((target, at.elapsed().as_millis(), after - before));
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        retained_checks > 0,
        "Test did not exercise a retained keyframe"
    );
    // Replay the log failure: playback advances past the target before the
    // worker gets its next poll. Successful current-segment evidence must latch.
    e.seek_with_state(0., false).unwrap();
    let reopen_started = std::time::Instant::now();
    while e
        .pending_seek
        .as_ref()
        .is_some_and(|s| s.repreroll || s.await_pause)
    {
        e.poll_seek().unwrap();
        assert!(
            reopen_started.elapsed().as_secs() < 5,
            "Evicted source did not reopen"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(1400));
    assert!(
        e.poll_seek().unwrap(),
        "Delayed poll forgot a successful target frame"
    );
    // Seek now deliberately freezes audio until valid video preroll. Once the
    // delayed worker resumes, both outputs must advance together, not race.
    let resumed = std::time::Instant::now();
    while e.position().is_none_or(|p| p.seconds_f64() < 0.2) {
        assert!(
            resumed.elapsed().as_secs() < 2,
            "Seek did not resume after preroll"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let clock = e.position().unwrap().seconds_f64();
    let pts = e
        .video_sink
        .property::<Option<gst::Sample>>("last-sample")
        .unwrap()
        .buffer()
        .unwrap()
        .pts()
        .unwrap()
        .seconds_f64();
    assert!(
        clock >= 0.2 && (clock - pts).abs() < 0.2,
        "Delayed seek A/V drift: {clock}, {pts}"
    );
    e.stop().unwrap();
    assert!(
        e.buffered_ranges().0.is_empty(),
        "Stopped quality retained its ranges"
    );
    e.load(
        &uri,
        None,
        false,
        false,
        0.,
        &[],
        &[],
        None,
        Some((2., true)),
    )
    .unwrap();
    assert!(
        e.buffered_ranges().0.is_empty(),
        "Replacement quality inherited old ranges"
    );
    let mut start = playback_state::Start::new(2., true);
    let at = std::time::Instant::now();
    loop {
        e.poll_seek().unwrap();
        if e.poll_start(&mut start).unwrap() {
            break;
        }
        assert!(at.elapsed().as_secs() < 5, "paused reload stalled");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(e.pipeline.current_state(), gst::State::Paused);
    // Exact EOF and beyond EOF must resolve to a real last frame, not wait for
    // a sample that cannot exist. Preserve pause intent, then permit rewinding.
    for target in [duration, duration + 5., duration - 5.] {
        e.seek_with_state(target, true).unwrap();
        let started = std::time::Instant::now();
        while !e.poll_seek().unwrap() {
            assert!(started.elapsed().as_secs() < 5, "EOF-bound seek stalled");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(e.pipeline.current_state(), gst::State::Paused);
        assert!(e.position().unwrap().seconds_f64() < duration);
    }
    e.stop().unwrap();
    e.seek_with_state(5., false).unwrap();
    assert!(!e.seeking());
    assert_eq!(e.pipeline.current_state(), gst::State::Null);
    // A complete compressed source must remain exactly full over repeated
    // samples, including split header/tail ranges, with no VBR endpoint drift.
    e.load_buffered(
        &uri,
        None,
        false,
        false,
        0.,
        &[],
        &[],
        None,
        None,
        buffering::Hints {
            duration,
            primary_bps: 9_000_000.,
            audio_bps: 0.,
        },
    )
    .unwrap();
    let started = std::time::Instant::now();
    loop {
        if let Some(pts) = e.seek_progress.lock().unwrap().video_pts {
            LAST_VIDEO_PTS.store(pts.nseconds(), Ordering::Release);
        }
        e.poll_rebuffer(true, false).unwrap();
        if e.buffered_ranges().0 == vec![(0., 1.)] {
            break;
        }
        assert!(
            started.elapsed().as_secs() < 6,
            "Complete native source wasn't shown as full: {:?}",
            e.buffer_state.lock().unwrap().diagnostics
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            e.buffered_ranges(),
            (vec![(0., 1.)], false),
            "Complete source regressed to an estimated partial bar"
        );
    }
    e.stop().unwrap();
    eprintln!(
        "{}",
        serde_json::json!({"seeks_ms":elapsed,"buffered_ranges":ranges,"estimated":estimated,"quality_ranges_reset":true,"paused_reload":true})
    );
}
