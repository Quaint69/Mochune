//! Runtime evidence, independent of the user's hardware preference or UI renderer.
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};
#[derive(Default)]
pub(super) struct Evidence {
    decoder: Mutex<Option<(String, bool, String)>>,
    pub decoded: AtomicU64,
    pub presented: AtomicU64,
}
#[derive(serde::Serialize)]
pub(crate) struct Report {
    pub verified: bool,
    pub status: String,
    pub details: String,
}
impl Evidence {
    pub fn reset(&self) {
        *self.decoder.lock().unwrap() = None;
        self.decoded.store(0, Ordering::Relaxed);
        self.presented.store(0, Ordering::Relaxed);
    }
    pub fn record_decoder(&self, name: &str, hardware: bool, memory: &str) {
        *self.decoder.lock().unwrap() = Some((name.into(), hardware, memory.into()));
    }
    pub fn report(&self, active: bool, music: bool, paused: bool, input_memory: &str) -> Report {
        if !active || music {
            return Report {
                verified: false,
                status: if active && music {
                    "Audio only — video decoder idle"
                } else {
                    "Not in use — play a video to verify"
                }
                .into(),
                details: String::new(),
            };
        }
        let decoded = self.decoded.load(Ordering::Relaxed);
        let shown = self.presented.load(Ordering::Relaxed);
        let decoder = self.decoder.lock().unwrap();
        let Some((name, hardware, memory)) = decoder.as_ref().filter(|_| decoded > 0) else {
            return Report {
                verified: false,
                status: "Waiting for actual decoded frames…".into(),
                details: String::new(),
            };
        };
        let verified = *hardware && shown > 0;
        let status = if verified {
            if paused {
                "Paused — hardware decoding verified"
            } else {
                "Hardware decoding in use — verified frames"
            }
        } else if *hardware {
            "Hardware decoded frames — waiting for UI presentation"
        } else {
            "Software video decoding — GPU rendering alone is not hardware decoding"
        };
        let mut details = format!(
            "Decoder: {name}\nDecoded frames: {decoded} · Displayed frames: {shown}\nDecoder memory: {memory} · Display input: {input_memory}"
        );
        #[cfg(target_os = "windows")]
        {
            let stats = super::gpu_bridge_stats();
            if super::native_windows_enabled() {
                details.push_str("\nNative D3D12 swapchain · desktop controls · no UI bitmap IPC");
                if verified && memory.contains("D3D12") {
                    details.push_str("\nDecoder → D3D12 sink · no CPU video readback");
                }
            } else {
                details.push_str(&format!(
                    "\nGPU copies completed / submitted (total): {} / {}",
                    stats["completed"], stats["submitted"]
                ));
            }
        }
        #[cfg(not(target_os = "windows"))]
        details
            .push_str("\nGPU presentation does not by itself prove copy-free decoder transport.");
        Report {
            verified,
            status: status.into(),
            details,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preference_or_decoder_name_alone_does_not_verify_acceleration() {
        let e = Evidence::default();
        e.record_decoder("d3d12h264dec", true, "D3D12Memory");
        assert!(!e.report(true, false, false, "memory:D3D12Memory").verified);
        e.decoded.store(2, Ordering::Relaxed);
        assert!(!e.report(true, false, false, "memory:D3D12Memory").verified);
        e.presented.store(1, Ordering::Relaxed);
        assert!(e.report(true, false, false, "memory:D3D12Memory").verified);
        assert!(!e.report(false, false, false, "memory:D3D12Memory").verified);
        assert!(!e.report(true, true, false, "memory:D3D12Memory").verified);
        e.reset();
        assert!(!e.report(true, false, false, "memory:D3D12Memory").verified);
    }
    #[test]
    fn gpu_rendering_does_not_disguise_software_decode() {
        let e = Evidence::default();
        e.record_decoder("avdec_h264", false, "SystemMemory");
        e.decoded.store(100, Ordering::Relaxed);
        e.presented.store(99, Ordering::Relaxed);
        let report = e.report(true, false, false, "memory:GLMemory");
        assert!(!report.verified);
        assert!(report.status.starts_with("Software"));
    }
}
