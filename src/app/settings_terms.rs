//! Small, bounded editors for recommendation terms. Stored settings stay comma-separated.
use super::*;
fn terms(value: &str) -> Vec<String> {
    let mut entries = Vec::new();
    // Match the settings field's existing limit; don't silently shorten individual names.
    let bounded: String = value.chars().take(4096).collect();
    for term in bounded
        .split([',', '\n'])
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let term = term.to_owned();
        if !entries
            .iter()
            .any(|v: &String| v.to_lowercase() == term.to_lowercase())
        {
            entries.push(term);
        }
    }
    entries
}
fn edit(value: &str, input: &str, remove: bool) -> String {
    let mut entries = terms(value);
    if remove {
        entries.retain(|v| v.to_lowercase() != input.trim().to_lowercase());
    } else {
        entries.extend(terms(input));
    }
    terms(&entries.join(", ")).join(", ")
}
pub(super) fn install(w: &SetupWindow) {
    w.on_split_terms(|value| {
        ModelRc::new(VecModel::from(
            terms(&value)
                .into_iter()
                .map(Into::into)
                .collect::<Vec<slint::SharedString>>(),
        ))
    });
    w.on_edit_terms(|value, input, remove| edit(&value, &input, remove).into());
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editors_add_remove_deduplicate_and_bound_terms() {
        assert_eq!(
            edit("Science, Jazz", "jazz, Animation\nSpace", false),
            "Science, Jazz, Animation, Space"
        );
        assert_eq!(edit("Science, Jazz", "science", true), "Jazz");
        assert_eq!(edit("", "  , \n", false), "");
        assert_eq!(terms(&"x".repeat(5000))[0].chars().count(), 4096);
        assert_eq!(
            terms(
                &(0..100)
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
            .len(),
            100
        );
    }
}
