//! Bounded HTTP ranges for native MP4/WebM seeking; optional MP4 index footer.
//!
//! qtdemux 1.26 reads SIDX for duration only; without MFRA it scans every earlier
//! fragment on a distant seek. Translate a validated direct SIDX into TFRA. The
//! original media bytes and offsets never change. The adapter stores no media
//! on disk and does not change video decoding policy. Queue2 budgets are unchanged; it
//! adds a 128 KiB read cache and at most 78 KiB of index data per source.
use anyhow::{Context, Result, bail};
use gst::{glib, prelude::*, subclass::prelude::*};
use gst_base::{
    prelude::*,
    subclass::{base_src::CreateSuccess, prelude::*},
};
use std::{
    io::Read,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const CHUNK: usize = 128 * 1024;
const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    Mp4,
    AudioWebm,
    VideoWebm,
}
impl Container {
    fn from_url(u: &url::Url) -> Option<Self> {
        if u.fragment().is_some_and(|s|s.starts_with("mochune-mp4=")){return Some(Self::Mp4);}
        match u.query_pairs().find(|(k, _)| k == "mime")?.1.as_ref() {
            "video/mp4" | "audio/mp4" => Some(Self::Mp4),
            "audio/webm" => Some(Self::AudioWebm),
            "video/webm" => Some(Self::VideoWebm),
            _ => None,
        }
    }
    fn caps(self) -> gst::Caps {
        match self {
            Self::Mp4 => gst::Caps::builder("video/quicktime")
                .field("variant", "iso")
                .build(),
            Self::AudioWebm => gst::Caps::builder("audio/webm").build(),
            Self::VideoWebm => gst::Caps::builder("video/webm").build(),
        }
    }
}
pub(super) fn prepare(uri: &str, range_enabled: bool) -> Result<String> {
    let supported = url::Url::parse(uri).is_ok_and(|u| {
        u.scheme() == "https" && declared_size(&u).is_some() && Container::from_url(&u).is_some()
    });
    if !range_enabled || !supported {
        return Ok(uri.to_owned());
    }
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    if !*REGISTERED.get_or_init(|| {
        gst::Element::register(
            None,
            "mochuneindexedhttpsrc",
            gst::Rank::PRIMARY,
            IndexedSource::static_type(),
        )
        .is_ok()
    }) {
        bail!("Cannot register the indexed HTTP source");
    }
    Ok(uri.replacen("https:", "mochunehttps:", 1))
}

