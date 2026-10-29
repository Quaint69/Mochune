//! On-demand text captions. No video readback, subtitle renderer, or extra media pipeline.
use super::*;
#[derive(Clone)]
pub(super) struct Track {
    label: String,
    language: String,
    url: String,
    automatic: bool,
}
#[derive(Debug)]
struct Cue {
    start: f64,
    end: f64,
    text: String,
}
#[derive(Default)]
struct Loaded {
    id: String,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    choice: usize,
}
pub(super) struct Controller {
    loaded: Mutex<Loaded>,
    epoch: Arc<AtomicU64>,
    tx: mpsc::SyncSender<()>,
    pending: Mutex<Option<Option<(u64, Track, bool)>>>,
    music_language: Mutex<MusicPreference>,
    preference_dirty: std::sync::atomic::AtomicBool,
}
pub(super) struct Guard {
    pub controller: Arc<Controller>,
    worker: Option<thread::JoinHandle<()>>,
    _timer: slint::Timer,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.controller.epoch.fetch_add(1, Ordering::Relaxed);
        *self.controller.pending.lock().unwrap() = Some(None);
        let _ = self.controller.tx.try_send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub(super) fn tracks(info: &Value) -> Vec<Track> {
    let mut result = Vec::new();
    for (key, auto) in [("subtitles", false), ("automatic_captions", true)] {
        if let Some(languages) = info[key].as_object() {
            for (language, formats) in languages {
                // Auto-translated copies can number in the hundreds. Keep original ASR only.
                if language == "live_chat" {
                    continue;
                }
                let Some(format) = formats.as_array().and_then(|fs| {
                    fs.iter().find(|f| {
                        f["ext"] == "vtt"
                            && f["url"]
                                .as_str()
                                .is_some_and(|u| !u.contains("/api/manifest/"))
                            && (!auto
                                || f["url"].as_str().is_some_and(|u| {
                                    !u.contains("&tlang=") && !u.contains("?tlang=")
                                }))
                    })
                }) else {
                    continue;
                };
                let Some(url) = format["url"].as_str().filter(|u| {
                    u.starts_with("https://")
                        || (info["external_source"] == true && u.starts_with("http://127.0.0.1:"))
                }) else {
                    continue;
                };
                let name = format["name"].as_str().unwrap_or(language);
                result.push(Track {
                    language: language.clone(),
                    label: format!("{name}{}", if auto { " (auto)" } else { "" }),
                    url: url.into(),
                    automatic: auto,
                });
                if result.len() >= 64 {
                    break;
                }
            }
        }
    }
    result.sort_by_key(|t| {
        (
            t.automatic,
            !t.label.to_lowercase().starts_with("english"),
            t.label.clone(),
        )
    });
    result.truncate(64);
    result
}
#[derive(Clone, Debug, PartialEq)]
struct MusicPreference {
    enabled: bool,
    language: String,
}
fn music_preference(value: &Value) -> MusicPreference {
    MusicPreference {
        enabled: value
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| value.as_str().is_some()),
        language: value
            .as_str()
            .or_else(|| value.get("language").and_then(Value::as_str))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("en")
            .chars()
            .take(64)
            .collect(),
    }
}
fn preference_json(preference: &MusicPreference) -> Value {
    serde_json::json!({"version":1,"enabled":preference.enabled,"language":preference.language})
}
fn preferred_choice(tracks: &[Track], language: Option<&str>) -> usize {
    let Some(language) = language else {
        return 0;
    };
    // An authored track wins over generated text; within each source prefer
    // the requested language, then English. Explicit picker choices still work.
    tracks
        .iter()
        .enumerate()
        .min_by_key(|(_, t)| {
            (
                t.automatic,
                if t.language == language {
                    0
                } else if t.language.trim_end_matches("-orig") == language.trim_end_matches("-orig")
                {
                    1
                } else if t.language.trim_end_matches("-orig") == "en" {
                    2
                } else {
                    3
                },
            )
        })
        .map_or(0, |(i, _)| i + 1)
}
impl Controller {
    fn publish_music_controls(&self, w: &SetupWindow) {
        let loaded = self.loaded.lock().unwrap();
        let preference = self.music_language.lock().unwrap();
        w.set_lyrics_enabled(preference.enabled);
        w.set_lyrics_available(!loaded.tracks.is_empty());
        let choice = preferred_choice(&loaded.tracks, Some(&preference.language));
        w.set_lyrics_language_choice(choice.saturating_sub(1) as i32);
        w.set_lyrics_language_label(if preference.language == "en" {
            "English".into()
        } else {
            preference.language.clone().into()
        });
        w.set_lyrics_languages(ModelRc::new(VecModel::from(
            loaded
                .tracks
                .iter()
                .map(|t| slint::SharedString::from(&t.label))
                .collect::<Vec<_>>(),
        )));
    }

