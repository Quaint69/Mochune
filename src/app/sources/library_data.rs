use super::providers::Entry;
use serde::{Deserialize, Serialize};
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct SavedTitle {
    pub mode: i32,
    pub provider: i32,
    pub entry: Entry,
    pub chapter: Option<Entry>,
    pub page: usize,
    pub total: usize,
    pub bookmarked: bool,
    #[serde(default)]
    pub folder: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Folder {
    pub id: String,
    pub mode: i32,
    pub name: String,
}
#[derive(Default, Serialize, Deserialize)]
pub(super) struct Library {
    #[serde(default)]
    pub folders: Vec<Folder>,
    pub titles: Vec<SavedTitle>,
}

impl Library {
    pub fn create_folder(&mut self, mode: i32, name: &str) -> anyhow::Result<String> {
        let name = name.trim();
        anyhow::ensure!(
            !name.is_empty() && name.chars().count() <= 80,
            "Use a folder name of 1–80 characters"
        );
        anyhow::ensure!(self.folders.len() < 100, "Folder limit reached (100)");
        anyhow::ensure!(
            !self
                .folders
                .iter()
                .any(|f| f.mode == mode && f.name.to_lowercase() == name.to_lowercase()),
            "That folder already exists"
        );
        let seq = self
            .folders
            .iter()
            .filter_map(|f| f.id.rsplit('-').next()?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let id = format!("folder-{mode}-{seq}");
        self.folders.push(Folder {
            id: id.clone(),
            mode,
            name: name.into(),
        });
        Ok(id)
    }
    pub fn rename_folder(&mut self, mode: i32, id: &str, name: &str) -> anyhow::Result<()> {
        let name = name.trim();
        anyhow::ensure!(
            !name.is_empty() && name.chars().count() <= 80,
            "Use a folder name of 1–80 characters"
        );
        anyhow::ensure!(
            !self.folders.iter().any(|f| f.mode == mode
                && f.id != id
                && f.name.to_lowercase() == name.to_lowercase()),
            "That folder already exists"
        );
        let f = self
            .folders
            .iter_mut()
            .find(|f| f.mode == mode && f.id == id)
            .ok_or_else(|| anyhow::anyhow!("Select a custom folder first"))?;
        f.name = name.into();
        Ok(())
    }
    pub fn delete_folder(&mut self, mode: i32, id: &str) {
        self.folders.retain(|f| !(f.mode == mode && f.id == id));
        for s in &mut self.titles {
            if s.mode == mode && s.folder == id {
                s.folder.clear();
            }
        }
    }
    pub fn visible<'a>(
        &'a self,
        mode: i32,
        folder: &'a str,
    ) -> impl Iterator<Item = &'a SavedTitle> {
        self.titles.iter().filter(move |s| {
            s.mode == mode && s.bookmarked && (folder == "*" || s.folder == folder)
        })
    }
    pub fn record(
        &mut self,
        mode: i32,
        provider: i32,
        mut entry: Entry,
        mut chapter: Option<Entry>,
        page: usize,
        total: usize,
        toggle: bool,
    ) {
        fn compact(e: &mut Entry) {
            e.description.clear();
            e.title = e.title.chars().take(256).collect();
            e.detail = e.detail.chars().take(256).collect();
        }
        compact(&mut entry);
        if let Some(chapter) = chapter.as_mut() {
            compact(chapter);
        }
        let existing = self
            .titles
            .iter()
            .position(|s| s.mode == mode && s.provider == provider && s.entry.id == entry.id);
        let mut saved = existing
            .map(|i| self.titles.remove(i))
            .unwrap_or_else(|| SavedTitle {
                mode,
                provider,
                entry: entry.clone(),
                ..Default::default()
            });
        if toggle {
            saved.bookmarked = !saved.bookmarked;
        } else {
            saved.chapter = chapter;
            saved.page = page;
            saved.total = total;
        }
        let position = if toggle { 0 } else { existing.unwrap_or(0) };
        self.titles.insert(position.min(self.titles.len()), saved);
        while self.titles.len() > 500 {
            if let Some(i) = self.titles.iter().rposition(|s| !s.bookmarked) {
                self.titles.remove(i);
            } else {
                self.titles.truncate(500);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bookmark_toggling_preserves_read_progress_and_provider_identity() {
        let mut l = Library::default();
        let e = Entry {
            id: "same".into(),
            title: "A title".into(),
            ..Default::default()
        };
        let ch = Entry {
            id: "ch2".into(),
            ..Default::default()
        };
        l.record(3, 0, e.clone(), Some(ch.clone()), 7, 24, false);
        l.record(3, 0, e.clone(), None, 0, 0, true);
        assert!(l.titles[0].bookmarked);
        assert_eq!(l.titles[0].page, 7);
        assert_eq!(l.titles[0].chapter, Some(ch));
        l.record(3, 1, e.clone(), None, 0, 0, true);
        assert_eq!(l.titles.len(), 2);
        l.record(3, 0, e, None, 0, 0, true);
        assert!(!l.titles[0].bookmarked);
        assert_eq!(l.titles[0].page, 7);
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;
    #[test]
    fn folder_delete_moves_bookmarks_without_erasing_progress() {
        let mut l = Library::default();
        let id = l.create_folder(3, "Reading").unwrap();
        l.record(
            3,
            0,
            Entry {
                id: "a".into(),
                ..Default::default()
            },
            None,
            8,
            20,
            true,
        );
        let entry = l.titles[0].entry.clone();
        l.record(3, 0, entry, None, 8, 20, false);
        l.titles[0].folder = id.clone();
        assert_eq!(l.visible(3, &id).count(), 1);
        assert!(l.create_folder(3, "reading").is_err());
        l.rename_folder(3, &id, "Finished").unwrap();
        l.delete_folder(3, &id);
        assert_eq!(l.visible(3, "").count(), 1);
        assert_eq!(l.titles[0].page, 8);
    }
    #[test]
    fn older_profiles_deserialize_without_folders() {
        let l: Library = serde_json::from_str(r#"{"titles":[]}"#).unwrap();
        assert!(l.folders.is_empty());
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;
    #[test]
    fn reading_progress_does_not_reorder_bookmarks_under_an_open_menu() {
        let mut l = Library::default();
        let a = Entry {
            id: "a".into(),
            ..Default::default()
        };
        let b = Entry {
            id: "b".into(),
            ..Default::default()
        };
        l.record(3, 0, a.clone(), None, 0, 20, true);
        l.record(3, 0, b, None, 0, 20, true);
        l.record(3, 0, a, None, 8, 20, false);
        assert_eq!(
            l.visible(3, "*")
                .map(|s| s.entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert_eq!(l.titles[1].page, 8);
    }
}
