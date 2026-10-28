//! Disk image cache is explicitly opt-in; current on-screen images are not retained here.
use super::*;
use std::sync::atomic::AtomicBool;
#[derive(Default)]
pub(super) struct Policy {
    enabled: AtomicBool,
    epoch: AtomicU64,
    disk: Mutex<()>,
}
impl Policy {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    pub fn set(self: &Arc<Self>, enabled: bool, path: PathBuf) {
        self.enabled.store(enabled, Ordering::Release);
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        if !enabled {
            let policy = self.clone();
            thread::spawn(move || {
                let _lock = policy.disk.lock().unwrap();
                if !policy.enabled() && policy.epoch() == epoch {
                    let _ = fs::remove_dir_all(path);
                }
            });
        }
    }
    pub fn clear(&self, path: &Path) -> Result<()> {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        let _lock = self.disk.lock().unwrap();
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn read(&self, path: &Path) -> Option<Vec<u8>> {
        let _lock = self.disk.lock().unwrap();
        if !self.enabled() {
            return None;
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .ok()?
            .take(1024 * 1024)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    }
    pub fn write(&self, path: &Path, bytes: &[u8], epoch: u64) {
        let _lock = self.disk.lock().unwrap();
        if !self.enabled() || self.epoch() != epoch {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(path, bytes);
    }
    #[cfg(test)]
    fn clear_now(&self, path: &Path) {
        let _lock = self.disk.lock().unwrap();
        if !self.enabled() {
            let _ = fs::remove_dir_all(path);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opt_in_and_disabling_reject_late_writes() {
        let root =
            std::env::temp_dir().join(format!("mochune-cache-policy-{}", std::process::id()));
        let path = root.join("image.jpg");
        let p = Arc::new(Policy::default());
        p.write(&path, b"not retained", p.epoch());
        assert!(!path.exists());
        p.set(true, root.clone());
        let old = p.epoch();
        p.write(&path, b"image", old);
        assert_eq!(p.read(&path).unwrap(), b"image");
        p.set(false, root.clone());
        p.clear_now(&root);
        p.write(&path, b"late", old);
        assert!(!path.exists());
        assert!(p.read(&path).is_none());
        p.set(true, root.clone());
        p.write(&path, b"stale", old);
        assert!(!path.exists());
        p.write(&path, b"fresh", p.epoch());
        assert!(path.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