    fn choose(&self, w: &SetupWindow, choice: i32, remember: bool) {
        let epoch = self.epoch.fetch_add(1, Ordering::Relaxed) + 1;
        let mut loaded = self.loaded.lock().unwrap();
        loaded.cues.clear();
        loaded.choice = choice.max(0) as usize;
        w.set_caption_choice(choice.max(0));
        w.set_caption_text("".into());
        w.set_caption_next("".into());
        w.set_caption_status("".into());
        let track = loaded
            .choice
            .checked_sub(1)
            .and_then(|i| loaded.tracks.get(i))
            .cloned();
        drop(loaded);
        if remember && w.get_music_layout() {
            let mut preference = self.music_language.lock().unwrap();
            preference.enabled = track.is_some();
            if let Some(track) = &track {
                preference.language = track.language.clone();
            }
            self.preference_dirty.store(true, Ordering::Release);
        }
        if w.get_music_layout() {
            self.publish_music_controls(w);
        }
        if let Some(track) = track {
            w.set_caption_status(
                if w.get_music_layout() {
                    "Loading lyrics…"
                } else {
                    "Loading captions…"
                }
                .into(),
            );
            *self.pending.lock().unwrap() = Some(Some((epoch, track, w.get_music_layout())));
        } else {
            *self.pending.lock().unwrap() = None;
        }
        let _ = self.tx.try_send(());
    }

    pub fn publish(&self, w: &SetupWindow, id: &str, tracks: Vec<Track>) {
        let mut loaded = self.loaded.lock().unwrap();
        if loaded.id == id
            && loaded
                .tracks
                .iter()
                .map(|t| (&t.language, &t.label))
                .eq(tracks.iter().map(|t| (&t.language, &t.label)))
        {
            loaded.tracks = tracks;
            return;
        }
        self.epoch.fetch_add(1, Ordering::Relaxed);
        let music = w.get_music_layout();
        let preferred = if music {
            let preference = self.music_language.lock().unwrap();
            preferred_choice(
                &tracks,
                preference.enabled.then_some(preference.language.as_str()),
            )
        } else if id.starts_with("external:1:") {
            tracks
                .iter()
                .position(|t| {
                    t.language == "en"
                        || t.label.to_ascii_lowercase().starts_with("english")
                        || t.label.eq_ignore_ascii_case("en")
                })
                .map_or(0, |i| i + 1)
        } else {
            0
        };
        let labels = std::iter::once(slint::SharedString::from(if music {
            if tracks.is_empty() {
                "No lyrics / captions"
            } else {
                "Lyrics off"
            }
        } else {
            "Captions off"
        }))
        .chain(tracks.iter().map(|t| t.label.clone().into()))
        .collect::<Vec<_>>();
        *loaded = Loaded {
            id: id.into(),
            tracks,
            ..Loaded::default()
        };
        w.set_caption_choice(0);
        w.set_caption_text("".into());
        w.set_caption_next("".into());
        w.set_caption_status("".into());
        w.set_caption_labels(ModelRc::new(VecModel::from(labels)));
        drop(loaded);
        if music {
            self.publish_music_controls(w);
        }
        w.set_lyric_previous("".into());
        w.set_lyric_following("".into());
        if preferred > 0 {
            self.choose(w, preferred as i32, false);
        }
    }
}
fn timestamp(s: &str) -> Option<f64> {
    let mut value = 0.;
    let mut parts = 0;
    for token in s.trim().split(':') {
        value = value * 60. + token.replace(',', ".").parse::<f64>().ok()?;
        parts += 1;
    }
    (parts >= 2 && value.is_finite() && value >= 0.).then_some(value)
}
fn plain(s: &str) -> String {
    let mut result = String::new();
    let mut tag = false;
    for c in s.chars().take(4096) {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag => result.push(c),
            _ => {}
        }
    }
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .chars()
        .take(512)
        .collect()
}
fn parse(input: &str) -> Vec<Cue> {
    parse_vtt(input, false)
}