fn declared_size(url: &url::Url) -> Option<u64> {
    let declared=url.query_pairs().find(|(k,_)|k=="clen").map(|(_,v)|v.into_owned()).or_else(||url.fragment()?.strip_prefix("mochune-mp4=").map(|s|s.split(';').next().unwrap_or(s).to_owned()));
    declared?.parse::<u64>().ok().filter(|n|*n>0&&*n<i64::MAX as u64-100_000)
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn be64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        data.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}
fn boxes(data: &[u8]) -> impl Iterator<Item = (usize, &[u8], &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        let size = be32(data, at)? as usize;
        if size < 8 {
            return None;
        }
        let end = at.checked_add(size)?;
        let item = (at, data.get(at + 4..at + 8)?, data.get(at..end)?);
        at = end;
        Some(item)
    })
}
fn track_scale(moov: &[u8], id: u32) -> Option<u32> {
    for (_, kind, trak) in boxes(moov.get(8..)?) {
        if kind != b"trak" {
            continue;
        }
        let children: Vec<_> = boxes(trak.get(8..)?).collect();
        let tkhd = children.iter().find(|(_, k, _)| *k == b"tkhd")?.2;
        let tid = be32(tkhd, if *tkhd.get(8)? == 1 { 28 } else { 20 })?;
        if tid != id {
            continue;
        }
        let mdia = children.iter().find(|(_, k, _)| *k == b"mdia")?.2;
        let mdhd = boxes(mdia.get(8..)?).find(|(_, k, _)| *k == b"mdhd")?.2;
        return be32(mdhd, if *mdhd.get(8)? == 1 { 28 } else { 20 }).filter(|s| *s > 0);
    }
    None
}
/// Reject incomplete, nested, non-random-access and oversized tables. Their
/// original demuxer path remains available, without advertising a fabricated index.
fn footer(header: &[u8], file_size: u64) -> Option<Vec<u8>> {
    let all: Vec<_> = boxes(header).collect();
    let moov = all.iter().find(|(_, k, _)| *k == b"moov")?.2;
    let (offset, _, sidx) = *all.iter().find(|(_, k, _)| *k == b"sidx")?;
    let version = *sidx.get(8)?;
    if version > 1 {
        return None;
    }
    let id = be32(sidx, 12)?;
    let scale = be32(sidx, 16)?;
    if scale == 0 {
        return None;
    }
    let track_scale = track_scale(moov, id)?;
    let (mut time, first, count_at) = if version == 0 {
        (be32(sidx, 20)? as u64, be32(sidx, 24)? as u64, 30)
    } else {
        (be64(sidx, 20)?, be64(sidx, 28)?, 38)
    };
    let count = u16::from_be_bytes(sidx.get(count_at..count_at + 2)?.try_into().ok()?) as usize;
    if count == 0 || count > MAX_ENTRIES || sidx.len() != count_at + 2 + count * 12 {
        return None;
    }
    let mut byte = (offset as u64)
        .checked_add(sidx.len() as u64)?
        .checked_add(first)?;
    let mut entries = Vec::with_capacity(count * 19);
    for i in 0..count {
        let at = count_at + 2 + i * 12;
        let reference = be32(sidx, at)?;
        let delta = be32(sidx, at + 4)?;
        let sap = be32(sidx, at + 8)?;
        // starts_with_SAP; type 1/2, zero SAP delta. TFRA names first sample.
        if reference >> 31 != 0
            || reference == 0
            || delta == 0
            || sap & 0x8000_0000 == 0
            || !matches!((sap >> 28) & 7, 1 | 2)
            || sap & 0x0fff_ffff != 0
        {
            return None;
        }
        if let Ok(at) = usize::try_from(byte) {
            if let Some(kind) = header.get(at.checked_add(4)?..at.checked_add(8)?) {
                if kind != b"moof" {
                    return None;
                }
            }
        }
        let end = byte.checked_add(reference as u64)?;
        if end > file_size {
            return None;
        }
        let timestamp = u64::try_from(time as u128 * track_scale as u128 / scale as u128).ok()?;
        entries.extend_from_slice(&timestamp.to_be_bytes());
        entries.extend_from_slice(&byte.to_be_bytes());
        entries.extend_from_slice(&[1, 1, 1]); // traf, trun and sample numbers
        time = time.checked_add(delta as u64)?;
        byte = end;
    }
    let size = 8 + 24 + entries.len() + 16;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&(size as u32).to_be_bytes());
    out.extend_from_slice(b"mfra");
    out.extend_from_slice(&((24 + entries.len()) as u32).to_be_bytes());
    out.extend_from_slice(b"tfra");
    out.extend_from_slice(&0x0100_0000u32.to_be_bytes());
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(count as u32).to_be_bytes());
    out.extend_from_slice(&entries);
    out.extend_from_slice(&16u32.to_be_bytes());
    out.extend_from_slice(b"mfro");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(size as u32).to_be_bytes());
    Some(out)
}

