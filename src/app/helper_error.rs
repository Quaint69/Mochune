//! Keep helper failures readable even when Windows writes a legacy code page.
pub(super) fn message(code: Option<i32>, stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    if text.contains("Sign in to confirm") && text.contains("not a bot") {
        return "YouTube is asking you to sign in to confirm you are not a bot. Try again later or open the video in your browser.".into();
    }
    if let Some(line) = text.lines().find(|s| s.contains("ERROR:"))
        .or_else(|| text.lines().rev().find(|s| !s.trim().is_empty())) {
        return line.trim().chars().take(320).collect();
    }
    match code {
        Some(code) => format!("YouTube helper failed without an error message (exit code {code}, 0x{:08X}). Please retry.", code as u32),
        None => "YouTube helper stopped without an error message. Please retry.".into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_legacy_apostrophe_does_not_hide_sign_in_challenge() {
        let bytes = b"ERROR: [youtube] example: Sign in to confirm you\x92re not a bot. Use --cookies.\n";
        assert!(message(Some(1), bytes).contains("not a bot"));
        assert!(!message(Some(1), bytes).contains("--cookies"));
    }
    #[test]
    fn preserves_other_errors_and_reports_silent_native_exit() {
        assert_eq!(message(Some(1), b"WARNING: note\nERROR: Video is unavailable\n"), "ERROR: Video is unavailable");
        assert!(message(Some(0xc0000005u32 as i32), b"").contains("0xC0000005"));
        assert!(message(Some(1), b"fatal helper problem\n").contains("fatal helper problem"));
    }
}
