//! A bounded, on-demand catalog. Never retain decoded artwork while scanning.
use super::*;

#[derive(Clone)]
pub(super) struct Entry {
    pub name: String,
    pub path: PathBuf,
}

fn installed_root(executable: &Path) -> Option<PathBuf> {
    let directory = executable.parent()?;
    // The Windows portable launcher replaces runtime on updates, but keeps
    // editable themes beside it, just like the persistent data directory.
    // Only the explicit portable marker permits looking above the executable.
    if directory.join("PORTABLE.txt").is_file() {
        return Some(directory.parent()?.join("themes"));
    }
    if directory.join("INSTALLED.txt").is_file() {
        return directories::BaseDirs::new().map(|d| d.data_local_dir().join("Mochune/themes"));
    }
    if directory.file_name().is_some_and(|name| name == "bin") {
        let shared = directory.parent()?.join("share/mochune/themes");
        if shared.is_dir() {
            return Some(shared);
        }
    }
    Some(directory.join("themes"))
}

pub(super) fn root() -> PathBuf {
    if cfg!(test) {
        return Path::new(env!("CARGO_MANIFEST_DIR")).join("themes");
    }
    let adjacent = std::env::current_exe()
        .ok()
        .and_then(|p| installed_root(&p));
    if let Some(path) = adjacent {
        if path.is_dir() || !cfg!(debug_assertions) {
            return path;
        }
    }
    if !cfg!(debug_assertions) {
        // Fail closed when the executable location cannot be determined.
        return PathBuf::new();
    }
    // Development builds live on the external build drive, not in the repo.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("themes")
}

pub(super) fn confined(path: &Path, root: &Path) -> Result<PathBuf> {
    ensure!(!root.as_os_str().is_empty(), "Themes folder unavailable");
    let root = root
        .canonicalize()
        .with_context(|| format!("Themes folder unavailable: {}", root.display()))?;
    ensure!(
        path.file_name().is_some_and(|n| n == "skin.json"),
        "Theme entry point must be skin.json"
    );
    let folder = path.parent().context("Theme directory missing")?;
    ensure!(
        !std::fs::symlink_metadata(folder)?.file_type().is_symlink(),
        "Linked theme folders are not allowed"
    );
    ensure!(
        !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
        "Linked theme entry points are not allowed"
    );
    let path = path.canonicalize().context("Theme file unavailable")?;
    ensure!(
        path.starts_with(&root),
        "Themes must be inside {}",
        root.display()
    );
    ensure!(
        path.file_name().is_some_and(|n| n == "skin.json")
            && path.parent().and_then(Path::parent) == Some(root.as_path()),
        "Select a theme at themes/<folder>/skin.json"
    );
    Ok(path)
}