// YouTube ASR repeats the preceding line and grows the next line word by word.
// Build stable lines once at load time, rather than reflowing them every tick.
fn push_auto_line(cues: &mut Vec<Cue>, cue: Cue) {
    let recent = cues.len().saturating_sub(2);
    if let Some(existing) = cues[recent..]
        .iter_mut()
        .rev()
        .find(|c| c.text == cue.text && cue.start <= c.end + 0.15)
    {
        existing.end = existing.end.max(cue.end);
        return;
    }
    if let Some(last) = cues.last_mut() {
        if cue.start <= last.end + 0.15 && cue.start - last.start <= 15.0 {
            if cue.text.starts_with(&last.text) {
                last.text = cue.text;
                last.end = last.end.max(cue.end);
                return;
            }
            if last.text.starts_with(&cue.text) {
                last.end = last.end.max(cue.end);
                return;
            }
        }
    }
    if cues.len() < 10_000 {
        cues.push(cue);
    }
}

fn parse_vtt(input: &str, automatic: bool) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let mut lines = input.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((a, b)) = line.split_once(" --> ") else {
            continue;
        };
        let (Some(start), Some(end)) = (
            timestamp(a),
            b.split_whitespace().next().and_then(timestamp),
        ) else {
            continue;
        };
        let mut text = String::new();
        while let Some(line) = lines.next_if(|s| !s.trim().is_empty()) {
            if text.len() < 4096 {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.extend(line.chars().take(1024));
            }
        }
        if automatic && end > start {
            for line in text.lines() {
                let clean = plain(line).split_whitespace().collect::<Vec<_>>().join(" ");
                if clean.is_empty() {
                    continue;
                }
                // Inline word timestamps identify when a newly added line starts.
                let line_start = line
                    .split('<')
                    .skip(1)
                    .filter_map(|part| part.split_once('>'))
                    .find_map(|(tag, _)| timestamp(tag))
                    .filter(|t| *t >= start && *t < end)
                    .unwrap_or(start);
                push_auto_line(
                    &mut cues,
                    Cue {
                        start: line_start,
                        end,
                        text: clean,
                    },
                );
            }
        } else {
            let text = plain(&text)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if end > start && !text.is_empty() {
                cues.push(Cue { start, end, text });
            }
        }
        if cues.len() >= 10_000 {
            break;
        }
    }
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    if automatic {
        // Carried-over ASR text can outlive the next line's start. Stable lyric
        // lines no longer overlap, so the normal scrolling transition can run.
        for i in 0..cues.len().saturating_sub(1) {
            cues[i].end = cues[i].end.min(cues[i + 1].start);
        }
    }
    cues
}
fn caption_pair(cues: &[Cue], pos: f64) -> (&str, &str) {
    let index = cues.partition_point(|c| c.start <= pos);
    // Rolling captions can overlap: the newest active cue is the current line.
    let Some(current) = cues[..index].iter().rev().take(8).find(|c| c.end > pos) else {
        return ("", "");
    };
    let next = cues[index..]
        .iter()
        .take(32)
        .find(|c| c.text != current.text);
    (&current.text, next.map(|c| c.text.as_str()).unwrap_or(""))
}
// Start scrolling just before the cue to absorb the 260 ms animation and timer tick.
// Video captions keep their original timestamps.
const MUSIC_LYRIC_LEAD_SECONDS: f64 = 0.30;