#[derive(Default)]
struct Settings {
    uri: Option<String>,
    agent: String,
    headers: Option<gst::Structure>,
}
struct Session {
    client: ureq::Agent,
    source_id: String,
    container: Container,
    uri: String,
    headers: Vec<(String, String)>,
    size: u64,
    tail: Vec<u8>,
    cache_at: u64,
    cache: Vec<u8>,
    initialized: bool,
    reader: Option<ureq::BodyReader<'static>>,
    reader_at: u64,
    reader_end: u64,
    requests: u64,
    bytes: u64,
}
impl Session {
    fn fetch(&mut self, offset: u64, length: usize) -> Result<Vec<u8>> {
        let started = Instant::now();
        let need = if self.size == 0 {
            length
        } else {
            length.min((self.size - offset) as usize)
        };
        let mut opened = false;
        if self.reader.is_none()
            || self.reader_at != offset
            || self.reader_end < offset + need as u64
        {
            self.reader = None;
            let requested = (2 * 1024 * 1024usize).max(need);
            let end = if self.size == 0 {
                offset + requested as u64 - 1
            } else {
                (offset + requested as u64 - 1).min(self.size - 1)
            };
            let mut request = self
                .client
                .get(&self.uri)
                .header("Range", format!("bytes={offset}-{end}"));
            for (k, v) in &self.headers {
                request = request.header(k.as_str(), v.as_str());
            }
            let response = request.call().context("Media range request failed")?;
            if response.status().as_u16() != 206 {
                bail!("Media server did not honor the requested byte range");
            }
            let range = response
                .headers()
                .get("content-range")
                .and_then(|h| h.to_str().ok())
                .context("Missing media Content-Range")?;
            let (start, end, total) = parse_range(range).context("Invalid media Content-Range")?;
            if start != offset
                || end - start + 1 > requested as u64
                || (self.size != 0 && total != self.size)
            {
                bail!("Media server returned an inconsistent byte range");
            }
            self.size = total;
            self.reader_at = start;
            self.reader_end = end + 1;
            self.reader = Some(response.into_body().into_reader());
            self.requests += 1;
            opened = true;
        }
        let need = length.min((self.size - offset) as usize);
        let mut data = vec![0; need];
        let result = self.reader.as_mut().unwrap().read_exact(&mut data);
        if let Err(error) = result {
            self.reader = None;
            return Err(error).context("Media range response was interrupted");
        }
        self.reader_at += data.len() as u64;
        let total = self.size;
        self.size = total;
        self.bytes += data.len() as u64;
        // One diagnostic per slow/distant read; never print signed URLs/headers.
        if started.elapsed() > Duration::from_secs(2) || opened {
            crate::diagnostics::record(
                "indexed_http_read",
                serde_json::json!({"source":self.source_id,"offset":offset,"bytes":data.len(),"elapsed_ms":started.elapsed().as_millis(),"request":self.requests}),
            );
        }
        Ok(data)
    }
    fn fetch_checked(&mut self, at: u64, length: usize, cancelled: &AtomicBool) -> Result<Vec<u8>> {
        if cancelled.load(Ordering::Acquire) {
            bail!("Media read cancelled");
        }
        match self.fetch(at, length) {
            Ok(data) => Ok(data),
            Err(first) if !retryable(&first, cancelled) => Err(first),
            Err(first) => {
                crate::diagnostics::record(
                    "indexed_http_retry",
                    serde_json::json!({"source":self.source_id,"offset":at}),
                );
                self.fetch(at, length)
                    .with_context(|| format!("Media read retry failed after {first:#}"))
            }
        }
    }
    fn read(&mut self, at: u64, length: usize, cancelled: &AtomicBool) -> Result<Vec<u8>> {
        if !self.initialized {
            self.cache = self.fetch_checked(0, CHUNK, cancelled)?;
            self.cache_at = 0;
            self.tail = if self.container == Container::Mp4 {
                footer(&self.cache, self.size).unwrap_or_default()
            } else {
                Vec::new()
            };
            self.initialized = true;
            crate::diagnostics::record(
                if self.container == Container::Mp4 {
                    "mp4_seek_index"
                } else {
                    "webm_range_source"
                },
                serde_json::json!({"source":self.source_id,"indexed":!self.tail.is_empty(),"index_bytes":self.tail.len(),"file_bytes":self.size,"read_cache_bytes":CHUNK}),
            );
        }

        let total = self.size + self.tail.len() as u64;
        if at >= total {
            return Ok(Vec::new());
        }
        let length = length.min((total - at) as usize);
        let mut out = Vec::with_capacity(length);
        if at < self.size {
            let media_len = length.min((self.size - at) as usize);
            let cached_end = self.cache_at + self.cache.len() as u64;
            if at < self.cache_at || at + media_len as u64 > cached_end {
                let chunk = CHUNK.max(media_len).min((self.size - at) as usize);
                self.cache = self.fetch_checked(at, chunk, cancelled)?;
                self.cache_at = at;
            }
            let begin = (at - self.cache_at) as usize;
            out.extend_from_slice(&self.cache[begin..begin + media_len]);
        }
        if out.len() < length {
            let tail_at = (at + out.len() as u64 - self.size) as usize;
            out.extend_from_slice(&self.tail[tail_at..tail_at + length - out.len()]);
        }
        Ok(out)
    }
}
fn retryable(error: &anyhow::Error, cancelled: &AtomicBool) -> bool {
    !cancelled.load(Ordering::Acquire)
        && !matches!(
            error.downcast_ref::<ureq::Error>(),
            Some(ureq::Error::StatusCode(400..=499))
        )
}

