//! Progressive history-based home feed. One seed request and one thumbnail job at a time.
use super::*;
use std::collections::HashSet;
fn publish(
    weak: &slint::Weak<SetupWindow>,
    state: &Arc<Mutex<State>>,
    paths: &Paths,
    generation: &Arc<AtomicU64>,
    n: u64,
    videos: &[Video],
    done: usize,
    total: usize,
    music: bool,
) {
    let state = state.clone();
    let paths = paths.clone();
    let videos = videos.to_vec();
    ui_generation(weak, generation, n, move |w| {
        state.lock().unwrap().videos = videos.clone();
        if music {
            music::sync_discovery_queue(&w, &state, &videos, false);
        }
        let previous = w.get_videos();
        let cards = videos
            .iter()
            .map(|v| {
                let mut card = cached_card(&paths, v);
                if let Some(old) = previous.iter().find(|row| row.source_id.as_str() == v.id) {
                    card.thumbnail = old.thumbnail;
                }
                card
            })
            .collect::<Vec<_>>();
        w.set_history_view(false);
        w.set_playlist_id("".into());
        w.set_collection_title(
            if w.get_browse_tab() == 5 {
                "From likes"
            } else if music {
                "For you · recent music"
            } else {
                "For you · recent history"
            }
            .into(),
        );
        browse_view::publish(&w, cards);
        w.set_status(
            format!(
                "{} · {done}/{total} seeds",
                if w.get_browse_tab() == 5 {
                    "Building From likes"
                } else {
                    "Building For You"
                }
            )
            .into(),
        );
    });
}
fn hydrate(
    weak: &slint::Weak<SetupWindow>,
    paths: &Paths,
    generation: &Arc<AtomicU64>,
    n: u64,
    videos: &[Video],
    loaded: &mut HashSet<String>,
) -> Result<()> {
    for video in videos {
        anyhow::ensure!(
            wait_for_playback(Some((generation, n))),
            "Request cancelled"
        );
        if loaded.contains(&video.id) {
            continue;
        }
        if let Ok((pixels, width, height)) = thumbnail(paths, video) {
            anyhow::ensure!(generation.load(Ordering::Relaxed) == n, "Request cancelled");
            let id = video.id.clone();
            loaded.insert(id.clone());
            ui_generation(weak, generation, n, move |w| {
                let model = w.get_videos();
                if let Some((i, mut row)) = model
                    .iter()
                    .enumerate()
                    .find(|(_, r)| r.source_id.as_str() == id)
                {
                    row.thumbnail = slint::Image::from_rgba8(slint::SharedPixelBuffer::<
                        slint::Rgba8Pixel,
                    >::clone_from_slice(
                        &pixels, width, height
                    ));
                    model.set_row_data(i, row);
                }
            });
        }
    }
    Ok(())
}
pub(super) fn load(
    weak: &slint::Weak<SetupWindow>,
    state: &Arc<Mutex<State>>,
    paths: &Paths,
    s: &Settings,
    history: &[Video],
    generation: &Arc<AtomicU64>,
    n: u64,
    seeds: Vec<Video>,
    loaded: &mut HashSet<String>,
) -> Result<Vec<Video>> {
    let mut classified = false;
    let result = fill_feed(
        &seeds,
        s,
        history,
        |seed, cursor| {
            let (page, next) = recommendations::home_page(&seed.id, cursor)?;
            anyhow::ensure!(generation.load(Ordering::Relaxed) == n, "Request cancelled");
            if page.classified {
                let mut st = state.lock().unwrap();
                if let Some(row) = st.history[usize::from(s.music)]
                    .iter_mut()
                    .find(|v| v.id == seed.id)
                {
                    if row.music != page.music {
                        row.music = page.music;
                        classified = true;
                    }
                }
            }
            Ok((page, next))
        },
        || {
            anyhow::ensure!(
                wait_for_playback(Some((generation, n))),
                "Request cancelled"
            );
            Ok(!paths.network.offline())
        },
        |videos, done, total| {
            anyhow::ensure!(generation.load(Ordering::Relaxed) == n, "Request cancelled");
            publish(
                weak, state, paths, generation, n, videos, done, total, s.music,
            );
            hydrate(weak, paths, generation, n, videos, loaded)
        },
    )?;
    anyhow::ensure!(generation.load(Ordering::Relaxed) == n, "Request cancelled");
    if classified {
        let st = state.lock().unwrap();
        if st.settings.remember && st.history_error.is_none() {
            let _ = super::history::save(paths, &st.history);
        }
    }
    crate::diagnostics::record(
        "recommendation_feed_complete",
        serde_json::json!({
            "music":s.music,"requested":recommendations::count(s),"rows":result.videos.len(),
            "requests":result.requests,"failed_sources":result.failures,"stop":result.stop
        }),
    );
    anyhow::ensure!(
        !result.videos.is_empty(),
        if result.failures > 0 {
            "Recommendation requests failed. Try again later; check your connection or YouTube availability."
        } else if s.recommendation_defer.is_empty() {
            "No matching suggestions available right now. Try again or adjust your recommendation filters."
        } else {
            "No fresh matching suggestions right now. Your current song keeps playing."
        }
    );
    Ok(result.videos)
}

