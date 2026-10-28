//! About content is bundled; external links are a fixed project-owned allowlist.
use super::*;
const KOFI_URL: Option<&str> = Some("https://ko-fi.com/duckeytubey");
fn project_link(index: i32) -> Option<&'static str> {
    match index {
        0 => Some("https://github.com/Quaint69/Mochune"),
        1 => Some(
            "https://github.com/Quaint69/Mochune/blob/main/third-party/RUST-LICENSE-INVENTORY.md",
        ),
        2 => Some("https://www.paypal.com/ncp/payment/Y3ND4S8E29RS4"),
        3 => KOFI_URL,
        _ => None,
    }
}
pub(super) fn install(w: &SetupWindow) {
    w.set_about_version(concat!("Version ", env!("CARGO_PKG_VERSION")).into());
    w.set_kofi_available(KOFI_URL.is_some());
    let weak = w.as_weak();
    w.on_open_themes_folder(move || {
        let weak = weak.clone();
        thread::spawn(move || {
            let folder = crate::music_skin::themes_folder();
            if !folder.is_dir() {
                ui(&weak, |w| w.invoke_notify("Themes folder is unavailable.".into()));
                return;
            }
            #[cfg(target_os = "linux")]
            let mut command = Command::new("xdg-open");
            #[cfg(target_os = "windows")]
            let mut command = {
                use std::os::windows::process::CommandExt;
                let mut command = Command::new("explorer.exe");
                command.creation_flags(0x08000000);
                command
            };
            let result = command.arg(folder).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
            // Explorer may return 1 after handing the request to an existing window.
            #[cfg(target_os = "windows")]
            let opened = result.is_ok_and(|s| s.success() || s.code() == Some(1));
            #[cfg(target_os = "linux")]
            let opened = result.is_ok_and(|s| s.success());
            if !opened {
                ui(&weak, |w| w.invoke_notify("Could not open the themes folder.".into()));
            }
        });
    });
    let weak = w.as_weak();
    w.on_show_license(move |index| {
        if let Some(w) = weak.upgrade() {
            w.set_about_license_text(
                match index {
                    1 => include_str!("../../LICENSE"),
                    2 => include_str!("../../NOTICE"),
                    _ => "",
                }
                .into(),
            );
        }
    });
    let weak = w.as_weak();
    w.on_about_link(move |index| {
        let Some(url) = project_link(index) else {
            return;
        };
        let weak = weak.clone();
        thread::spawn(move || {
            #[cfg(target_os = "linux")]
            let mut command = Command::new("xdg-open");
            #[cfg(target_os = "windows")]
            let mut command = {
                use std::os::windows::process::CommandExt;
                let mut command = Command::new("rundll32.exe");
                command
                    .args(["url.dll,FileProtocolHandler"])
                    .creation_flags(0x08000000);
                command
            };
            if !command
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
            {
                ui(&weak, |w| {
                    w.invoke_notify(
                        "Could not open your default browser.".into(),
                    )
                });
            }
        });
    });
}