fn parse_range(value: &str) -> Option<(u64, u64, u64)> {
    let (span, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    (start <= end && end < total).then_some((start, end, total))
}
mod imp {
    use super::*;
    #[derive(Default)]
    pub struct IndexedSource {
        settings: Mutex<Settings>,
        session: Mutex<Option<Session>>,
        flushing: AtomicBool,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for IndexedSource {
        const NAME: &'static str = "MochuneIndexedHttpSource";
        type Type = super::IndexedSource;
        type ParentType = gst_base::BaseSrc;
        type Interfaces = (gst::URIHandler,);
    }
    impl ObjectImpl for IndexedSource {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_format(gst::Format::Bytes);
            self.obj().set_dynamic_size(true);
            self.obj().set_blocksize(CHUNK as u32);
        }
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPS.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("user-agent").build(),
                    glib::ParamSpecBoxed::builder::<gst::Structure>("extra-headers").build(),
                ]
            })
        }
        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let mut settings = self.settings.lock().unwrap();
            match pspec.name() {
                "user-agent" => settings.agent = value.get::<String>().unwrap_or_default(),
                "extra-headers" => settings.headers = value.get().ok(),
                _ => unreachable!(),
            }
        }
        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let settings = self.settings.lock().unwrap();
            match pspec.name() {
                "user-agent" => settings.agent.to_value(),
                "extra-headers" => settings.headers.to_value(),
                _ => unreachable!(),
            }
        }
    }
    impl GstObjectImpl for IndexedSource {}
    impl ElementImpl for IndexedSource {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: OnceLock<gst::subclass::ElementMetadata> = OnceLock::new();
            Some(META.get_or_init(|| {
                gst::subclass::ElementMetadata::new(
                    "Mochune indexed HTTP source",
                    "Source/Network",
                    "Bounded HTTP ranges with MP4 random access",
                    "Mochune",
                )
            }))
        }
        fn pad_templates() -> &'static [gst::PadTemplate] {
            static PADS: OnceLock<Vec<gst::PadTemplate>> = OnceLock::new();
            PADS.get_or_init(|| {
                vec![
                    gst::PadTemplate::new(
                        "src",
                        gst::PadDirection::Src,
                        gst::PadPresence::Always,
                        &gst::Caps::new_any(),
                    )
                    .unwrap(),
                ]
            })
        }
    }
    impl URIHandlerImpl for IndexedSource {
        const URI_TYPE: gst::URIType = gst::URIType::Src;
        fn protocols() -> &'static [&'static str] {
            &["mochunehttps"]
        }
        fn uri(&self) -> Option<String> {
            self.settings.lock().unwrap().uri.clone()
        }
        fn set_uri(&self, uri: &str) -> Result<(), glib::Error> {
            if self.obj().current_state() > gst::State::Ready {
                return Err(glib::Error::new(
                    gst::URIError::BadState,
                    "Cannot change an active media source",
                ));
            }
            let https = uri.replacen("mochunehttps:", "https:", 1);
            if url::Url::parse(&https)
                .ok()
                .is_none_or(|u| u.scheme() != "https" || u.host_str().is_none())
            {
                return Err(glib::Error::new(
                    gst::URIError::BadUri,
                    "Invalid HTTPS media URI",
                ));
            }
            self.settings.lock().unwrap().uri = Some(https);
            Ok(())
        }
    }
    impl BaseSrcImpl for IndexedSource {
        fn query(&self, query: &mut gst::QueryRef) -> bool {
            if let gst::QueryViewMut::Scheduling(q) = query.view_mut() {
                // Known-length byte ranges support pull mode. Capability probes
                // must not open a connection or fetch media speculatively.
                q.set(
                    gst::SchedulingFlags::SEEKABLE | gst::SchedulingFlags::BANDWIDTH_LIMITED,
                    1,
                    -1,
                    0,
                );
                q.add_scheduling_modes(&[gst::PadMode::Pull, gst::PadMode::Push]);
                return true;
            }
            BaseSrcImplExt::parent_query(self, query)
        }
        fn caps(&self, filter: Option<&gst::Caps>) -> Option<gst::Caps> {
            let caps = self
                .settings
                .lock()
                .unwrap()
                .uri
                .as_deref()
                .and_then(|uri| url::Url::parse(uri).ok())
                .and_then(|u| Container::from_url(&u))
                .map(Container::caps)
                .unwrap_or_else(gst::Caps::new_any);
            Some(filter.map_or_else(|| caps.clone(), |f| f.intersect(&caps)))
        }
        fn start(&self) -> Result<(), gst::ErrorMessage> {
            self.flushing.store(false, Ordering::Release);
            let result = (|| -> Result<Session> {
                let settings = self.settings.lock().unwrap();
                let uri = settings.uri.clone().context("Missing media URI")?;
                let mut parsed=url::Url::parse(&uri)?;
                let size=declared_size(&parsed).context("Missing media byte length")?;
                let container=Container::from_url(&parsed).context("Unsupported media container")?;
                let uri=if parsed.fragment().is_some_and(|s|s.starts_with("mochune-mp4=")){parsed.set_fragment(None);parsed.to_string()}else{uri};
                let mut headers = Vec::new();
                if !settings.agent.is_empty() {
                    headers.push(("User-Agent".into(), settings.agent.clone()));
                }
                if let Some(extra) = &settings.headers {
                    for (k, v) in extra.iter() {
                        if let Ok(v) = v.get::<String>() {
                            if !k.eq_ignore_ascii_case("range")
                                && !k.eq_ignore_ascii_case("accept-encoding")
                            {
                                headers.push((k.to_string(), v));
                            }
                        }
                    }
                }
                headers.push(("Accept-Encoding".into(), "identity".into()));
                let client = ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(12)))
                    .timeout_connect(Some(Duration::from_secs(5)))
                    .build()
                    .new_agent();
                Ok(Session {
                    client,
                    source_id: self.obj().path_string().to_string(),
                    container,
                    uri,
                    headers,
                    size,
                    tail: Vec::new(),
                    cache_at: 0,
                    cache: Vec::new(),
                    initialized: false,
                    reader: None,
                    reader_at: 0,
                    reader_end: 0,
                    requests: 0,
                    bytes: 0,
                })
            })();
            match result {
                Ok(session) => {
                    *self.session.lock().unwrap() = Some(session);
                    Ok(())
                }
                Err(error) => Err(gst::error_msg!(
                    gst::ResourceError::OpenRead,
                    ["Cannot initialize media ranges: {error:#}"]
                )),
            }
        }
        fn stop(&self) -> Result<(), gst::ErrorMessage> {
            if let Some(s) = self.session.lock().unwrap().take() {
                crate::diagnostics::record(
                    "indexed_http_closed",
                    serde_json::json!({"requests":s.requests,"bytes":s.bytes}),
                );
            }
            Ok(())
        }
        fn is_seekable(&self) -> bool {
            true
        }
        fn size(&self) -> Option<u64> {
            self.session
                .lock()
                .unwrap()
                .as_ref()
                .map(|s| s.size + s.tail.len() as u64)
        }
        fn unlock(&self) -> Result<(), gst::ErrorMessage> {
            self.flushing.store(true, Ordering::Release);
            Ok(())
        }
        fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
            self.flushing.store(false, Ordering::Release);
            Ok(())
        }
        fn create(
            &self,
            offset: u64,
            _: Option<&mut gst::BufferRef>,
            length: u32,
        ) -> Result<CreateSuccess, gst::FlowError> {
            if self.flushing.load(Ordering::Acquire) {
                return Err(gst::FlowError::Flushing);
            }
            // Header/index I/O happens on GStreamer's streaming task, never
            // during UI-thread state changes or scheduling capability queries.
            let length = (length as usize).min(CHUNK);
            let result = self
                .session
                .lock()
                .unwrap()
                .as_mut()
                .ok_or(gst::FlowError::Flushing)?
                .read(offset, length, &self.flushing);
            if self.flushing.load(Ordering::Acquire) {
                return Err(gst::FlowError::Flushing);
            }
            match result {
                Ok(data) if data.is_empty() => Err(gst::FlowError::Eos),
                Ok(data) => {
                    let size = data.len();
                    let mut buffer = gst::Buffer::from_mut_slice(data);
                    let b = buffer.get_mut().unwrap();
                    b.set_offset(offset);
                    b.set_offset_end(offset + size as u64);
                    Ok(CreateSuccess::NewBuffer(buffer))
                }
                Err(e) => {
                    gst::element_error!(
                        self.obj(),
                        gst::ResourceError::Read,
                        ("Media range read failed: {e:#}"),
                        ["{e:#}"]
                    );
                    Err(gst::FlowError::Error)
                }
            }
        }
    }
}
glib::wrapper! { pub struct IndexedSource(ObjectSubclass<imp::IndexedSource>) @extends gst_base::BaseSrc, gst::Element, gst::Object, @implements gst::URIHandler; }