const FEED_REQUEST_LIMIT: usize = 32;
struct FeedResult {
    videos: Vec<Video>,
    requests: usize,
    failures: usize,
    stop: &'static str,
}
// One initial request per unique seed, then only its unconsumed continuations.
// Keep at most target rows per seed; no watch-page refetch or unrelated padding.
fn fill_feed<C>(
    seeds: &[Video],
    s: &Settings,
    history: &[Video],
    mut fetch: impl FnMut(&Video, Option<C>) -> Result<(recommendations::Suggestions, Option<C>)>,
    mut ready: impl FnMut() -> Result<bool>,
    mut publish: impl FnMut(&[Video], usize, usize) -> Result<()>,
) -> Result<FeedResult> {
    let target = recommendations::count(s);
    let mut seen = HashSet::new();
    let seeds: Vec<_> = seeds
        .iter()
        .filter(|v| seen.insert(v.id.clone()))
        .take(10)
        .collect();
    let total = seeds.len();
    let mut groups = vec![Vec::new(); total];
    let mut cursors: Vec<Option<C>> = (0..total).map(|_| None).collect();
    let mut active = vec![true; total];
    let mut settings = s.clone();
    for seed in &seeds {
        settings.recommendation_avoid.push(seed.id.clone());
    }
    let mut requests = 0;
    let mut failures = 0;
    let mut stop = "exhausted";
    'pages: for round in 0..10 {
        for index in 0..total {
            if !active[index] {
                continue;
            }
            if requests >= FEED_REQUEST_LIMIT {
                stop = "request_budget";
                break 'pages;
            }
            if !ready()? {
                stop = "offline";
                break 'pages;
            }
            requests += 1;
            // Consume before requesting. Failure retires this source, never restarts it.
            let page = fetch(seeds[index], cursors[index].take());
            active[index] = false;
            match page {
                Ok((page, next)) => {
                    if !s.music || page.music != Some(false) {
                        let mut seed = (*seeds[index]).clone();
                        if page.classified {
                            seed.music = page.music;
                        }
                        let mut candidates =
                            recommendations::filter(page.videos, &settings, history, Some(&seed));
                        candidates.truncate(target - groups[index].len());
                        for row in &candidates {
                            settings.recommendation_avoid.push(row.id.clone());
                        }
                        append_unique(&mut groups[index], candidates, &[], target);
                        cursors[index] = next;
                        active[index] = cursors[index].is_some() && groups[index].len() < target;
                    }
                }
                Err(error) => {
                    failures += 1;
                    if recommendations::request_blocked(&error) {
                        stop = "upstream_rejected";
                        break 'pages;
                    }
                }
            }
            let partial = if round == 0 {
                let capped: Vec<_> = groups
                    .iter()
                    .enumerate()
                    .map(|(i, g)| {
                        g.iter()
                            .take(target / total + usize::from(i < target % total))
                            .cloned()
                            .collect()
                    })
                    .collect();
                recommendations::mix(&capped, target)
            } else {
                recommendations::mix(&groups, target)
            };
            publish(&partial, if round == 0 { index + 1 } else { total }, total)?;
            if round > 0 && partial.len() >= target {
                stop = "filled";
                break 'pages;
            }
        }
        let rows = recommendations::mix(&groups, target);
        if rows.len() >= target {
            stop = "filled";
            break;
        }
        if !active.iter().any(|&v| v) {
            break;
        }
    }
    let videos = recommendations::mix(&groups, target);
    publish(&videos, total, total)?;
    Ok(FeedResult {
        videos,
        requests,
        failures,
        stop,
    })
}