pub(super) fn scan(root: &Path, stop: &AtomicBool) -> Result<Vec<Entry>> {
    ensure!(!root.as_os_str().is_empty(), "Themes folder unavailable");
    let root = root
        .canonicalize()
        .with_context(|| format!("Themes folder unavailable: {}", root.display()))?;
    let mut entries = Vec::new();
    let mut inspected = 0;
    for item in std::fs::read_dir(&root)? {
        if stop.load(Ordering::Relaxed) {
            bail!("Theme scan canceled");
        }
        inspected += 1;
        ensure!(
            inspected <= 256,
            "Themes folder exceeds 256 entries; reduce its contents and rescan"
        );
        let Ok(item) = item else { continue };
        // No linked theme folders or alternate JSON entry points.
        if !item.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(path) = confined(&item.path().join("skin.json"), &root) else {
            continue;
        };
        if entries.iter().any(|entry: &Entry| entry.path == path) {
            continue;
        }
        // Same parsers, paths, resource budgets and first-frame decode as apply.
        // Drop all decoded data before moving to the next candidate.
        if let Ok((Some(c), _, _, _)) = load_unconfined(&Source::File(path.clone())) {
            if c.font_warnings.is_empty() {
                entries.push(Entry { name: c.name, path });
            }
        }
    }
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.path.cmp(&b.path))
    });
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_install_uses_executable_adjacent_themes() {
        let dir = super::super::tests::Temp::new();
        let runtime = dir.0.join("runtime");
        std::fs::create_dir(&runtime).unwrap();
        // A parent themes directory alone must not widen the search root.
        std::fs::create_dir(dir.0.join("themes")).unwrap();
        assert_eq!(
            installed_root(&runtime.join("mochune.exe")),
            Some(runtime.join("themes"))
        );
    }

    #[test]
    fn installed_themes_use_user_data_and_debian_shared_data() {
        let dir = super::super::tests::Temp::new();
        let runtime = dir.0.join("programs");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::write(runtime.join("INSTALLED.txt"), b"installed").unwrap();
        assert_eq!(
            installed_root(&runtime.join("mochune.exe")),
            directories::BaseDirs::new().map(|d| d.data_local_dir().join("Mochune/themes"))
        );
        let bin = dir.0.join("usr/bin");
        let shared = dir.0.join("usr/share/mochune/themes");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        assert_eq!(installed_root(&bin.join("mochune")), Some(shared));
    }
    #[test]
    fn portable_install_uses_persistent_themes_not_runtime() {
        let dir = super::super::tests::Temp::new();
        let runtime = dir.0.join("runtime");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::write(runtime.join("PORTABLE.txt"), b"portable").unwrap();
        let expected = dir.0.join("themes");
        // Select the correct location even before extraction and even when an
        // obsolete executable-adjacent themes directory is left behind.
        assert_eq!(
            installed_root(&runtime.join("mochune.exe")),
            Some(expected.clone())
        );
        std::fs::create_dir(runtime.join("themes")).unwrap();
        std::fs::create_dir(&expected).unwrap();
        assert_eq!(installed_root(&runtime.join("mochune.exe")), Some(expected));
    }

    #[test]
    fn missing_scan_root_reports_the_expected_path() {
        let dir = super::super::tests::Temp::new();
        let missing = dir.0.join("missing-themes");
        let error = scan(&missing, &AtomicBool::new(false)).err().unwrap();
        assert!(error.to_string().contains(&missing.display().to_string()));
    }

    #[test]
    fn confinement_requires_a_direct_theme_folder() {
        let dir = super::super::tests::Temp::new();
        let folder = dir.0.join("valid");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("skin.json"), b"{}").unwrap();
        std::fs::write(folder.join("other.json"), b"{}").unwrap();
        assert!(confined(&folder.join("skin.json"), &dir.0).is_ok());
        assert!(confined(&folder.join("other.json"), &dir.0).is_err());
        assert!(confined(&folder.join("skin.json"), &folder).is_err());
        assert!(confined(&folder.join("skin.json"), &dir.0.join("missing")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_entry_points_and_folders_are_rejected() {
        let dir = super::super::tests::Temp::new();
        let folder = dir.0.join("valid");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("skin.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(&folder, dir.0.join("linked")).unwrap();
        assert!(confined(&dir.0.join("linked/skin.json"), &dir.0).is_err());
        std::fs::create_dir(dir.0.join("other")).unwrap();
        std::os::unix::fs::symlink(folder.join("skin.json"), dir.0.join("other/skin.json"))
            .unwrap();
        assert!(confined(&dir.0.join("other/skin.json"), &dir.0).is_err());
    }

    #[test]
    fn scan_excludes_invalid_and_broken_font_themes() {
        let dir = super::super::tests::Temp::new();
        for name in ["valid", "invalid", "font-missing"] {
            std::fs::create_dir(dir.0.join(name)).unwrap();
        }
        let mut json: serde_json::Value =
            serde_json::from_slice(embedded(1, "skin.json").unwrap()).unwrap();
        json["assets"] = serde_json::json!({});
        json["sections"] = serde_json::json!({});
        json["theme_assets"] = serde_json::json!({});
        json["fonts"] = serde_json::json!([]);
        json["style"]["font_family"] = "DejaVu Sans".into();
        std::fs::write(
            dir.0.join("valid/skin.json"),
            serde_json::to_vec(&json).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.0.join("invalid/skin.json"), b"{}").unwrap();
        json["fonts"] = serde_json::json!([{"family":"User Font","file":"missing.ttf","license":"LICENSE.txt"}]);
        std::fs::write(
            dir.0.join("font-missing/skin.json"),
            serde_json::to_vec(&json).unwrap(),
        )
        .unwrap();
        let entries = scan(&dir.0, &AtomicBool::new(false)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, dir.0.join("valid/skin.json"));
    }
}
