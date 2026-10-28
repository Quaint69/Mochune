//! Diagnose empty upstream searches; retain only bounded, metadata-only local matches.
use super::*;
pub(super) fn local_matches(
    query: &str,
    history: &[Video],
    selected: Option<&Video>,
) -> Vec<Video> {
    let query = query.to_lowercase();
    let words: Vec<_> = query.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    selected
        .into_iter()
        .chain(history.iter())
        .filter(|video| {
            let title = video.title.to_lowercase();
            !video.id.is_empty()
                && words.iter().all(|word| title.contains(word))
                && seen.insert(video.id.clone())
        })
        .take(30)
        .cloned()
        .collect()
}
fn message(node: &Value, depth: usize) -> Option<&'static str> {
    if depth > 40 {
        return None;
    }
    if let Some(promo) = node
        .get("backgroundPromoRenderer")
        .or_else(|| node.get("messageRenderer"))
    {
        let text = promo.to_string().to_lowercase();
        if text.contains("confirm your age") || text.contains("age-restricted") {
            return Some(
                "YouTube requires age confirmation for this search. Try different keywords or open YouTube in your browser.",
            );
        }
        if text.contains("sign in") || text.contains("sign-in") {
            return Some(
                "YouTube asks you to sign in for this search. Open YouTube in your browser.",
            );
        }
    }
    match node {
        Value::Object(map) => map.values().find_map(|value| message(value, depth + 1)),
        Value::Array(items) => items.iter().find_map(|value| message(value, depth + 1)),
        _ => None,
    }
}
fn page_reason(page: &str) -> Option<&'static str> {
    let data = recommendations::json_after(page, "var ytInitialData = ")
        .or_else(|| recommendations::json_after(page, "ytInitialData = "))?;
    message(
        data.pointer("/contents/twoColumnSearchResultsRenderer/primaryContents")?,
        0,
    )
}
pub(super) fn empty_reason(query: &str) -> Option<String> {
    let mut url = url::Url::parse("https://www.youtube.com/results").ok()?;
    url.query_pairs_mut()
        .append_pair("search_query", query)
        .append_pair("hl", "en");
    let mut response = metadata_agent()
        .get(url.as_str())
        .header("Accept-Language", "en-US,en;q=0.8")
        .call()
        .ok()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()
        .ok()?;
    page_reason(std::str::from_utf8(&bytes).ok()?).map(str::to_owned)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinguishes_a_search_gate_from_unrelated_page_chrome() {
        let gate = serde_json::json!({"contents":{"twoColumnSearchResultsRenderer":{"primaryContents":{"backgroundPromoRenderer":{"title":{"runs":[{"text":"Confirm your age"}]}}}}}});
        assert!(
            page_reason(&format!("var ytInitialData = {gate};"))
                .unwrap()
                .contains("age confirmation")
        );
        let unrelated = serde_json::json!({"contents":{"twoColumnSearchResultsRenderer":{"primaryContents":{}}},"dialog":{"backgroundPromoRenderer":{"title":"Confirm your age"}}});
        assert!(page_reason(&format!("var ytInitialData = {unrelated};")).is_none());
    }
    #[test]
    fn local_search_preserves_current_then_history_order_without_duplicates() {
        let first = Video {
            id: "abcdefghijk".into(),
            title: "Tokimeki Poporon".into(),
            ..Video::default()
        };
        let second = Video {
            id: "lmnopqrstuv".into(),
            title: "Tokimeki Poporon Remix".into(),
            ..Video::default()
        };
        let unrelated = Video {
            id: "xxxxxxxxxxx".into(),
            title: "Other song".into(),
            ..Video::default()
        };
        let rows = local_matches(
            "TOKIMEKI poporon",
            &[first.clone(), second, unrelated],
            Some(&first),
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, first.id);
        assert!(local_matches("   ", &[first], None).is_empty());
    }
}