// Keep already-published candidates in place while extending an overlapping group.
fn append_unique(group: &mut Vec<Video>, candidates: Vec<Video>, seeds: &[Video], limit: usize) {
    let mut seen: HashSet<_> = group.iter().map(|v| v.id.clone()).collect();
    for video in candidates {
        if group.len() >= limit {
            break;
        }
        if !seeds.iter().any(|seed| seed.id == video.id) && seen.insert(video.id.clone()) {
            group.push(video);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(id: &str) -> Video {
        Video {
            id: id.into(),
            ..Video::default()
        }
    }
    #[test]
    fn overlapping_groups_refill_without_repeating_or_reordering_existing_rows() {
        let mut groups = vec![vec![row("one"), row("two")], vec![row("one"), row("two")]];
        assert_eq!(recommendations::mix(&groups, 4).len(), 2);
        append_unique(
            &mut groups[0],
            vec![
                row("one"),
                row("seed"),
                row("three"),
                row("four"),
                row("extra"),
            ],
            &[row("seed")],
            4,
        );
        assert_eq!(
            groups[0].iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["one", "two", "three", "four"]
        );
        assert_eq!(recommendations::mix(&groups, 4).len(), 4);
    }
    fn song(index: usize) -> Video {
        Video {
            id: format!("song{index:07}"),
            title: format!("Artist - Song {index}"),
            channel: "Artist".into(),
            duration: 180.,
            music: Some(true),
            views: Some(20_000),
            ..Video::default()
        }
    }
    fn settings(music: bool, limit: i32) -> Settings {
        Settings {
            music,
            recommendation_count: limit,
            hide_alternate_versions: false,
            minimum_recommendation_views: 10_000,
            ..Settings::default()
        }
    }
    #[test]
    fn music_and_video_fill_after_overlapping_pages_without_refetching_seeds() {
        for (music, limit) in [
            (true, 40),
            (false, 25),
            (false, 30),
            (false, 40),
            (false, 50),
        ] {
            let s = settings(music, limit);
            let seeds = [song(900), song(901), song(900)]; // A duplicate seed must not cause a second GET.
            let mut requests = HashSet::new();
            let result = fill_feed(
                &seeds,
                &s,
                &[],
                |seed, cursor: Option<usize>| {
                    let page = cursor.unwrap_or(0);
                    assert!(requests.insert((seed.id.clone(), page)), "repeated request");
                    let videos = if page == 0 {
                        (0..4).map(song).collect()
                    } else if seed.id == seeds[0].id {
                        (0..24).map(song).collect()
                    } else {
                        (20..80).map(song).collect()
                    };
                    Ok((
                        recommendations::Suggestions {
                            videos,
                            music: Some(true),
                            classified: true,
                        },
                        (page == 0).then_some(1),
                    ))
                },
                || Ok(true),
                |_, _, _| Ok(()),
            )
            .unwrap();
            assert_eq!(result.videos.len(), recommendations::count(&s));
            assert_eq!(result.stop, "filled");
            assert_eq!(result.requests, 4);
            assert_eq!(
                result
                    .videos
                    .iter()
                    .map(|v| &v.id)
                    .collect::<HashSet<_>>()
                    .len(),
                result.videos.len()
            );
        }
    }
    #[test]
    fn music_keeps_paging_after_duration_views_and_classification_filters() {
        let s = settings(true, 40);
        let result = fill_feed(
            &[song(900)],
            &s,
            &[],
            |_, cursor: Option<usize>| {
                let page = cursor.unwrap_or(0);
                let mut videos: Vec<_> = (0..60).map(song).collect();
                if page == 0 {
                    for (i, row) in videos.iter_mut().enumerate() {
                        if i < 20 {
                            row.duration = 3600.;
                        } else if i < 40 {
                            row.views = Some(100);
                        } else {
                            row.music = Some(false);
                        }
                    }
                }
                Ok((
                    recommendations::Suggestions {
                        videos,
                        music: Some(true),
                        classified: true,
                    },
                    (page == 0).then_some(1),
                ))
            },
            || Ok(true),
            |_, _, _| Ok(()),
        )
        .unwrap();
        assert_eq!(result.requests, 2);
        assert_eq!(result.videos.len(), 40);
        assert!(
            result
                .videos
                .iter()
                .all(|v| v.music != Some(false) && recommendations::song_duration(v))
        );
    }
    #[test]
    fn failed_or_exhausted_sources_are_never_restarted() {
        let s = settings(true, 40);
        let seeds = [song(900), song(901)];
        let mut calls = 0;
        let result = fill_feed(
            &seeds,
            &s,
            &[],
            |seed, cursor: Option<usize>| {
                calls += 1;
                assert!(cursor.is_none());
                if seed.id == seeds[0].id {
                    anyhow::bail!("HTTP 429");
                }
                Ok((
                    recommendations::Suggestions {
                        videos: (0..4).map(song).collect(),
                        music: Some(true),
                        classified: true,
                    },
                    None,
                ))
            },
            || Ok(true),
            |_, _, _| Ok(()),
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(result.failures, 1);
        assert_eq!(result.videos.len(), 4);
        assert_eq!(result.stop, "exhausted");
    }
    #[test]
    fn filtered_empty_pages_have_a_strict_request_budget() {
        let seeds: Vec<_> = (900..910).map(song).collect();
        let result = fill_feed(
            &seeds,
            &settings(true, 40),
            &[],
            |_, cursor: Option<usize>| {
                Ok((
                    recommendations::Suggestions {
                        videos: Vec::new(),
                        music: None,
                        classified: false,
                    },
                    Some(cursor.unwrap_or(0) + 1),
                ))
            },
            || Ok(true),
            |_, _, _| Ok(()),
        )
        .unwrap();
        assert_eq!(result.requests, FEED_REQUEST_LIMIT);
        assert_eq!(result.stop, "request_budget");
        assert!(result.videos.is_empty());
    }
    #[test]
    fn cancellation_stops_before_the_next_page() {
        let mut calls = 0;
        let mut checks = 0;
        let result = fill_feed(
            &[song(900)],
            &settings(true, 40),
            &[],
            |_, _: Option<usize>| {
                calls += 1;
                Ok((
                    recommendations::Suggestions {
                        videos: Vec::new(),
                        music: None,
                        classified: false,
                    },
                    Some(1),
                ))
            },
            || {
                checks += 1;
                anyhow::ensure!(checks == 1, "Request cancelled");
                Ok(true)
            },
            |_, _, _| Ok(()),
        );
        assert!(result.err().unwrap().to_string().contains("cancelled"));
        assert_eq!(calls, 1);
    }
    #[test]
    fn rejection_stops_the_feed_instead_of_requesting_more_seeds() {
        for status in [403, 429] {
            let seeds: Vec<_> = (900..910).map(song).collect();
            let mut calls = 0;
            let result = fill_feed(
                &seeds,
                &settings(true, 40),
                &[],
                |_, _: Option<usize>| {
                    calls += 1;
                    Err(anyhow::Error::new(ureq::Error::StatusCode(status))
                        .context("YouTube request"))
                },
                || Ok(true),
                |_, _, _| Ok(()),
            )
            .unwrap();
            assert_eq!(calls, 1);
            assert_eq!(result.failures, 1);
            assert_eq!(result.stop, "upstream_rejected");
        }
    }
}
