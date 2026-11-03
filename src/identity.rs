//! Current identity plus narrowly scoped compatibility with earlier releases.
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// RUST_EXTERNAL_ROOT is the external build drive. ~/.profile exports it to
/// every desktop session, so an installed release binary would otherwise
/// store data and resolve helpers on the development SSD. Only a launch
/// through run.sh (MOCHUNE_DEV_LAUNCH=1) or a direct debug build adopts it.
pub fn dev_external_root() -> Option<OsString> {
    adopt_external_root(
        std::env::var_os("MOCHUNE_DEV_LAUNCH").is_some(),
        cfg!(debug_assertions),
        std::env::var_os("RUST_EXTERNAL_ROOT"),
    )
}

fn adopt_external_root(
    dev_launch: bool,
    debug_assertions: bool,
    root: Option<OsString>,
) -> Option<OsString> {
    if !dev_launch && !debug_assertions {
        return None;
    }
    root
}

pub fn data_directory() -> PathBuf {
    if let Some(path) = std::env::var_os("MOCHUNE_DATA_DIR") {
        return path.into();
    }
    if let Some(root) = dev_external_root() {
        return PathBuf::from(root).join("data/Mochune");
    }
    directories::BaseDirs::new()
        .map(|d| d.data_local_dir().join("Mochune"))
        .unwrap_or_else(|| "Mochune".into())
}

fn legacy_directories() -> Vec<PathBuf> {
    if let Some(root) = dev_external_root() {
        let root = PathBuf::from(root).join("data");
        return vec![root.join("DuckTube"), root.join("ytlite")];
    }
    let mut paths = Vec::new();
    if let Some(base) = directories::BaseDirs::new() {
        paths.push(base.data_local_dir().join("DuckTube"));
    }
    if let Some(old) = directories::ProjectDirs::from("io", "monoma", "YTLite") {
        paths.push(old.data_local_dir().to_path_buf());
    }
    paths
}

fn move_profile(old: &Path, new: &Path) -> anyhow::Result<bool> {
    if new.exists() || !old.is_dir() {
        return Ok(false);
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(old, new)?;
    // Absolute custom-theme paths inside the moved profile must move with it.
    let settings = new.join("settings.json");
    if let Ok(bytes) = std::fs::read(&settings)
        && let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        fn rebase(value: &mut serde_json::Value, old: &Path, new: &Path) {
            match value {
                serde_json::Value::String(s) => {
                    if let Ok(relative) = Path::new(s).strip_prefix(old) {
                        *s = new.join(relative).to_string_lossy().into_owned();
                    }
                }
                serde_json::Value::Array(a) => a.iter_mut().for_each(|v| rebase(v, old, new)),
                serde_json::Value::Object(o) => o.values_mut().for_each(|v| rebase(v, old, new)),
                _ => {}
            }
        }
        rebase(&mut value, old, new);
        // Keep the original settings available if the rewritten file cannot be saved.
        let backup = new.join("settings.before-rename.json");
        std::fs::write(&backup, &bytes)?;
        std::fs::write(&settings, serde_json::to_vec_pretty(&value)?)?;
    }
    Ok(true)
}

/// Called once by main, before helpers, UI, logging or multimedia spawn threads.
pub fn initialize() -> anyhow::Result<()> {
    // Existing launchers and owner test scripts continue to work. New names win.
    let aliases: Vec<_> = std::env::vars_os()
        .filter_map(|(key, value)| {
            key.to_str()
                .and_then(|s| s.strip_prefix("DUCKTUBE_"))
                .map(|suffix| (format!("MOCHUNE_{suffix}"), value))
        })
        .collect();
    for (key, value) in aliases {
        if std::env::var_os(&key).is_none() {
            // SAFETY: initialize runs before any application threads are created.
            unsafe { std::env::set_var(key, value) };
        }
    }
    #[cfg(windows)]
    if std::env::var_os("MOCHUNE_DATA_DIR").is_none() {
        let executable = std::env::current_exe()?;
        if let Some(runtime) = executable.parent()
            && runtime.join("PORTABLE.txt").is_file()
        {
            let data = runtime.parent().unwrap_or(runtime).join("data");
            // SAFETY: initialization still precedes all application threads.
            unsafe { std::env::set_var("MOCHUNE_DATA_DIR", data) };
        }
    }
    let current = data_directory();
    if std::env::var_os("MOCHUNE_DATA_DIR").is_none() {
        for old in legacy_directories() {
            if move_profile(&old, &current)? {
                break;
            }
        }
    }
    normalize_settings(&current)?;
    Ok(())
}

