use super::*;
pub(super) struct Details {
    subscribers: String,
    channel_url: String,
    ratings: String,
    stats: String,
    description: String,
}
fn count(info: &Value, key: &str) -> String {
    info[key]
        .as_u64()
        .map(|n| view_count(n).trim_end_matches(" views").to_string())
        .unwrap_or_else(|| "Unavailable".into())
}
impl Details {
    pub fn from_info(info: &Value) -> Self {
        let date = info["upload_date"]
            .as_str()
            .filter(|s| s.len() == 8 && s.bytes().all(|c| c.is_ascii_digit()))
            .map(|s| format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..]));
        Self {
            channel_url: info["channel_url"]
                .as_str()
                .and_then(super::pages::channel_url)
                .unwrap_or_default(),
            subscribers: if info["channel_follower_count"].as_u64().is_some() {
                format!("{} subscribers", count(info, "channel_follower_count"))
            } else {
                "Subscribers unavailable".into()
            },
            // Dislike display is intentionally disabled until a dependable source exists.
            // ratings: format!("Likes: {} · Dislikes: {}", count(info, "like_count"), count(info, "dislike_count")),
            ratings: format!("Likes: {}", count(info, "like_count")),
            stats: [info["view_count"].as_u64().map(view_count), date]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
            description: info["description"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(16_384)
                .collect(),
        }
    }
    pub fn apply(self, w: &SetupWindow) {
        w.set_selected_channel_url(self.channel_url.into());
        w.set_subscriber_text(self.subscribers.into());
        w.set_rating_text(self.ratings.into());
        w.set_video_stats(self.stats.into());
        w.set_description(self.description.into());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_counts_are_not_zero_and_description_is_bounded() {
        let d = Details::from_info(
            &serde_json::json!({"like_count":0,"description":"界".repeat(20_000),"upload_date":"20261016"}),
        );
        assert!(d.ratings.contains("Likes: 0"));
        assert!(!d.ratings.contains("Dislikes"));
        assert_eq!(d.description.chars().count(), 16_384);
        assert!(d.stats.contains("2026-10-16"));
    }
}
