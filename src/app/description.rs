//! Bounded description links; only an explicit click launches the system browser.
use super::*;
fn links(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.split_whitespace()
        .filter_map(|token| {
            let start = token.find("https://").or_else(|| token.find("http://"))?;
            let token = token[start..].trim_end_matches(['.', ',', ';', '!', '"', '\'', '>', ']']);
            let token = if token.ends_with(')')
                && token.matches(')').count() > token.matches('(').count()
            {
                token.trim_end_matches(')')
            } else {
                token
            };
            if token.len() > 2048 {
                return None;
            }
            let url = url::Url::parse(token).ok()?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return None;
            }
            let url = url.to_string();
            seen.insert(url.clone()).then_some(url)
        })
        .take(32)
        .collect()
}
pub(super) fn install(w: &SetupWindow) {
    let weak = w.as_weak();
    w.on_update_description_links(move || {
        if let Some(w) = weak.upgrade() {
            w.set_description_links(ModelRc::new(VecModel::from(
                links(w.get_description().as_str())
                    .into_iter()
                    .map(slint::SharedString::from)
                    .collect::<Vec<_>>(),
            )));
        }
    });
    let weak = w.as_weak();
    w.on_open_description_link(move |index| {
        let Some(w) = weak.upgrade() else {
            return;
        };
        let Some(link) = w.get_description_links().row_data(index as usize) else {
            return;
        };
        // Revalidate against the description rather than accepting arbitrary UI input.
        if !links(w.get_description().as_str())
            .iter()
            .any(|v| v == link.as_str())
        {
            return;
        }
        let weak = weak.clone();
        thread::spawn(move || {
            #[cfg(target_os = "linux")]
            let mut command = Command::new("xdg-open");
            #[cfg(target_os = "windows")]
            let mut command = {
                use std::os::windows::process::CommandExt;
                let mut c = Command::new("rundll32.exe");
                c.args(["url.dll,FileProtocolHandler"])
                    .creation_flags(0x08000000);
                c
            };
            if !command
                .arg(link.as_str())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
            {
                ui(&weak, |w| {
                    w.invoke_notify("Could not open the link in your browser.".into())
                });
            }
        });
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_are_deduplicated_bounded_web_urls_only() {
        assert_eq!(
            links(
                "Watch https://example.com/a. Again https://example.com/a and (https://example.org/b). javascript:alert(1) file:///tmp/a https://user:pass@example.net/"
            ),
            vec!["https://example.com/a", "https://example.org/b"]
        );
        assert_eq!(
            links(
                &(0..100)
                    .map(|i| format!("https://example.com/{i} "))
                    .collect::<String>()
            )
            .len(),
            32
        );
        assert!(links(&format!("https://example.com/{}", "a".repeat(3000))).is_empty());
    }
}
