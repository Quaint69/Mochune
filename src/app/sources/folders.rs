use super::*;
fn id(b: &Browser, mode: i32, index: i32) -> String {
    if index <= 0 {
        "*".into()
    } else if index == 1 {
        String::new()
    } else {
        b.library
            .folders
            .iter()
            .filter(|f| f.mode == mode)
            .nth((index - 2) as usize)
            .map(|f| f.id.clone())
            .unwrap_or_else(|| "*".into())
    }
}
fn save(w: &SetupWindow, b: &Browser, j: &Jobs) {
    if let Ok(bytes) = serde_json::to_vec(&b.library) {
        let pending = j.pending.lock().unwrap();
        *j.save.lock().unwrap() = Some(bytes);
        drop(pending);
        j.wake.notify_one();
    }
    library_rows(w, b);
}
pub(super) fn install(w: &SetupWindow, b: Arc<Mutex<Browser>>, j: Arc<Jobs>) {
    let weak = w.as_weak();
    let browser = b.clone();
    w.global::<Sources>().on_select_folder(move |index| {
        if let Some(w) = weak.upgrade() {
            let mut b = browser.lock().unwrap();
            b.folder = id(&b, sources(&w).get_mode(), index);
            library_rows(&w, &b);
        }
    });
    let weak = w.as_weak();
    let browser = b.clone();
    let jobs = j.clone();
    w.global::<Sources>().on_save_folder(move || {
        if let Some(w) = weak.upgrade() {
            let g = sources(&w);
            let mut b = browser.lock().unwrap();
            let result = if g.get_folder_rename() {
                let folder = b.folder.clone();
                b.library
                    .rename_folder(g.get_mode(), &folder, g.get_folder_name().as_str())
                    .map(|_| folder)
            } else {
                b.library
                    .create_folder(g.get_mode(), g.get_folder_name().as_str())
            };
            match result {
                Ok(id) => {
                    b.folder = id;
                    g.set_folder_editor(false);
                    save(&w, &b, &jobs);
                }
                Err(e) => g.set_folder_error(e.to_string().into()),
            }
        }
    });
    let weak = w.as_weak();
    let browser = b.clone();
    let jobs = j.clone();
    w.global::<Sources>().on_delete_folder(move || {
        if let Some(w) = weak.upgrade() {
            let g = sources(&w);
            let mut b = browser.lock().unwrap();
            if b.folder != "*" && !b.folder.is_empty() {
                let folder = b.folder.clone();
                b.library.delete_folder(g.get_mode(), &folder);
                b.folder = String::new();
                save(&w, &b, &jobs);
                g.set_status("Folder removed. Its bookmarks are in Ungrouped.".into());
            }
        }
    });
    let weak = w.as_weak();
    let browser = b.clone();
    let jobs = j.clone();
    w.global::<Sources>()
        .on_move_bookmark(move |index, destination| {
            if let Some(w) = weak.upgrade() {
                let g = sources(&w);
                let mut b = browser.lock().unwrap();
                let key = b
                    .library
                    .visible(g.get_mode(), &b.folder)
                    .nth(index.max(0) as usize)
                    .map(|s| (s.provider, s.entry.id.clone()));
                let folder = id(&b, g.get_mode(), destination);
                if folder == "*" {
                    return;
                }
                if let Some((provider, key)) = key {
                    if let Some(s) = b.library.titles.iter_mut().find(|s| {
                        s.mode == g.get_mode() && s.provider == provider && s.entry.id == key
                    }) {
                        s.folder = folder;
                    }
                    save(&w, &b, &jobs);
                }
            }
        });
    let weak = w.as_weak();
    w.global::<Sources>().on_remove_bookmark(move |index| {
        if let Some(w) = weak.upgrade() {
            let g = sources(&w);
            let mut b = b.lock().unwrap();
            let key = b
                .library
                .visible(g.get_mode(), &b.folder)
                .nth(index.max(0) as usize)
                .map(|s| (s.provider, s.entry.id.clone()));
            if let Some((provider, key)) = key {
                if let Some(s) =
                    b.library.titles.iter_mut().find(|s| {
                        s.mode == g.get_mode() && s.provider == provider && s.entry.id == key
                    })
                {
                    s.bookmarked = false;
                }
                save(&w, &b, &j);
                rows(&w, &b);
            }
        }
    });
}
