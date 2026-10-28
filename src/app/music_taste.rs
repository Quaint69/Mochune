//! Small metadata-only taste profile. No audio analysis, extra requests or persistent model.
use super::{Video, recommendations::music_hint};
use serde_json::Value;
const STYLES: &[&[&str]] = &[
    &["rock", "j rock", "jrock", "ロック"],
    &["metal", "metalcore", "deathcore"],
    &["pop", "j pop", "jpop", "city pop"],
    &["hip hop", "hiphop", "rap", "trap"],
    &[
        "electronic",
        "edm",
        "house",
        "techno",
        "dubstep",
        "drum and bass",
        "dnb",
    ],
    &["phonk", "funk automotivo"],
    &["jazz", "ジャズ"],
    &["classical", "orchestra", "symphony", "concerto"],
    &["country", "bluegrass", "folk"],
    &["rnb", "r b", "soul"],
    &["reggae", "reggaeton"],
    &[
        "anime",
        "アニメ",
        "soundtrack",
        "ost",
        "ゲーム音楽",
        "game music",
        "opening theme",
        "ending theme",
    ],
    &["k pop", "kpop"],
    &["opm", "filipino", "tagalog"],
    &["latin", "latino", "bossa nova", "samba"],
    &["japanese", "jpop", "jrock", "j pop", "j rock"],
];
fn normalized(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
fn mask(text: &str) -> u32 {
    // Kana is a language/scene hint, not a claimed musical genre.
    let japanese = text
        .chars()
        .any(|c| matches!(c as u32, 0x3040..=0x30ff | 0xff66..=0xff9d));
    let text = normalized(text);
    STYLES
        .iter()
        .enumerate()
        .fold(if japanese { 1 << 15 } else { 0 }, |bits, (i, names)| {
            let matched = names.iter().any(|name| {
                text.match_indices(name).any(|(start, _)| {
                    (start == 0 || text.as_bytes()[start - 1] == b' ')
                        && (start + name.len() == text.len()
                            || text.as_bytes()[start + name.len()] == b' ')
                })
            });
            if matched { bits | 1 << i } else { bits }
        })
}
pub(super) fn metadata_genres(info: &Value) -> Vec<String> {
    // Preserve only small supplied genre fields, never a video's potentially huge tag list.
    info["genres"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .chain(info["genre"].as_str())
        .take(8)
        .map(|v| v.chars().take(64).collect())
        .collect()
}
fn styles(video: &Video) -> u32 {
    let explicit = video
        .genres
        .iter()
        .fold(0, |bits, value| bits | mask(value));
    if explicit != 0 {
        explicit
    } else {
        mask(&video.title)
    }
}
#[derive(Default)]
pub(super) struct Taste {
    preferred: u32,
}
impl Taste {
    pub(super) fn from_song(song: &Video) -> Self {
        Self { preferred: styles(song) }
    }
    pub(super) fn from_history(history: &[Video]) -> Self {
        let mut weights = [0u32; STYLES.len()];
        let mut support = [0u8; STYLES.len()];
        for (rank, video) in history
            .iter()
            .filter(|v| music_hint(v))
            .take(20)
            .enumerate()
        {
            let tags = styles(video);
            for i in 0..STYLES.len() {
                if tags & (1 << i) != 0 {
                    weights[i] += (20 - rank) as u32;
                    support[i] += 1;
                }
            }
        }
        let mut ranked: Vec<_> = (0..STYLES.len()).filter(|&i| support[i] >= 2).collect();
        ranked.sort_by_key(|&i| std::cmp::Reverse(weights[i]));
        let strongest = ranked.first().map_or(0, |&i| weights[i]);
        let preferred = ranked
            .into_iter()
            .filter(|&i| weights[i] * 3 >= strongest)
            .take(3)
            .fold(0, |bits, i| bits | 1 << i);
        Self { preferred }
    }
    pub(super) fn filter(&self, videos: &mut Vec<Video>) {
        if self.preferred == 0 {
            return;
        }
        // Unknown genres stay eligible; sparse metadata must not empty the feed.
        videos.retain(|v| {
            let tags = styles(v);
            tags == 0 || tags & self.preferred != 0
        });
        videos.sort_by_cached_key(|v| styles(v) & self.preferred == 0);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn song(title: &str) -> Video {
        Video {
            title: title.into(),
            music: Some(true),
            ..Default::default()
        }
    }
    #[test]
    fn taste_uses_recent_twenty_and_keeps_top_three_supported_styles() {
        let mut history = vec![
            song("Anime OST"),
            song("Anime opening theme"),
            song("J-pop"),
            song("City pop"),
            song("Rock"),
            song("Rock"),
        ];
        history.extend((6..20).map(|_| song("Unknown")));
        history.extend([song("Phonk"), song("Phonk"), song("Phonk")]);
        let taste = Taste::from_history(&history);
        assert_eq!(taste.preferred.count_ones(), 3);
        let mut candidates = vec![
            song("Phonk remix"),
            song("Unknown song"),
            song("Anime OST"),
            song("Jazz"),
        ];
        taste.filter(&mut candidates);
        assert_eq!(
            candidates
                .iter()
                .map(|v| v.title.as_str())
                .collect::<Vec<_>>(),
            ["Anime OST", "Unknown song"]
        );
    }
    #[test]
    fn sparse_history_and_unknown_genres_are_not_guessed() {
        let taste = Taste::from_history(&[song("Rock")]);
        let mut candidates = vec![song("Jazz"), song("Tokimeki Poporon")];
        taste.filter(&mut candidates);
        assert_eq!(candidates.len(), 2);
        assert_eq!(styles(&candidates[1]), 0); // not "pop" because of a substring
    }
    #[test]
    fn explicit_genre_takes_precedence_over_title_and_metadata_is_bounded() {
        let genres = metadata_genres(&serde_json::json!({"genres":["Rock"],"tags":["jazz"]}));
        let v = Video {
            title: "Pop".into(),
            genres,
            ..Default::default()
        };
        assert_eq!(styles(&v), 1);
    }
}
