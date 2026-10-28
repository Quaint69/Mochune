//! Bounded local event logs; never log stream URLs, headers, cookies or full metadata.
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
static LOG: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());
const LIMIT: u64 = 256 * 1024;
fn clean(value: &mut Value) {
    match value {
        Value::String(s) => {
            let lower = s.to_ascii_lowercase();
            if lower.contains("cookie") || lower.contains("authorization") {
                *s = "[redacted credentials]".into();
                return;
            }
            *s = s
                .split_whitespace()
                .map(|word| {
                    let lower = word.to_ascii_lowercase();
                    if lower.contains("http://")
                        || lower.contains("https://")
                        || lower.contains("cookie")
                        || lower.contains("authorization")
                    {
                        "[redacted]"
                    } else {
                        word
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(4096)
                .collect();
        }
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if ["cookie", "authorization", "header", "url", "token"]
                    .iter()
                    .any(|name| key.to_ascii_lowercase().contains(name))
                {
                    *value = json!("[redacted]");
                } else {
                    clean(value);
                }
            }
        }
        Value::Array(array) => {
            array.truncate(32);
            for v in array {
                clean(v);
            }
        }
        _ => (),
    }
}
fn append(path: &Path, event: &str, mut data: Value) -> std::io::Result<()> {
    clean(&mut data);
    if path.metadata().is_ok_and(|m| m.len() >= LIMIT) {
        let previous = path.with_file_name(format!(
            "{}-previous.jsonl",
            path.file_stem().unwrap_or_default().to_string_lossy()
        ));
        if previous.exists() {
            std::fs::remove_file(&previous)?;
        }
        std::fs::rename(path, previous)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let line = json!({"time_ms": time, "event": event, "data": data}).to_string();
    let line = if line.len() > 16384 {
        json!({"time_ms":time,"event":event,"data":"[oversized event omitted]"}).to_string()
    } else {
        line
    };
    writeln!(file, "{line}")
}
pub fn initialize(data: &Path) {
    let dir = data.join("logs");
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = LOG.set(dir.join(format!("{}.jsonl", std::env::consts::OS)));
    }
    record(
        "startup",
        json!({"version":env!("CARGO_PKG_VERSION"), "os":std::env::consts::OS, "arch":std::env::consts::ARCH, "pid":std::process::id(), "playback_revision":"bounded-ahead-rewind-preroll-v2", "runtime_id":std::env::var("MOCHUNE_RUNTIME_ID").ok()}),
    );
}
pub fn record(event: &str, data: Value) {
    let Some(path) = LOG.get() else {
        return;
    };
    let Ok(_guard) = LOCK.lock() else {
        return;
    };
    let _ = append(path, event, data);
}
#[cfg(test)]
mod tests {
    #[test]
    fn logs_rotate_and_redact_credentials_and_signed_urls() {
        let dir = std::env::temp_dir().join(format!("mochune-logs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("windows.jsonl");
        std::fs::write(&path, vec![b'x'; super::LIMIT as usize]).unwrap();
        super::append(&path,"error",serde_json::json!({"debug":"failed https://example.com/private?sig=SECRET codec", "headers":{"Cookie":"SECRET"}})).unwrap();
        assert!(dir.join("windows-previous.jsonl").exists());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("SECRET"));
        assert!(!text.contains("example.com"));
        assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Native process counters, sampled with health events rather than per frame.
#[cfg(target_os = "windows")]
pub fn process_memory() -> Value {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
        Threading::GetCurrentProcess,
    };
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    // SAFETY: current-process pseudo handle, correct EX buffer size and lifetime.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            counters.cb,
        )
    };
    if ok == 0 {
        return Value::Null;
    }
    json!({"working_set_bytes":counters.WorkingSetSize,"private_bytes":counters.PrivateUsage,"peak_working_set_bytes":counters.PeakWorkingSetSize})
}