fn music_window(cues: &[Cue], pos: f64) -> [&str; 4] {
    let pos = pos + MUSIC_LYRIC_LEAD_SECONDS;
    let after = cues.partition_point(|c| c.start <= pos);
    if after == 0 {
        return [
            "",
            "",
            cues.first().map_or("", |c| c.text.as_str()),
            cues.get(1).map_or("", |c| c.text.as_str()),
        ];
    }
    let index = after - 1;
    let current = cues[index].text.as_str();
    let previous = cues[..index]
        .iter()
        .rev()
        .take(32)
        .find(|c| c.text != current)
        .map_or("", |c| c.text.as_str());
    let mut upcoming = cues[after..]
        .iter()
        .take(64)
        .filter(|c| c.text != current)
        .map(|c| c.text.as_str());
    let next = upcoming.next().unwrap_or("");
    let following = upcoming.find(|text| *text != next).unwrap_or("");
    [previous, current, next, following]
}
// Overlapping cue times are common even in authored captions. Animate a normal
// adjacent text advance; jumps, first display and language replacement stay direct.
fn animate_music_step(old: &str, old_next: &str, previous: &str, current: &str) -> bool {
    !old.is_empty() && current != old && previous == old && current == old_next
}

// Source HLS captions are commonly one complete WebVTT file behind a tiny
// playlist. Resolve only on explicit selection, with a fixed response budget.
fn caption_document(url: &str, epoch: &AtomicU64, expected: u64) -> Result<Vec<u8>> {
    const LIMIT: usize = 2 * 1024 * 1024;
    let fetch = |url: &str, limit: usize| -> Result<Vec<u8>> {
        anyhow::ensure!(
            epoch.load(Ordering::Relaxed) == expected,
            "Caption request cancelled"
        );
        let bytes = metadata_agent()
            .get(url)
            .call()?
            .body_mut()
            .with_config()
            .limit((limit + 1) as u64)
            .read_to_vec()?;
        anyhow::ensure!(bytes.len() <= limit, "Caption text exceeds the size limit");
        Ok(bytes)
    };
    let bytes = fetch(url, LIMIT)?;
    if !bytes.starts_with(b"#EXTM3U") {
        return Ok(bytes);
    }
    let base = url::Url::parse(url)?;
    anyhow::ensure!(
        base.scheme() == "http" && base.host_str() == Some("127.0.0.1"),
        "Unsupported caption playlist"
    );
    let text = std::str::from_utf8(&bytes)?;
    let targets = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        targets.len() == 1,
        "This source uses segmented captions that are not supported yet"
    );
    let target = base.join(targets[0])?;
    anyhow::ensure!(
        target.scheme() == base.scheme()
            && target.host_str() == base.host_str()
            && target.port() == base.port()
            && target.path_segments().and_then(|mut p| p.next())
                == base.path_segments().and_then(|mut p| p.next()),
        "Caption playlist left its source session"
    );
    let payload = fetch(target.as_str(), LIMIT.saturating_sub(bytes.len()))?;
    anyhow::ensure!(
        !payload.starts_with(b"#EXTM3U"),
        "Nested caption playlists are not supported"
    );
    Ok(payload)
}
pub(super) fn install(w: &SetupWindow, paths: Paths) -> Guard {
    w.set_caption_labels(ModelRc::new(VecModel::from(vec![
        slint::SharedString::from("Captions off"),
    ])));
    let (tx, rx) = mpsc::sync_channel(1);
    let controller = Arc::new(Controller {
        loaded: Mutex::new(Loaded::default()),
        epoch: Arc::new(AtomicU64::new(0)),
        tx,
        pending: Mutex::new(None),
        music_language: Mutex::new(music_preference(&read_json::<Value>(
            paths.data.join("music-caption-language.json"),
        ))),
        preference_dirty: std::sync::atomic::AtomicBool::new(false),
    });
    controller.publish_music_controls(w);
    let control = controller.clone();
    let weak = w.as_weak();
    let worker = thread::spawn(move || {
        while rx.recv().is_ok() {
            // Save only the small preference; caption text is never cached implicitly.
            if control.preference_dirty.swap(false, Ordering::AcqRel) {
                let language = control.music_language.lock().unwrap().clone();
                if let Err(e) =
                    paths.save("music-caption-language.json", &preference_json(&language))
                {
                    ui(&weak, move |w| {
                        w.set_caption_status(
                            format!("Could not remember lyrics language: {e}").into(),
                        )
                    });
                }
            }
            let Some(job) = control.pending.lock().unwrap().take() else {
                continue;
            };
            let Some((epoch, track, music)) = job else {
                break;
            };
            if control.epoch.load(Ordering::Relaxed) != epoch {
                continue;
            }
            let result = (|| -> Result<Vec<Cue>> {
                anyhow::ensure!(
                    wait_for_playback(Some((&control.epoch, epoch))),
                    "Caption request cancelled"
                );
                let bytes = if track.url.starts_with("http://127.0.0.1:") {
                    caption_document(&track.url, &control.epoch, epoch)?
                } else {
                    metadata_agent()
                        .get(&track.url)
                        .call()?
                        .body_mut()
                        .with_config()
                        .limit(2 * 1024 * 1024)
                        .read_to_vec()?
                };
                let text = std::str::from_utf8(&bytes)?;
                let cues = if music && track.automatic {
                    parse_vtt(text, true)
                } else {
                    parse(text)
                };
                anyhow::ensure!(!cues.is_empty(), "No usable caption text was returned");
                Ok(cues)
            })();
            let mut loaded = control.loaded.lock().unwrap();
            if control.epoch.load(Ordering::Relaxed) != epoch {
                continue;
            }
            let status = match result {
                Ok(cues) => {
                    loaded.cues = cues;
                    String::new()
                }
                Err(_) => {
                    "Captions unavailable from YouTube. Try another language or turn them off."
                        .into()
                }
            };
            ui_generation(&weak, &control.epoch, epoch, move |w| {
                w.set_caption_status(status.into())
            });
        }
    });
    let control = controller.clone();
    let weak = w.as_weak();
    w.on_reset_captions(move || {
        if let Some(w) = weak.upgrade() {
            control.publish(&w, "", Vec::new());
        }
    });
    let control = controller.clone();
    let weak = w.as_weak();
    w.on_toggle_lyrics(move || {
        let Some(w) = weak.upgrade() else {
            return;
        };
        let choice = {
            let loaded = control.loaded.lock().unwrap();
            let mut preference = control.music_language.lock().unwrap();
            preference.enabled = !preference.enabled;
            control.preference_dirty.store(true, Ordering::Release);
            preferred_choice(
                &loaded.tracks,
                preference.enabled.then_some(preference.language.as_str()),
            )
        };
        control.choose(&w, choice as i32, false);
    });
    let control = controller.clone();
    let weak = w.as_weak();
    w.on_choose_caption(move |choice| {
        let Some(w) = weak.upgrade() else {
            return;
        };
        control.choose(&w, choice, true);
    });
    let control = controller.clone();
    let weak = w.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let mut loaded = control.loaded.lock().unwrap();
            if loaded.choice == 0
                && w.get_caption_text().is_empty()
                && w.get_caption_next().is_empty()
            {
                return;
            }
            if w.get_selected_id().as_str() != loaded.id {
                loaded.cues.clear();
                loaded.choice = 0;
                control.epoch.fetch_add(1, Ordering::Relaxed);
                w.set_caption_choice(0);
                w.set_caption_text("".into());
                w.set_caption_next("".into());
                w.set_caption_status("".into());
                return;
            }
            if w.get_music_layout() {
                let [previous, current, next, following] =
                    music_window(&loaded.cues, w.get_position() as f64);
                let advanced = w.get_caption_text().as_str() != current;
                let animate = animate_music_step(
                    w.get_caption_text().as_str(),
                    w.get_caption_next().as_str(),
                    previous,
                    current,
                );
                if w.get_lyric_previous().as_str() != previous {
                    w.set_lyric_previous(previous.into());
                }
                if advanced {
                    w.set_caption_text(current.into());
                }
                if w.get_caption_next().as_str() != next {
                    w.set_caption_next(next.into());
                }
                if w.get_lyric_following().as_str() != following {
                    w.set_lyric_following(following.into());
                }
                if advanced {
                    w.set_lyrics_animate(animate);
                    w.set_lyrics_frame(w.get_lyrics_frame().wrapping_add(1));
                }
                return;
            }
            let (text, next) = caption_pair(&loaded.cues, w.get_position() as f64);
            if w.get_caption_text().as_str() != text {
                w.set_caption_text(text.into());
            }
            if w.get_caption_next().as_str() != next {
                w.set_caption_next(next.into());
            }
        },
    );
    Guard {
        controller,
        worker: Some(worker),
        _timer: timer,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_captions_precede_automatic_even_for_exact_language_match() {
        let track = |lang: &str, automatic| Track {
            language: lang.into(),
            automatic,
            label: lang.into(),
            url: String::new(),
        };
        let tracks = [
            track("en", true),
            track("en-orig", false),
            track("ja", false),
        ];
        assert_eq!(preferred_choice(&tracks, Some("en")), 2);
        assert_eq!(preferred_choice(&tracks, Some("ja")), 3);
        assert_eq!(preferred_choice(&tracks[..1], Some("en")), 1);
        assert_eq!(preferred_choice(&[], Some("en")), 0);
    }

    #[test]
    fn lyrics_default_off_and_remember_explicit_or_legacy_state() {
        assert_eq!(
            music_preference(&Value::Null),
            MusicPreference {
                enabled: false,
                language: "en".into()
            }
        );
        let preference = MusicPreference {
            enabled: false,
            language: "ja".into(),
        };
        assert_eq!(music_preference(&preference_json(&preference)), preference);
        assert_eq!(music_preference(&serde_json::json!("ja")).language, "ja");
        assert!(music_preference(&serde_json::json!("ja")).enabled);
        assert!(music_preference(&serde_json::json!({"enabled":true,"language":"en"})).enabled);
        assert!(!music_preference(&serde_json::json!({"enabled":false,"language":"en"})).enabled);
    }
    #[test]
    fn music_animation_handles_overlaps_but_not_seek_or_language_jumps() {
        let cues = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:06.000\nFirst\n\n00:00:04.000 --> 00:00:08.000\nSecond\n\n00:00:07.000 --> 00:00:10.000\nThird\n",
        );
        let before = music_window(&cues, 2.);
        let after = music_window(&cues, 4.);
        assert!(animate_music_step(before[1], before[2], after[0], after[1]));
        let jump = music_window(&cues, 8.);
        assert!(!animate_music_step(before[1], before[2], jump[0], jump[1]));
        assert!(!animate_music_step(
            after[1], after[2], before[0], before[1]
        ));
        assert!(!animate_music_step("", "First", "", "First"));
        assert!(!animate_music_step(
            "First",
            "Second",
            "First",
            "Translated"
        ));
    }

    #[test]
    fn scrolling_music_stays_visible_through_gaps_and_tracks_seeks() {
        let cues = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nFirst\n\n00:00:05.000 --> 00:00:06.000\nSecond\n\n00:00:08.000 --> 00:00:09.000\nThird\n",
        );
        assert_eq!(music_window(&cues, 3.), ["", "First", "Second", "Third"]);
        assert_eq!(music_window(&cues, 4.69)[1], "First");
        assert_eq!(music_window(&cues, 4.71), ["First", "Second", "Third", ""]);
        // A backward seek recomputes the early window, while video timing stays exact.
        assert_eq!(music_window(&cues, 4.0)[1], "First");
        assert_eq!(caption_pair(&cues, 4.71).0, "");
        assert_eq!(music_window(&cues, 5.5), ["First", "Second", "Third", ""]);
        assert_eq!(music_window(&cues, 20.)[1], "Third");
        assert_eq!(music_window(&cues, 1.5)[1], "First");
        assert_eq!(music_window(&cues, 0.)[1], "");
        assert_eq!(caption_pair(&cues, 3.).0, "");
    }
    #[test]
    fn music_language_preference_uses_language_not_picker_position() {
        let info = serde_json::json!({"subtitles":{"en":[{"ext":"vtt","url":"https://example.org/en"}],"ja":[{"ext":"vtt","url":"https://example.org/ja"}]}});
        let mut list = tracks(&info);
        let i = preferred_choice(&list, Some("ja"));
        assert_eq!(list[i - 1].language, "ja");
        list.reverse();
        assert_eq!(list[preferred_choice(&list, Some("ja")) - 1].language, "ja");
        assert_eq!(list[preferred_choice(&list, Some("fr")) - 1].language, "en");
        assert_eq!(preferred_choice(&list, None), 0);
    }
    #[test]
    fn captions_seek_gaps_tags_and_overlaps() {
        let cues = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\n<c>one &amp; two</c>\n\n00:00:02.000 --> 00:00:04.000 align:start\n<00:00:02.000><c>newer</c>\n\n00:00:08.000 --> 00:00:09.000\nlast\n",
        );
        assert_eq!(caption_pair(&cues, 0.).0, "");
        assert_eq!(caption_pair(&cues, 1.5).0, "one & two");
        assert_eq!(caption_pair(&cues, 2.5).0, "newer");
        assert_eq!(caption_pair(&cues, 5.).0, "");
        assert_eq!(caption_pair(&cues, 8.5).0, "last");
        assert_eq!(caption_pair(&cues, 1.5).0, "one & two");
    }
    #[test]
    fn current_above_next_skips_duplicate_cues_and_resets_on_seek_or_gap() {
        let cues = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nCurrent\nline\n\n00:00:02.000 --> 00:00:04.000\nCurrent line\n\n00:00:05.000 --> 00:00:06.000\nAhead\n\n00:00:07.000 --> 00:00:08.000\nLast\n",
        );
        assert_eq!(caption_pair(&cues, 1.5), ("Current line", "Ahead"));
        assert_eq!(caption_pair(&cues, 2.5), ("Current line", "Ahead"));
        assert_eq!(caption_pair(&cues, 4.5), ("", ""));
        assert_eq!(caption_pair(&cues, 5.5), ("Ahead", "Last"));
        assert_eq!(caption_pair(&cues, 7.5), ("Last", ""));
        assert_eq!(caption_pair(&cues, 1.5), ("Current line", "Ahead"));
    }
    #[test]
    fn automatic_captions_combine_growth_and_skip_rolling_carry_over() {
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\n<c>I tell</c>\n\n00:00:01.500 --> 00:00:03.000\n<c>I tell them take it easy</c>\n\n00:00:03.000 --> 00:00:06.000\nI tell them take it easy\n<00:00:03.100><c>They laugh</c>\n\n00:00:04.000 --> 00:00:06.000\nI tell them take it easy\n<00:00:04.100><c>They laugh and tell me no</c>\n\n00:00:20.000 --> 00:00:22.000\nI tell them take it easy\n";
        let cues = parse_vtt(vtt, true);
        assert_eq!(cues.len(), 3);
        assert_eq!(cues[0].text, "I tell them take it easy");
        assert_eq!(cues[0].start, 1.0);
        assert_eq!(cues[1].text, "They laugh and tell me no");
        assert_eq!(cues[1].start, 3.1);
        assert!(cues[0].end <= cues[1].start);
        assert_eq!(cues[2].start, 20.0); // Repeated chorus later is preserved.
        assert_eq!(music_window(&cues, 1.1)[1], "I tell them take it easy");
        assert_eq!(music_window(&cues, 3.2)[1], "They laugh and tell me no");
        assert_eq!(music_window(&cues, 1.1)[1], "I tell them take it easy");
        // Authored captions retain their original grouping and timing.
        assert_eq!(parse(vtt).len(), 5);
    }

    #[test]
    fn automatic_captions_preserve_distinct_lines_and_unspaced_growth() {
        let cues = parse_vtt(
            "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\n君\n\n00:00:02.000 --> 00:00:03.000\n君の歌\n\n00:00:04.000 --> 00:00:06.000\n次の行\n",
            true,
        );
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "君の歌");
        assert_eq!(cues[1].text, "次の行");
    }

    #[test]
    fn filter_translations_and_live_chat() {
        let info = serde_json::json!({"subtitles":{"live_chat":[{"ext":"vtt","url":"https://a"}],"en":[{"ext":"vtt","url":"https://a"}]},"automatic_captions":{"fr":[{"ext":"vtt","url":"https://a?tlang=fr"}],"en-orig":[{"ext":"vtt","url":"https://a?lang=en"}]}});
        let tracks = tracks(&info);
        assert_eq!(tracks.len(), 2);
        assert!(tracks[1].label.contains("auto"));
    }
}