fn normalize_settings(current: &Path) -> anyhow::Result<()> {
    let file = current.join("settings.json");
    let Ok(bytes) = std::fs::read(&file) else {
        return Ok(());
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(());
    };
    let original = value.clone();
    if let Some(path) = value["music_skin_path"].as_str() {
        let mut replacement = None;
        for old in legacy_directories() {
            if let Ok(relative) = Path::new(path).strip_prefix(old) {
                replacement = Some(current.join(relative));
                break;
            }
        }
        if replacement.is_none() {
            // Stock themes move with the installed prefix or source checkout.
            let renamed = path
                .replace("/share/ducktube/", "/share/mochune/")
                .replace("/DuckTube/", "/Mochune/")
                .replace("\\DuckTube\\", "\\Mochune\\")
                .replace("/projects/ytlite-native/", "/projects/mochune-native/");
            if renamed != path && Path::new(&renamed).is_file() {
                replacement = Some(renamed.into());
            }
        }
        if let Some(path) = replacement {
            value["music_skin_path"] = path.to_string_lossy().into_owned().into();
        }
    }
    if let Some(user) = directories::UserDirs::new() {
        let downloads = user
            .download_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| user.home_dir().join("Downloads"));
        let old = downloads.join("DuckTube");
        let new = downloads.join("Mochune");
        if value["download_folder"]
            .as_str()
            .is_some_and(|p| Path::new(p) == old)
        {
            if old.is_dir() && !new.exists() {
                std::fs::rename(&old, &new)?;
            }
            if !old.exists() {
                value["download_folder"] = new.to_string_lossy().into_owned().into();
            }
        }
    }
    if value != original {
        let backup = current.join("settings.before-rename.json");
        if !backup.exists() {
            std::fs::write(backup, bytes)?;
        }
        std::fs::write(file, serde_json::to_vec_pretty(&value)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_release_builds_ignore_the_external_build_root() {
        let root: Option<OsString> = Some(OsString::from("/media/ssd/Development/rust"));
        // Desktop-launched installed release binary: neither a dev launch
        // through run.sh nor a debug build. It must never adopt the SSD.
        assert!(adopt_external_root(false, false, root.clone()).is_none());
        // run.sh sets MOCHUNE_DEV_LAUNCH; a direct debug dev build has that.
        assert_eq!(adopt_external_root(true, false, root.clone()), root);
        assert_eq!(adopt_external_root(true, true, root.clone()), root);
        // Without any external root nothing is invented.
        assert!(adopt_external_root(true, true, None).is_none());
    }
    // Debug build: dev adoption is unconditional, matching run.sh launches.
    #[cfg(debug_assertions)]
    #[test]
    fn debug_dev_build_keeps_the_external_build_root() {
        if let Some(root) = std::env::var_os("RUST_EXTERNAL_ROOT") {
            assert_eq!(dev_external_root(), Some(root));
        }
    }
    // Release test binary: no MOCHUNE_DEV_LAUNCH and not a debug build, which
    // is exactly how a desktop-launched installed binary runs.
    #[cfg(not(debug_assertions))]
    #[test]
    fn installed_release_binary_ignores_the_desktop_exported_build_root() {
        assert!(dev_external_root().is_none());
        assert_eq!(
            data_directory(),
            directories::BaseDirs::new()
                .unwrap()
                .data_local_dir()
                .join("Mochune")
        );
    }
    #[test]
    fn migration_preserves_library_rebases_themes_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!("mochune-migration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join("previous");
        let new = root.join("current");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("playlists.json"), b"keep these likes").unwrap();
        std::fs::write(
            old.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "theme": old.join("themes/custom/skin.json"), "volume": 42
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(move_profile(&old, &new).unwrap());
        assert_eq!(
            std::fs::read(new.join("playlists.json")).unwrap(),
            b"keep these likes"
        );
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(new.join("settings.json")).unwrap()).unwrap();
        assert_eq!(
            settings["theme"],
            new.join("themes/custom/skin.json")
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(settings["volume"], 42);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("playlists.json"), b"different library").unwrap();
        assert!(!move_profile(&old, &new).unwrap());
        assert!(old.join("playlists.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