#[cfg(test)]
mod tests {
    use super::*;
    fn atom(kind: &[u8; 4], body: Vec<u8>) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend(kind);
        v.extend(body);
        v
    }
    fn fixture() -> Vec<u8> {
        let mut tkhd = vec![0; 16];
        tkhd[12..16].copy_from_slice(&1u32.to_be_bytes());
        let mut mdhd = vec![0; 16];
        mdhd[12..16].copy_from_slice(&24000u32.to_be_bytes());
        let mut trak = atom(b"tkhd", tkhd);
        trak.extend(atom(b"mdia", atom(b"mdhd", mdhd)));
        let mut data = atom(b"moov", atom(b"trak", trak));
        let mut sidx = vec![0; 4];
        sidx.extend(1u32.to_be_bytes());
        sidx.extend(24000u32.to_be_bytes());
        sidx.extend(0u32.to_be_bytes());
        sidx.extend(0u32.to_be_bytes());
        sidx.extend([0, 0, 0, 2]);
        for _ in 0..2 {
            sidx.extend(100u32.to_be_bytes());
            sidx.extend(48000u32.to_be_bytes());
            sidx.extend(0x90000000u32.to_be_bytes());
        }
        data.extend(atom(b"sidx", sidx));
        data
    }
    #[test]
    fn source_mp4_hint_supplies_container_and_exact_size_without_changing_http_url(){let u=url::Url::parse("https://example.test/movie.mp4#mochune-mp4=401204942").unwrap();assert_eq!(Container::from_url(&u),Some(Container::Mp4));assert_eq!(declared_size(&u),Some(401204942));assert!(Container::from_url(&url::Url::parse("https://example.test/movie.mp4").unwrap()).is_none());}
    #[test]
    fn cancelled_reads_and_expired_links_do_not_retry_the_same_request() {
        let cancel = AtomicBool::new(false);
        let expired =
            anyhow::Error::new(ureq::Error::StatusCode(403)).context("Media range request failed");
        assert!(!retryable(&expired, &cancel));
        let transient = anyhow::Error::new(ureq::Error::StatusCode(503));
        assert!(retryable(&transient, &cancel));
        cancel.store(true, Ordering::Release);
        assert!(!retryable(&transient, &cancel));
    }
    #[test]
    fn only_supported_known_length_https_media_use_the_adapter() {
        gst::init().unwrap();
        let mp4 = "https://example.test/video?mime=video%2Fmp4&clen=1000";
        assert_eq!(prepare(mp4, false).unwrap(), mp4);
        for mime in ["video%2Fmp4", "audio%2Fmp4", "video%2Fwebm", "audio%2Fwebm"] {
            let uri = format!("https://example.test/video?mime={mime}&clen=1000");
            assert_eq!(prepare(&uri, false).unwrap(), uri);
            assert!(prepare(&uri, true).unwrap().starts_with("mochunehttps:"));
        }
        assert_eq!(
            Container::AudioWebm.caps().structure(0).unwrap().name(),
            "audio/webm"
        );
        for uri in [
            "https://example.test/video?mime=video%2Fmp4",
            "https://example.test/video?mime=audio%2Fogg&clen=1000",
            "https://example.test/video?mime=video%2Fmp4&clen=0",
        ] {
            assert_eq!(prepare(uri, true).unwrap(), uri);
        }
    }
    #[test]
    fn translates_index_without_moving_media() {
        let h = fixture();
        let f = footer(&h, h.len() as u64 + 200).unwrap();
        assert_eq!(&f[4..8], b"mfra");
        assert_eq!(be64(&f, 40), Some(h.len() as u64));
        assert_eq!(be64(&f, 51), Some(48000));
        assert_eq!(be64(&f, 59), Some(h.len() as u64 + 100));
        assert_eq!(be32(&f, f.len() - 4), Some(f.len() as u32));
    }
    #[test]
    fn translates_version_one_and_different_track_timescale() {
        let h = fixture();
        let sidx = boxes(&h).find(|(_, k, _)| *k == b"sidx").unwrap().2;
        let mut body = sidx[8..20].to_vec();
        body[0] = 1;
        body.extend(0u64.to_be_bytes());
        body.extend(0u64.to_be_bytes());
        body.extend(&sidx[28..]);
        let moov = boxes(&h).find(|(_, k, _)| *k == b"moov").unwrap().2;
        let mut h = moov.to_vec();
        h.extend(atom(b"sidx", body));
        let mdhd = h.windows(4).position(|x| x == b"mdhd").unwrap();
        h[mdhd + 16..mdhd + 20].copy_from_slice(&48000u32.to_be_bytes());
        let f = footer(&h, h.len() as u64 + 200).unwrap();
        assert_eq!(be64(&f, 51), Some(96000));
        assert_eq!(be64(&f, 40), Some(h.len() as u64));
    }
    #[test]
    fn rejects_incomplete_and_out_of_file_indexes() {
        let h = fixture();
        assert!(footer(&h[..h.len() - 1], 1000).is_none());
        assert!(footer(&h, h.len() as u64 + 199).is_none());
        for n in 0..h.len() {
            let _ = footer(&h[..n], 1000);
        }
    }
    #[test]
    fn rejects_nested_and_non_random_access() {
        let mut h = fixture();
        let at = h.len() - 24;
        h[at] |= 0x80;
        assert!(footer(&h, 10000).is_none());
        let mut h = fixture();
        let at = h.len() - 4;
        h[at] = 0;
        assert!(footer(&h, 10000).is_none());
        h[at] = 0x80; // SAP present but its access-point type is undefined.
        assert!(footer(&h, 10000).is_none());
    }
    #[test]
    fn validates_content_range() {
        assert_eq!(parse_range("bytes 3-8/100"), Some((3, 8, 100)));
        for s in [
            "bytes 8-3/100",
            "bytes 0-100/100",
            "bytes */100",
            "bytes 0-1/*",
            "garbage",
        ] {
            assert!(parse_range(s).is_none());
        }
    }
}
