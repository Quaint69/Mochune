//! Reusable Deno code cache. Contains runtime/compiler data, never media or stream metadata.
//! Resolver access is serialized by HelperPermit; downloads keep their private scratch cache.
use std::{
    fs,
    path::{Path, PathBuf},
};
const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_NODES: usize = 4096;

pub(super) struct Cache {
    pub path: PathBuf,
    pub reused: bool,
    scratch: bool,
}
impl Cache {
    pub fn scratch(path: PathBuf) -> std::io::Result<Self> {
        fs::create_dir(&path)?;
        Ok(Self {
            path,
            reused: false,
            scratch: true,
        })
    }
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        // Refuse symlinks rather than traversing or clearing a caller-owned target.
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(std::io::Error::other(
                "Runtime cache directory cannot be a symbolic link",
            ));
        }
        let reused = path.is_dir() && !over_limit(&path)?;
        if path.exists() && !reused {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        Ok(Self {
            path,
            reused,
            scratch: false,
        })
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        // The helper has exited before this guard. Do not accumulate compiler caches
        // across runtime versions indefinitely; no maintenance thread or resident helper.
        if self.scratch {
            let _ = fs::remove_dir_all(&self.path);
            return;
        }
        if over_limit(&self.path).unwrap_or(false) {
            let cleared = fs::remove_dir_all(&self.path).is_ok();
            crate::diagnostics::record(
                "runtime_cache_limit",
                serde_json::json!({
                    "cleared":cleared,"limit_bytes":MAX_BYTES,"limit_entries":MAX_NODES
                }),
            );
        }
    }
}
fn over_limit(root: &Path) -> std::io::Result<bool> {
    fn visit(
        path: &Path,
        depth: usize,
        bytes: &mut u64,
        nodes: &mut usize,
    ) -> std::io::Result<bool> {
        if depth > 12 {
            return Ok(true);
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            *nodes += 1;
            if *nodes > MAX_NODES {
                return Ok(true);
            }
            let meta = fs::symlink_metadata(entry.path())?;
            if meta.file_type().is_symlink() {
                return Ok(true);
            }
            if meta.is_dir() {
                if visit(&entry.path(), depth + 1, bytes, nodes)? {
                    return Ok(true);
                }
            } else {
                *bytes = bytes.saturating_add(meta.len());
                if *bytes > MAX_BYTES {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    visit(root, 0, &mut 0, &mut 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_cache_is_reused_and_oversized_cache_is_pruned_without_touching_media() {
        let root = std::env::temp_dir().join(format!("mochune-deno-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("deno");
        let cache = Cache::open(path.clone()).unwrap();
        assert!(!cache.reused);
        fs::write(path.join("compiled"), b"code").unwrap();
        drop(cache);
        let cache = Cache::open(path.clone()).unwrap();
        assert!(cache.reused);
        fs::write(root.join("music.opus"), b"preserve").unwrap();
        fs::File::create(path.join("oversize"))
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        drop(cache);
        assert!(!path.exists());
        assert!(root.join("music.opus").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[cfg(unix)]
    fn code_cache_never_follows_symlinks() {
        let root =
            std::env::temp_dir().join(format!("mochune-deno-link-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(root.join("target"), root.join("link")).unwrap();
        assert!(Cache::open(root.join("link")).is_err());
        let cache = Cache::open(root.join("cache")).unwrap();
        std::os::unix::fs::symlink(root.join("target"), cache.path.join("link")).unwrap();
        drop(cache);
        assert!(root.join("target/keep").exists());
        fs::remove_dir_all(root).unwrap();
    }
}

pub(super) fn process_memory(child: &std::process::Child) -> serde_json::Value {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
        };
        let mut c: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
        c.cb = std::mem::size_of_val(&c) as u32;
        if unsafe {
            K32GetProcessMemoryInfo(
                child.as_raw_handle(),
                (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
                c.cb,
            )
        } != 0
        {
            return serde_json::json!({"rss_bytes":c.WorkingSetSize,"private_bytes":c.PrivateUsage,"peak_rss_bytes":c.PeakWorkingSetSize});
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = fs::read_to_string(format!("/proc/{}/status", child.id())) {
            let bytes = |key: &str| {
                status
                    .lines()
                    .find(|l| l.starts_with(key))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
                    .map(|k| k * 1024)
            };
            return serde_json::json!({"rss_bytes":bytes("VmRSS:"),"peak_rss_bytes":bytes("VmHWM:")});
        }
    }
    serde_json::Value::Null
}
