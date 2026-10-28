//! Bounded theme-local fonts. Registration is UI-thread-only and process-lived.
use super::*;
pub(super) const FILE_LIMIT: usize = 2 * 1024 * 1024;
const THEME_LIMIT: usize = 4 * 1024 * 1024;
const SESSION_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Font {
    pub family: String,
    pub file: String,
    pub license: String,
}
#[derive(Clone)]
pub(super) struct Prepared {
    family: String,
    bytes: Arc<[u8]>,
}
thread_local! {
    // Slint exposes no unregister operation. Bound unique registrations for the
    // entire UI thread; identical bytes on reload do not register again.
    static REGISTERED: RefCell<Vec<(&'static [u8], bool)>> = const { RefCell::new(Vec::new()) };
}
pub(super) fn safe_path(file: &str) -> Result<()> {
    ensure!(
        !file.is_empty()
            && file.len() <= 240
            && !file.contains(['\\', ':'])
            && Path::new(file)
                .components()
                .all(|p| matches!(p, Component::Normal(_))),
        "Font and license paths must be relative files inside the theme folder"
    );
    Ok(())
}
fn inside(source: &Source, file: &str) -> Result<PathBuf> {
    safe_path(file)?;
    let Source::File(path) = source else {
        bail!("Local fonts require an external theme")
    };
    let root = path
        .parent()
        .context("Theme directory missing")?
        .canonicalize()?;
    // Check existing ancestors too: a missing file behind an escaping symlink
    // must not silently turn a confinement violation into a fallback warning.
    let mut candidate = root.clone();
    for part in Path::new(file).components() {
        candidate.push(part);
        if std::fs::symlink_metadata(&candidate).is_ok() {
            candidate = candidate.canonicalize()?;
            ensure!(
                candidate.starts_with(&root),
                "Font or license escapes the theme directory"
            );
        }
    }
    Ok(candidate)
}
pub(super) fn declarations(fonts: &[Font]) -> Result<()> {
    ensure!(fonts.len() <= 4, "At most four local font faces per theme");
    let mut files = std::collections::BTreeSet::new();
    for font in fonts {
        ensure!(
            !font.family.trim().is_empty()
                && font.family.len() <= 80
                && !font.family.chars().any(char::is_control),
            "Invalid local font family"
        );
        ensure!(
            !crate::music_theme::FONT_FAMILIES.contains(&font.family.as_str()),
            "Local fonts cannot replace built-in font families"
        );
        safe_path(&font.file)?;
        safe_path(&font.license)?;
        ensure!(files.insert(&font.file), "Duplicate local font file");
        ensure!(
            matches!(
                Path::new(&font.file).extension().and_then(|v| v.to_str()),
                Some("ttf" | "otf")
            ),
            "Local fonts must be .ttf or .otf files"
        );
    }
    Ok(())
}
pub(super) fn prepare(c: &mut Config, source: &Source) -> Result<()> {
    let mut total = 0usize;
    for font in &c.fonts {
        let path = inside(source, &font.file)?;
        let license = inside(source, &font.license)?;
        let result = (|| -> Result<Prepared> {
            let notice = limited_read(&license, 64 * 1024)?;
            ensure!(
                !std::str::from_utf8(&notice)?.trim().is_empty(),
                "Empty font license notice"
            );
            let bytes: Arc<[u8]> = limited_read(&path, FILE_LIMIT)?.into();
            total += bytes.len();
            ensure!(total <= THEME_LIMIT, "Theme fonts exceed 4 MiB");
            ensure!(
                bytes.starts_with(&[0, 1, 0, 0]) || bytes.starts_with(b"OTTO"),
                "Use a single-face TTF or OTF font"
            );
            let face = ttf_parser::Face::parse(&bytes, 0)
                .map_err(|_| anyhow::anyhow!("Invalid font data"))?;
            ensure!(
                face.names().into_iter().any(|name| {
                    matches!(
                        name.name_id,
                        ttf_parser::name_id::FAMILY | ttf_parser::name_id::TYPOGRAPHIC_FAMILY
                    ) && name.to_string().as_deref() == Some(font.family.as_str())
                }),
                "Declared font family does not match the font's internal family"
            );
            Ok(Prepared {
                family: font.family.clone(),
                bytes,
            })
        })();
        match result {
            Ok(prepared) => c.prepared_fonts.push(prepared),
            Err(error) => c.font_warnings.push(format!(
                "{}: {error}; using fallback if unavailable",
                font.file
            )),
        }
    }
    Ok(())
}
#[cfg(any(target_os="windows", feature="native-overlay-test"))]
pub(super) fn registered_bytes() -> Vec<&'static [u8]> {
    REGISTERED.with(|cache| cache.borrow().iter().filter_map(|(bytes,ok)| ok.then_some(*bytes)).collect())
}
pub(super) fn register(w: &SetupWindow, c: &mut Config) {
    use slint::private_unstable_api::re_exports::WindowInner;
    let adapter = WindowInner::from_pub(w.window()).window_adapter();
    let mut available = std::collections::BTreeSet::new();
    for font in c.prepared_fonts.drain(..) {
        let result = REGISTERED.with(|cache| -> Result<()> {
            let mut cache = cache.borrow_mut();
            if let Some((_, ok)) = cache
                .iter()
                .find(|(bytes, _)| *bytes == font.bytes.as_ref())
            {
                ensure!(*ok, "Renderer previously rejected this font");
                return Ok(());
            }
            ensure!(
                cache.len() < 16
                    && cache.iter().map(|(b, _)| b.len()).sum::<usize>() + font.bytes.len()
                        <= SESSION_LIMIT,
                "Session font budget reached; restart to load more fonts"
            );
            // Slint requires static memory. Retain only these validated bytes,
            // never reopen a user-controlled path on the UI thread. Failed
            // registrations count toward the same lifetime cap as successes.
            let bytes: &'static [u8] = Box::leak(font.bytes.to_vec().into_boxed_slice());
            let result = adapter
                .renderer()
                .register_font_from_memory(bytes)
                .map_err(|e| anyhow::anyhow!("Font registration failed: {e}"));
            cache.push((bytes, result.is_ok()));
            result
        });
        match result {
            Ok(()) => {
                available.insert(font.family);
            }
            Err(error) => c.font_warnings.push(format!("{}: {error}", font.family)),
        }
    }
    let fallback = |family: &mut String| {
        if !crate::music_theme::FONT_FAMILIES.contains(&family.as_str())
            && !available.contains(family)
        {
            *family = "DejaVu Sans".into();
        }
    };
    fallback(&mut c.style.font_family);
    for section in c.sections.values_mut() {
        if let Some(f) = &mut section.defaults.font_family {
            fallback(f);
        }
        for component in section.components.values_mut() {
            if let Some(f) = &mut component.paint.font_family {
                fallback(f);
            }
            for paint in component.states.values_mut() {
                if let Some(f) = &mut paint.font_family {
                    fallback(f);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_reject_absolute_parent_and_windows_forms() {
        for path in [
            "",
            "/font.ttf",
            "../font.ttf",
            "fonts/../../font.ttf",
            "C:/font.ttf",
            "fonts\\font.ttf",
        ] {
            assert!(safe_path(path).is_err(), "{path}");
        }
        assert!(safe_path("fonts/custom.ttf").is_ok());
    }

    #[test]
    fn local_faces_and_licenses_are_prepared_without_registration() {
        for name in ["pixelduck"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("themes")
                .join(name)
                .join("skin.json");
            let (c, _, _, _) = load_unconfined(&Source::File(path)).unwrap();
            let c = c.unwrap();
            assert_eq!(c.prepared_fonts.len(), 2);
            assert!(c.font_warnings.is_empty(), "{:?}", c.font_warnings);
        }
    }

    #[test]
    fn missing_files_warn_and_unsafe_declarations_fail() {
        let mut c = config(embedded(1, "skin.json").unwrap()).unwrap();
        c.fonts = vec![Font {
            family: "User Font".into(),
            file: "fonts/missing.ttf".into(),
            license: "fonts/LICENSE.txt".into(),
        }];
        declarations(&c.fonts).unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("themes/pixelduck/skin.json");
        prepare(&mut c, &Source::File(path)).unwrap();
        assert_eq!(c.font_warnings.len(), 1);
        assert!(c.prepared_fonts.is_empty());
        c.fonts[0].file = "../outside.ttf".into();
        assert!(declarations(&c.fonts).is_err());
    }

    #[test]
    fn malformed_oversized_and_mismatched_fonts_fall_back() {
        let dir = super::super::tests::Temp::new();
        std::fs::write(dir.0.join("LICENSE.txt"), "Permission notice").unwrap();
        for bytes in [
            b"not a font".to_vec(),
            vec![0; FILE_LIMIT + 1],
            include_bytes!("../../ui/fonts/DuckPixel.otf").to_vec(),
        ] {
            let mut c = config(embedded(1, "skin.json").unwrap()).unwrap();
            c.fonts = vec![Font {
                family: "Wrong Family".into(),
                file: "custom.otf".into(),
                license: "LICENSE.txt".into(),
            }];
            std::fs::write(dir.0.join("custom.otf"), bytes).unwrap();
            prepare(&mut c, &Source::File(dir.0.join("skin.json"))).unwrap();
            assert_eq!(c.font_warnings.len(), 1);
            assert!(c.prepared_fonts.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn escaping_symlinks_are_rejected_even_when_target_file_is_missing() {
        let dir = super::super::tests::Temp::new();
        std::os::unix::fs::symlink(dir.0.parent().unwrap(), dir.0.join("escape")).unwrap();
        assert!(inside(&Source::File(dir.0.join("skin.json")), "escape/missing.ttf").is_err());
    }
}
