//! Private, bounded stdin/stdout controls/status protocol. Artwork decoding is
//! retained for wire compatibility; current UI composition stays in the parent.
//! Decoded audio/video stays in the playback process.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{Read, Write};
fn default_true()->bool{true}
pub const MAX_CONTROL: usize = 64 * 1024;
pub const MAX_ARTWORK: usize = 4_194_304 * 4;
#[derive(Serialize, Deserialize, Debug)]
pub enum Request {
    Load {
        epoch: u64,
        uri: String,
        audio: Option<String>,
        hardware: bool,
        volume: f64,
        start_position: f64,
        paused: bool,
        headers: Vec<(String, String)>,
        audio_headers: Vec<(String, String)>,
        #[serde(default)]
        buffer_hints: super::buffering::Hints,
        #[serde(default)]
        audio_effects: crate::audio_effects::Config,
        #[serde(default)]
        interpolation_mode:i32,
        #[serde(default="default_true")]
        interpolation_auto_fallback:bool,
    },
    Stop,
    Seek {
        seconds: f64,
        paused: bool,
        revision: u64,
    },
    Pause(bool),
    Volume(f64),
    Artwork {
        version: u64,
        canvas: [u32; 2],
        rect: [u32; 4],
        present: bool,
    },
    Shutdown,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Event {
    Ready,
    Started {
        epoch: u64,
    },
    State {
        epoch: u64,
        playing: bool,
    },
    Eos {
        epoch: u64,
    },
    Buffering {
        epoch: u64,
        percent: i32,
    },
    Error {
        epoch: u64,
        message: String,
        debug: String,
        element: String,
    },
    Status(Status),
    Input(Input),
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Status {
    pub epoch: u64,
    pub revision: u64,
    pub position: Option<f64>,
    pub duration: Option<f64>,
    #[serde(default)]
    pub buffered_ranges: Vec<(f64, f64)>,
    #[serde(default)]
    pub buffered_estimated: bool,
    #[serde(default)]
    pub buffered_confirmed: Vec<(f64, f64)>,
    #[serde(default)]
    pub rebuffering: bool,
    #[serde(default)]
    pub recovery_event: Option<serde_json::Value>,
    pub paused: bool,
    pub seeking: bool,
    pub rendered: u64,
    pub has_frame: bool,
    pub pts: f64,
    pub decoder: String,
    pub memory: String,
    pub verified: bool,
    pub acceleration_status: String,
    pub acceleration_details: String,
    pub process_memory: serde_json::Value,
    #[serde(default)]
    pub seek_details: serde_json::Value,
    #[serde(default)]
    pub interpolation:serde_json::Value,
}
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum Input {
    Move(i32, i32),
    Down(i32, i32),
    Up(i32, i32),
    Leave,
    Blur,
    Wheel(i16),
    Key(u32, bool),
}
pub fn write_packet<T: Serialize>(w: &mut impl Write, value: &T, bytes: &[u8]) -> Result<()> {
    let json = serde_json::to_vec(value)?;
    ensure!(
        json.len() <= MAX_CONTROL && bytes.len() <= MAX_ARTWORK,
        "IPC packet exceeds limit"
    );
    w.write_all(&(json.len() as u32).to_le_bytes())?;
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(&json)?;
    w.write_all(bytes)?;
    w.flush()?;
    Ok(())
}
pub fn read_packet<T: DeserializeOwned>(r: &mut impl Read) -> Result<(T, Vec<u8>)> {
    let mut head = [0u8; 8];
    r.read_exact(&mut head)
        .context("Playback process disconnected")?;
    let json = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
    let raw = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
    ensure!(
        json > 0 && json <= MAX_CONTROL && raw <= MAX_ARTWORK,
        "Invalid IPC packet size"
    );
    let mut body = vec![0; json];
    r.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body)?;
    let mut bytes = vec![0; raw];
    r.read_exact(&mut bytes)?;
    Ok((value, bytes))
}
pub fn valid_artwork(canvas: [u32; 2], rect: [u32; 4], len: usize) -> bool {
    let [x, y, w, h] = rect;
    w > 0
        && h > 0
        && x.checked_add(w).is_some_and(|v| v <= canvas[0])
        && y.checked_add(h).is_some_and(|v| v <= canvas[1])
        && u64::from(w)
            .checked_mul(u64::from(h))
            .and_then(|n| n.checked_mul(4))
            .is_some_and(|n| n == len as u64)
        && len <= MAX_ARTWORK
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quality_reload_carries_position_and_pause_intent() {
        let request = Request::Load {
            epoch: 42,
            uri: "https://example.org/video".into(),
            audio: None,
            hardware: true,
            volume: 0.7,
            start_position: 137.25,
            paused: false,
            headers: vec![],
            audio_headers: vec![],
            buffer_hints: Default::default(),
            audio_effects: Default::default(),
            interpolation_mode:0,
            interpolation_auto_fallback:true,
        };
        let mut bytes = Vec::new();
        write_packet(&mut bytes, &request, &[]).unwrap();
        let (request, _) = read_packet::<Request>(&mut bytes.as_slice()).unwrap();
        assert!(matches!(
            request,
            Request::Load {
                epoch: 42,
                start_position: 137.25,
                paused: false,
                ..
            }
        ));
        bytes.clear();
        write_packet(&mut bytes, &Event::Started { epoch: 42 }, &[]).unwrap();
        assert!(matches!(
            read_packet::<Event>(&mut bytes.as_slice()).unwrap().0,
            Event::Started { epoch: 42 }
        ));
    }
    #[test]
    fn protocol_roundtrip_and_bounded_allocations() {
        let mut buf = Vec::new();
        write_packet(&mut buf, &Request::Volume(0.7), &[]).unwrap();
        assert!(
            matches!(read_packet::<Request>(&mut buf.as_slice()).unwrap().0,Request::Volume(v) if v==0.7)
        );
        assert!(read_packet::<Request>(&mut [255u8; 8].as_slice()).is_err());
        assert!(!valid_artwork([100, 100], [0, 0, u32::MAX, u32::MAX], 16));
        assert!(!valid_artwork([100, 100], [90, 0, 20, 1], 80));
        assert!(!valid_artwork(
            [u32::MAX, u32::MAX],
            [0, 0, u32::MAX, u32::MAX],
            16
        ));
        assert!(valid_artwork([100, 100], [0, 0, 10, 10], 400));
    }
    #[test]
    fn rejects_truncated_and_malformed_packets() {
        assert!(read_packet::<Request>(&mut [1u8; 4].as_slice()).is_err());
        let mut b = Vec::new();
        write_packet(&mut b, &Event::Ready, &[]).unwrap();
        assert!(read_packet::<Request>(&mut b.as_slice()).is_err());
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    #[test]
    fn worker_error_preserves_origin_and_negotiation_detail() {
        let event = Event::Error {
            epoch: 7,
            message: "Internal data stream error.".into(),
            debug: "streaming stopped, reason not-negotiated (-4)".into(),
            element: "/pipeline/decoder".into(),
        };
        let mut bytes = Vec::new();
        write_packet(&mut bytes, &event, &[]).unwrap();
        let (event, _) = read_packet::<Event>(&mut bytes.as_slice()).unwrap();
        assert!(
            matches!(event,Event::Error{epoch:7,debug,element,..} if debug.contains("not-negotiated") && element=="/pipeline/decoder")
        );
    }
}
