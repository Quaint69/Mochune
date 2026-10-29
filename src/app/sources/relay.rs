//! On-demand HLS header relay. Stateless URLs, four connections, streamed bodies.
//! Never retains fragments or buffers a complete movie in RAM.
use super::providers::{Client, META_LIMIT, public_url};
use ani_lib::{RequestHeaders, StreamLink};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response,
    body::{Frame, Incoming},
    service::service_fn,
};
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinHandle};
type Error = Box<dyn std::error::Error + Send + Sync>;
type Body = UnsyncBoxBody<Bytes, Error>;
pub struct Relay {
    task: JoinHandle<()>,
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct ContextData {
    client: Client,
    headers: RequestHeaders,
    base: String,
    nonce: String,
    slots: Arc<Semaphore>,
    height: Option<u32>,
    trace: AtomicUsize,
    english_audio: bool,
    created: std::time::Instant,
    last_slow_log: AtomicU64,
}
fn trace(ctx: &ContextData, event: &str, data: serde_json::Value) {
    if ctx.trace.fetch_add(1, Ordering::Relaxed) < 20 {
        crate::diagnostics::record(&format!("source_relay_{event}"), data);
    }
}
fn encode(base: &str, url: &str) -> String {
    format!("{base}{}", URL_SAFE_NO_PAD.encode(url))
}
fn decode(path: &str, nonce: &str) -> Result<String> {
    let encoded = path
        .strip_prefix(&format!("/{nonce}/"))
        .context("Invalid relay token")?;
    ensure!(encoded.len() <= 16384, "Relay URL too long");
    let raw = String::from_utf8(URL_SAFE_NO_PAD.decode(encoded)?)?;
    public_url(&raw)?;
    Ok(raw)
}
fn rewrite(text: &str, upstream: &str, base: &str) -> Result<String> {
    let u = url::Url::parse(upstream)?;
    let mut output = String::with_capacity(text.len() + 1024);
    for line in text.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') {
            let target = u.join(line)?;
            public_url(target.as_str())?;
            output.push_str(&encode(base, target.as_str()));
        } else if let Some((prefix, rest)) = line.split_once("URI=\"") {
            let (uri, suffix) = rest.split_once('"').context("Invalid HLS URI")?;
            let target = u.join(uri)?;
            public_url(target.as_str())?;
            output.push_str(prefix);
            output.push_str("URI=\"");
            output.push_str(&encode(base, target.as_str()));
            output.push('"');
            output.push_str(suffix);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    Ok(output)
}
fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never: Infallible| match never {})
        .boxed_unsync()
}
fn failure(status: u16, message: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Cache-Control", "no-store")
        .body(full(message.to_owned()))
        .unwrap()
}
pub async fn start(client: Client, stream: &StreamLink) -> Result<(Relay, StreamLink)> {
    public_url(&stream.url)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|_| anyhow::anyhow!("Cannot secure local stream relay"))?;
    let nonce = URL_SAFE_NO_PAD.encode(random);
    let base = format!("http://{address}/{nonce}/");
    let mut local = stream.clone();
    local.url = encode(&base, &stream.url);
    local.headers = Default::default();
    for track in &mut local.subtitles {
        track.url = encode(&base, &track.url);
    }
    let height = stream.resolution.trim_end_matches('p').parse().ok();
    let ctx = Arc::new(ContextData {
        client,
        headers: stream.headers.clone(),
        base,
        nonce,
        slots: Arc::new(Semaphore::new(4)),
        height,
        trace: AtomicUsize::new(0),
        english_audio: stream.provider == "VixSrc",
        created: std::time::Instant::now(),
        last_slow_log: AtomicU64::new(0),
    });
    let task = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            // Do not create an unbounded number of tasks waiting for a connection slot.
            let Ok(permit) = ctx.slots.clone().try_acquire_owned() else {
                trace(&ctx, "connection_limit", serde_json::json!({}));
                drop(socket);
                continue;
            };
            let ctx = ctx.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let service = service_fn(move |request| {
                    let ctx = ctx.clone();
                    async move {
                        Ok::<_, Infallible>(match serve(request, ctx.clone()).await {
                            Ok(response) => response,
                            Err(error) => {
                                trace(
                                    &ctx,
                                    "transfer_failed",
                                    serde_json::json!({"reason":error.to_string().split("http").next().unwrap_or("Source connection failed")}),
                                );
                                failure(502, "Source transfer failed. Retry playback.")
                            }
                        })
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    Ok((Relay { task }, local))
}
async fn serve(req: Request<Incoming>, ctx: Arc<ContextData>) -> Result<Response<Body>> {
    let began = std::time::Instant::now();
    if req.method() != hyper::Method::GET && req.method() != hyper::Method::HEAD {
        return Ok(failure(405, "Unsupported method"));
    }
    let target = match decode(req.uri().path(), &ctx.nonce) {
        Ok(url) => url,
        Err(_) => return Ok(failure(403, "Invalid relay request")),
    };
    let mut upstream = ctx.client.0.request(
        if req.method() == hyper::Method::HEAD {
            reqwest::Method::HEAD
        } else {
            reqwest::Method::GET
        },
        &target,
    );
    if let Some(v) = &ctx.headers.referer {
        upstream = upstream.header("Referer", v);
    }
    if let Some(v) = &ctx.headers.origin {
        upstream = upstream.header("Origin", v);
    }
    for (k, v) in &ctx.headers.extra {
        upstream = upstream.header(k, v);
    }
    for name in ["Range", "If-Range"] {
        if let Some(v) = req.headers().get(name) {
            upstream = upstream.header(name, v);
        }
    }
    let response = upstream.send().await?;
    let status = response.status();
    trace(
        &ctx,
        "response",
        serde_json::json!({"status":status.as_u16(),"key":url::Url::parse(&target)?.path().contains("key"),"range":req.headers().contains_key("Range"),"length":response.content_length(),"type":response.headers().get("content-type").and_then(|h|h.to_str().ok())}),
    );
    if !status.is_success() {
        return Ok(failure(
            status.as_u16(),
            "Upstream source rejected the stream request.",
        ));
    }
    // Validate the final redirect, too. No credentials, headers or stream URLs go to logs.
    public_url(response.url().as_str())?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let playlist =
        content_type.contains("mpegurl") || url::Url::parse(&target)?.path().ends_with(".m3u8");
    let mut builder = Response::builder()
        .status(status.as_u16())
        .header("Cache-Control", "no-store");
    for key in ["Content-Range", "Accept-Ranges"] {
        if let Some(value) = response.headers().get(key) {
            builder = builder.header(key, value);
        }
    }
    if playlist {
        builder = builder.header("Content-Type", "application/vnd.apple.mpegurl");
    } else if !content_type.is_empty() {
        builder = builder.header("Content-Type", &content_type);
    }
    if req.method() == hyper::Method::HEAD {
        if let Some(length) = response.content_length() {
            builder = builder.header("Content-Length", length);
        }
        return Ok(builder.body(full(Bytes::new()))?);
    }
    if playlist {
        let final_url = response.url().to_string();
        let mut bytes = Vec::new();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            ensure!(
                bytes.len() + chunk.len() <= META_LIMIT,
                "HLS manifest exceeds limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        let text = String::from_utf8(bytes)?;
        ensure!(
            text.trim_start().starts_with("#EXTM3U"),
            "Source returned invalid HLS"
        );
        return Ok(builder.body(full(rewrite(
            &select_variant(&text, ctx.height, ctx.english_audio),
            &final_url,
            &ctx.base,
        )?))?);
    }
    const RESOURCE_CAP: u64 = 32 * 1024 * 1024;
    ensure!(
        response.content_length().is_none_or(|n| n <= RESOURCE_CAP),
        "HLS resource exceeds limit"
    );
    if let Some(length) = response.content_length() {
        builder = builder.header("Content-Length", length);
    }
    let expected = response.content_length();
    let mut total = 0u64;
    let chunks =
        response
            .bytes_stream()
            .map(move |chunk| -> std::result::Result<Frame<Bytes>, Error> {
                let chunk = chunk?;
                total += chunk.len() as u64;
                if total > RESOURCE_CAP {
                    return Err("HLS resource exceeds limit".into());
                }
                if Some(total)==expected && total>64*1024 && began.elapsed().as_secs_f64()>2. {
                    let stamp=ctx.created.elapsed().as_millis() as u64;
                    if {let previous=ctx.last_slow_log.load(Ordering::Relaxed);stamp>=previous+10_000&&ctx.last_slow_log.compare_exchange(previous,stamp,Ordering::Relaxed,Ordering::Relaxed).is_ok()}{crate::diagnostics::record("source_relay_slow_fragment",serde_json::json!({"bytes":total,"transfer_ms":began.elapsed().as_millis(),"bytes_per_second":total as f64/began.elapsed().as_secs_f64()}));}
                }
                Ok(Frame::data(chunk))
            });
    Ok(builder.body(StreamBody::new(chunks).boxed_unsync())?)
}
fn remove_subtitle_group(line: &str) -> String {
    let mut result = line.to_owned();
    if let Some(start) = result.find(",SUBTITLES=\"") {
        if let Some(end) = result[start + 12..].find('"') {
            result.replace_range(start..start + 12 + end + 1, "");
        }
    }
    result
}
fn select_variant(text: &str, height: Option<u32>, prefer_english: bool) -> String {
    let chosen = height.and_then(|requested| {
        text.lines()
            .filter_map(super::providers::hls_height)
            .min_by_key(|h| h.abs_diff(requested))
    });
    let english = prefer_english
        && text.lines().any(|l| {
            l.starts_with("#EXT-X-MEDIA:")
                && l.contains("TYPE=AUDIO")
                && (l.contains("LANGUAGE=\"en\"") || l.contains("LANGUAGE=\"eng\""))
        });
    let mut english_selected = false;
    let mut keep = true;
    let mut output = String::with_capacity(text.len());
    for line in text.lines() {
        // Captions are loaded on demand by Mochune, never preroll text renditions.
        if line.starts_with("#EXT-X-MEDIA:") && line.contains("TYPE=SUBTITLES") {
            continue;
        }
        let line = remove_subtitle_group(line);
        let line = line.as_str();
        if line.starts_with("#EXT-X-STREAM-INF:") {
            keep = chosen.is_none() || super::providers::hls_height(line) == chosen;
            if keep {
                output.push_str(line);
                output.push('\n');
            }
            continue;
        }
        if !line.starts_with('#') && !line.trim().is_empty() {
            if keep {
                output.push_str(line);
                output.push('\n');
            }
            keep = true;
            continue;
        }
        if english && line.starts_with("#EXT-X-MEDIA:") && line.contains("TYPE=AUDIO") {
            if !line.contains("LANGUAGE=\"en\"") && !line.contains("LANGUAGE=\"eng\"") {
                continue;
            }
            let mut line = line.replace("DEFAULT=YES", "DEFAULT=NO");
            if !english_selected
                && (line.contains("LANGUAGE=\"en\"") || line.contains("LANGUAGE=\"eng\""))
            {
                line = line.replace("DEFAULT=NO", "DEFAULT=YES");
                english_selected = true;
            }
            output.push_str(&line);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_and_relative_segments_preserve_identity() {
        let base = "http://127.0.0.1:1234/token/";
        let out = rewrite(
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key?k=1\"\n../video.ts?q=2\n",
            "https://example.org/a/list.m3u8",
            base,
        )
        .unwrap();
        assert!(out.contains(&encode(base, "https://example.org/a/key?k=1")));
        assert!(out.contains(&encode(base, "https://example.org/video.ts?q=2")));
    }
    #[test]
    fn manual_quality_keeps_audio_but_removes_other_video_variants() {
        let text = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,LANGUAGE=\"en\",DEFAULT=YES,URI=\"audio.m3u8\"\n#EXT-X-STREAM-INF:RESOLUTION=1920x1080\nhigh.m3u8\n#EXT-X-STREAM-INF:RESOLUTION=1280x720\nmedium.m3u8\n";
        let filtered = select_variant(text, Some(720), true);
        assert!(filtered.contains("audio.m3u8"));
        assert!(filtered.contains("medium.m3u8"));
        assert!(!filtered.contains("high.m3u8"));
    }
    #[test]
    fn relay_requires_its_session_token() {
        assert!(decode("/other/abc", "token").is_err());
    }
}
