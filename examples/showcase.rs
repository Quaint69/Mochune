//! Capture the real Slint interface with original artwork and a disposable demo library.
//! This is a static interface preview; it does not initialize playback or make network requests.
use mochune_native::{PlaylistCard, QueueCard, SetupWindow, VideoCard};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{path::PathBuf, time::Duration};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("video");
    if !matches!(mode, "video" | "music" | "lyrics" | "queue" | "caption") {
        return Err("Expected video, music, lyrics, queue or caption".into());
    }
    let music = matches!(mode, "music" | "lyrics");
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/assets");
    let output = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| assets.join(format!("{mode}.png")));
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
        .select()?;
    let w = SetupWindow::new()?;
    w.window().set_size(slint::LogicalSize::new(1280., 800.));
    w.set_music_layout(music);
    w.set_music_only(music);
    w.set_immersive(true);
    w.set_has_selection(true);
    w.set_selection_playable(true);
    w.set_playback_active(true);
    w.set_paused(false);
    w.set_position(95.);
    w.set_duration(226.);
    w.set_clock("1:35 / 3:46".into());
    w.set_volume(70.);
    w.set_controls_shown(true);
    w.set_selected_id("demo-1".into());
    w.on_song_liked(|id, _| id == "demo-1");
    w.set_selected_title(
        if music {
            "Golden hour"
        } else {
            "Somewhere quiet · a short escape"
        }
        .into(),
    );
    w.set_selected_channel("Mochune demo".into());
    w.set_selected_image(slint::Image::load_from_path(&assets.join(if music {
        "record.png"
    } else {
        "landscape-1.png"
    }))?);
    w.set_video_frame(slint::Image::load_from_path(
        &assets.join("landscape-1.png"),
    )?);
    w.set_video_visible(!music);
    w.set_video_stats("Original artwork · Interface preview".into());
    w.set_quality_labels(ModelRc::new(VecModel::from(vec!["720p · 30 fps".into()])));
    w.set_quality_compact_labels(ModelRc::new(VecModel::from(vec!["720@30".into()])));
    w.set_quality_choice(0);
    if mode == "caption" {
        w.set_caption_text("The current caption".into());
        w.set_caption_next("The next caption appears here".into());
        w.set_controls_outside(std::env::var_os("MOCHUNE_SHOWCASE_OUTSIDE").is_some());
        if std::env::var_os("MOCHUNE_SHOWCASE_FULLSCREEN").is_some() {
            w.set_fullscreen_mode(true);
        }
    }

    w.set_caption_labels(ModelRc::new(VecModel::from(if mode == "caption" {
        vec![
            "Captions off".into(),
            "English · en".into(),
            "Japanese · ja".into(),
        ]
    } else {
        vec!["Captions off".into()]
    })));
    if mode == "caption" {
        w.set_caption_choice(1);
    }
    if mode == "lyrics" {
        w.set_lyrics_available(true);
        w.set_lyrics_enabled(true);
        w.set_lyrics_languages(ModelRc::new(VecModel::from(vec![
            "English".into(),
            "Japanese".into(),
        ])));
        w.set_lyric_previous("An earlier line fades away".into());
        w.set_lyric_following("Another line waits below".into());
        w.set_caption_labels(ModelRc::new(VecModel::from(vec![
            "Lyrics off".into(),
            "English · en".into(),
            "Japanese · ja".into(),
        ])));
        w.set_caption_choice(1);
        w.set_caption_text("I tell them take it easy, they laugh and tell me no, but I keep going wherever the music takes me".into());
        w.set_caption_next("It’s cool but I don’t see them laughing at my money though".into());
    }

    w.set_playlist_names(ModelRc::new(VecModel::from(vec![
        "Liked songs".into(),
        "Evening rotation".into(),
        "On the road".into(),
    ])));
    w.set_playlists(ModelRc::new(VecModel::from(vec![
        PlaylistCard {
            id: "liked-songs".into(),
            name: "Liked songs".into(),
            count: "8 tracks".into(),
        },
        PlaylistCard {
            id: "evening".into(),
            name: "Evening rotation".into(),
            count: "8 tracks".into(),
        },
        PlaylistCard {
            id: "road".into(),
            name: "On the road".into(),
            count: "5 tracks".into(),
        },
    ])));
    w.set_save_to(1);
    if music {
        w.set_playlist_id("evening".into());
        w.set_collection_title("Evening rotation".into());
    }
    let tracks = [
        "Slow mornings",
        "Golden hour",
        "A little further",
        "After the rain",
        "Soft focus",
        "Night drive",
        "The quiet side",
        "Home again",
    ];
    let films = [
        "A little further · cinematic landscapes",
        "After the rain | a moment of calm",
        "Where the mountains meet the sky",
        "Night drive · city lights",
        "The quiet side of the world",
        "Golden hour in the hills",
        "A different perspective",
        "Home again · a short film",
    ];
    let cards = (0..8)
        .map(|i| -> Result<VideoCard, Box<dyn std::error::Error>> {
            Ok(VideoCard {
                title: if music { tracks[i] } else { films[i] }.into(),
                channel: if music {
                    "Mochune demo sessions"
                } else {
                    "Mochune demo"
                }
                .into(),
                duration_text: [
                    "4:12", "3:46", "5:08", "3:21", "4:36", "6:14", "3:58", "4:09",
                ][i]
                    .into(),
                thumbnail: slint::Image::load_from_path(
                    &assets.join(format!("landscape-{}.png", i + 1)),
                )?,
                source_id: format!("demo-{i}").into(),
                available: true,
                music_available: true,
                ..Default::default()
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cards = if let Ok(count) = std::env::var("MOCHUNE_SHOWCASE_ROWS") {
        (0..count.parse::<usize>()?.min(50))
            .map(|i| {
                let mut card = cards[i % cards.len()].clone();
                card.source_id = format!("demo-{i}").into();
                card
            })
            .collect::<Vec<_>>()
    } else {
        cards
    };
    if !music {
        w.set_save_to(-1);
        w.set_playlists(Default::default());
        w.set_playlist_names(Default::default());
    }
    if mode == "queue" {
        w.set_left_pane_hovered(true);
        w.set_queue_rows(ModelRc::new(VecModel::from(vec![
            QueueCard{title:"Somewhere quiet · a short escape".into(),channel:"Creator is intentionally hidden".into(),active:true},
            QueueCard{title:"A very long commentary title that demonstrates two lines and an ellipsis for everything that will not fit".into(),channel:"Hidden".into(),active:false},
            QueueCard{title:"After the rain".into(),channel:"Hidden".into(),active:false},
        ])));
        w.set_video_playlist_id("watch-later".into());
        w.set_video_playlist_name("Watch later".into());
        w.set_save_to(0);
        w.set_playlists(ModelRc::new(VecModel::from(vec![PlaylistCard {
            id: "watch-later".into(),
            name: "Watch later".into(),
            count: "8 videos".into(),
        }])));
        w.set_playlist_names(ModelRc::new(VecModel::from(vec!["Watch later".into()])));
        w.set_video_playlist_rows(ModelRc::new(VecModel::from(cards.clone())));
    }
    w.set_rec_indices(ModelRc::new(VecModel::from(
        (0..cards.len() as i32).collect::<Vec<_>>(),
    )));
    w.set_videos(ModelRc::new(VecModel::from(cards)));
    // Optional settings/layout fixtures exercise the actual widgets without a network or player.
    w.on_split_terms(|value| {
        ModelRc::new(VecModel::from(
            value
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(slint::SharedString::from)
                .collect::<Vec<_>>(),
        ))
    });
    if let Ok(tab) = std::env::var("MOCHUNE_SHOWCASE_SETTINGS") {
        w.set_settings_tab(tab.parse().unwrap_or(0));
        w.set_settings_open(true);
        w.set_interests("Science, Animation, Jazz".into());
        w.set_blocked_channels("Example channel".into());
        w.set_blocked_keywords("Spoilers, Reaction".into());
    }
    if std::env::var_os("MOCHUNE_SHOWCASE_SKELETON").is_some() {
        w.set_videos(ModelRc::new(VecModel::<VideoCard>::default()));
        w.set_rec_indices(ModelRc::new(VecModel::from(vec![-1; 8])));
        w.set_busy(true);
    }
    if let Ok(size) = std::env::var("MOCHUNE_SHOWCASE_SIZE") {
        if let Some((width, height)) = size.split_once('x') {
            w.window()
                .set_size(slint::LogicalSize::new(width.parse()?, height.parse()?));
        }
    }
    if std::env::var_os("MOCHUNE_SHOWCASE_CACHED").is_some() {
        w.set_playlist_id("".into());
        w.set_collection_title("Cached music".into());
        w.set_browse_tab(4);
        w.set_audio_cache_enabled(true);
    }
    if let Ok(dialog) = std::env::var("MOCHUNE_SHOWCASE_DIALOG") {
        if dialog == "playlist" {
            w.set_playlist_name("Evening favorites".into());
            w.set_playlist_editor(true);
        } else if dialog == "download" {
            w.set_download_title("Somewhere quiet · a short escape".into());
            w.set_download_folder("Downloads/Mochune".into());
            w.set_download_labels(ModelRc::new(VecModel::from(vec![
                "720p · 30 fps · ≈25 MB".into(),
            ])));
            w.set_download_ready(true);
            w.set_download_status("Ready to download".into());
            w.set_download_open(true);
        }
    }
    if let Ok(offset) = std::env::var("MOCHUNE_SHOWCASE_SETTINGS_SCROLL") {
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(w) = weak.upgrade() {
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(850., 450.),
                        delta_x: 0.,
                        delta_y: offset.parse().unwrap_or(-400.),
                    });
                eprintln!("settings wheel scroll: {}", w.get_settings_scroll_offset());
                // Some sections fit entirely; only require movement for overflow fixtures.
                if std::env::var_os("MOCHUNE_SHOWCASE_EXPECT_SCROLL").is_some() {
                    assert!(
                        w.get_settings_scroll_offset() < 0.,
                        "settings must respond to the wheel"
                    );
                }
            }
        });
    }
    if let Ok(offset) = std::env::var("MOCHUNE_SHOWCASE_LIST_SCROLL") {
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(w) = weak.upgrade() {
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(
                            if music { 650. } else { 1100. },
                            350.,
                        ),
                        delta_x: 0.,
                        delta_y: offset.parse().unwrap_or(-400.),
                    });
                eprintln!("list wheel scroll: {}", w.get_results_scroll());
                assert!(
                    w.get_results_scroll() < 0.,
                    "list must respond to the wheel"
                );
            }
        });
    }
    if let Ok(path) = std::env::var("MOCHUNE_SHOWCASE_INPUT") {
        let input: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        if let Some(tracks) = input["tracks"].as_array() {
            let rows = tracks.iter().map(|v| -> Result<VideoCard, Box<dyn std::error::Error>> {
                let seconds = v["duration"].as_f64().unwrap_or(0.) as u64;
                Ok(VideoCard {
                    source_id: v["id"].as_str().unwrap_or("").into(),
                    title: v["title"].as_str().unwrap_or("").into(),
                    channel: v["channel"].as_str().unwrap_or("").into(),
                    views_text: v["views_text"].as_str().unwrap_or("").into(),
                    duration_text: format!("{}:{:02}", seconds / 60, seconds % 60).into(),
                    thumbnail: slint::Image::load_from_path(std::path::Path::new(v["thumbnail"].as_str().unwrap_or("")))?,
                    available: true, music_available: true, ..Default::default()
                })
            }).collect::<Result<Vec<_>, _>>()?;
            let selected = input["selected"].as_u64().unwrap_or(0) as usize;
            if let Some(row) = rows.get(selected) {
                w.set_selected_id(row.source_id.clone());
                w.set_selected_title(row.title.clone());
                w.set_selected_channel(row.channel.clone());
                w.set_selected_image(slint::Image::load_from_path(std::path::Path::new(input["cover"].as_str().unwrap()))?);
                w.set_duration(tracks[selected]["duration"].as_f64().unwrap_or(240.) as f32);
                w.set_position(70.);
                w.set_clock(format!("1:10 / {}", row.duration_text).into());
            }
            w.set_videos(ModelRc::new(VecModel::from(rows.clone())));
            w.set_rec_indices(ModelRc::new(VecModel::from((0..rows.len() as i32).collect::<Vec<_>>())));
            w.set_playlist_id("".into());
            w.set_save_to(0);
            let collection = input["collection"].as_str().unwrap_or("Recently played");
            w.set_history_view(collection == "Recently played");
            w.set_collection_title(collection.into());
            if let Some(playlists) = input["playlists"].as_array() {
                let playlists = playlists.iter().map(|v| PlaylistCard {
                    id: v["id"].as_str().unwrap_or("").into(),
                    name: v["name"].as_str().unwrap_or("").into(),
                    count: v["count"].as_str().unwrap_or("").into(),
                }).collect::<Vec<_>>();
                w.set_playlist_names(ModelRc::new(VecModel::from(playlists.iter().map(|v| v.name.clone()).collect::<Vec<_>>())));
                w.set_playlists(ModelRc::new(VecModel::from(playlists)));
            }
        }
    }
    #[cfg(feature = "showcase")]
    if let Ok(path) = std::env::var("MOCHUNE_SHOWCASE_THEME") {
        mochune_native::apply_showcase_theme(&w, std::path::Path::new(&path))?;
        w.set_music_extra_card(true);
        w.set_skin_card_visible(true);
    }
    if std::env::var_os("MOCHUNE_SHOWCASE_COMPACT").is_some() {
        w.window().set_size(slint::LogicalSize::new(380., 740.));
    }
    if std::env::var_os("MOCHUNE_SHOWCASE_MINI").is_some() {
        w.set_mini_music(true);
        w.window().set_size(slint::LogicalSize::new(420., 120.));
    }
    w.show()?;
    let weak = w.as_weak();
    slint::Timer::single_shot(
        Duration::from_millis(
            if std::env::var_os("MOCHUNE_SHOWCASE_LYRIC_STEP").is_some() {
                1400
            } else {
                700
            },
        ),
        move || {
            let w = weak.upgrade().expect("showcase window");
            let pixels = w.window().take_snapshot().expect("interface snapshot");
            image::save_buffer(
                &output,
                pixels.as_bytes(),
                pixels.width(),
                pixels.height(),
                image::ColorType::Rgba8,
            )
            .expect("write screenshot");
            slint::quit_event_loop().expect("quit showcase");
        },
    );
    if mode == "caption" && std::env::var_os("MOCHUNE_SHOWCASE_CC_MENU").is_some() {
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_show_captions();
            }
        });
    }
    if std::env::var_os("MOCHUNE_SHOWCASE_REC_MENU").is_some() {
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(w) = weak.upgrade() {
                w.set_rec_filter_open(1);
            }
        });
    }
    if std::env::var_os("MOCHUNE_SHOWCASE_HOVER_ART").is_some() {
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(w) = weak.upgrade() {
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                        position: slint::LogicalPosition::new(1080., 200.),
                    });
            }
        });
    }
    if std::env::var_os("MOCHUNE_SHOWCASE_LYRIC_STEP").is_some() {
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(600), move || {
            if let Some(w) = weak.upgrade() {
                w.set_lyric_previous(w.get_caption_text());
                w.set_caption_text(w.get_caption_next());
                w.set_caption_next(w.get_lyric_following());
                w.set_lyric_following("A new line arrives below".into());
                w.set_lyrics_animate(true);
                w.set_lyrics_frame(w.get_lyrics_frame() + 1);
            }
        });
    }
    slint::run_event_loop()?;
    Ok(())
}
