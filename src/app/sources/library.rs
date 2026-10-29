pub(super) use super::library_data::{Library, SavedTitle};
use super::*;
impl Library {
    pub fn load() -> Self {
        let p = crate::identity::data_directory();
        [
            p.join("sources-library.json"),
            p.join("sources-library.json.bak"),
        ]
        .into_iter()
        .find_map(|p| {
            std::fs::read(p)
                .ok()
                .filter(|b| b.len() <= 2 * 1024 * 1024)
                .and_then(|b| serde_json::from_slice::<Self>(&b).ok())
        })
        .map(|mut l| {
            l.titles.truncate(500);
            l
        })
        .unwrap_or_default()
    }
}

pub(super) fn library_rows(w: &SetupWindow, b: &Browser) {
    let g = sources(w);
    let titles = b
        .library
        .visible(g.get_mode(), &b.folder)
        .map(|s| SourceRow {
            title: s.entry.title.clone().into(),
            detail: format!(
                "{}{}",
                match s.provider {
                    0 =>
                        if s.mode == 1 {
                            "Anikoto"
                        } else if s.mode == 2 {
                            "Dramachi"
                        } else {
                            "MangaDex"
                        },
                    _ =>
                        if s.mode == 2 {
                            "VixSrc"
                        } else {
                            "MangaPill"
                        },
                },
                if s.total > 0 {
                    format!(" · page {} / {}", s.page + 1, s.total)
                } else {
                    String::new()
                }
            )
            .into(),
            active: b.parent.as_ref().is_some_and(|e| e.id == s.entry.id)
                && g.get_provider() == s.provider,
        })
        .collect::<Vec<_>>();
    let folders = std::iter::once(slint::SharedString::from("All bookmarks"))
        .chain(std::iter::once(slint::SharedString::from("Ungrouped")))
        .chain(
            b.library
                .folders
                .iter()
                .filter(|f| f.mode == g.get_mode())
                .map(|f| f.name.clone().into()),
        )
        .collect::<Vec<_>>();
    g.set_move_folder_names(ModelRc::new(VecModel::from(
        folders.iter().skip(1).cloned().collect::<Vec<_>>(),
    )));
    g.set_folder_names(ModelRc::new(VecModel::from(folders)));
    g.set_folder_index(if b.folder == "*" {
        0
    } else if b.folder.is_empty() {
        1
    } else {
        b.library
            .folders
            .iter()
            .filter(|f| f.mode == g.get_mode())
            .position(|f| f.id == b.folder)
            .map_or(0, |i| i as i32 + 2)
    });
    if let Some(model) = g
        .get_bookmarks()
        .as_any()
        .downcast_ref::<VecModel<SourceRow>>()
    {
        model.set_vec(titles);
    } else {
        g.set_bookmarks(ModelRc::new(VecModel::from(titles)));
    }
}
pub(super) fn remember(w: &SetupWindow, b: &mut Browser, jobs: &Jobs, toggle: bool) {
    let g = sources(w);
    let entry = b
        .parent
        .clone()
        .or_else(|| b.current.and_then(|i| b.results.get(i).cloned()));
    if let Some(e) = entry {
        let chapter = if g.get_mode() == 3 {
            b.current.and_then(|i| b.children.get(i).cloned())
        } else {
            None
        };
        let was_saved = b.library.titles.iter().any(|s| {
            s.mode == g.get_mode()
                && s.provider == g.get_provider()
                && s.entry.id == e.id
                && s.bookmarked
        });
        b.library.record(
            g.get_mode(),
            g.get_provider(),
            e,
            chapter,
            b.page,
            b.pages.len(),
            toggle,
        );
        if toggle && !was_saved && b.folder != "*" {
            if let Some(s) = b.library.titles.first_mut() {
                s.folder = b.folder.clone();
            }
        }
        if let Ok(bytes) = serde_json::to_vec(&b.library) {
            let pending = jobs.pending.lock().unwrap();
            *jobs.save.lock().unwrap() = Some(bytes);
            drop(pending);
            jobs.wake.notify_one();
        } else {
            g.set_status("Could not save source bookmarks/progress.".into());
        }
        let active = b
            .parent
            .as_ref()
            .or_else(|| b.current.and_then(|i| b.results.get(i)));
        g.set_bookmarked(active.is_some_and(|e| {
            b.library.titles.iter().any(|s| {
                s.mode == g.get_mode()
                    && s.provider == g.get_provider()
                    && s.entry.id == e.id
                    && s.bookmarked
            })
        }));
        library_rows(w, b);
    }
}
