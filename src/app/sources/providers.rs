//! Small native adapters. No browser, Python runtime, speculative requests or video decoder.
use ani_lib::{RequestHeaders, StreamLink};
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
use url::Url;

pub const META_LIMIT: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub description: String,
}
#[derive(Clone)]
pub struct Streams {
    pub entry: Entry,
    pub streams: Vec<StreamLink>,
}
pub struct Pages {
    pub entry: Entry,
    pub urls: Vec<String>,
    pub referer: Option<String>,
}
#[derive(Clone)]
pub struct Client(pub reqwest::Client);
impl Client {
    pub fn new() -> Result<Self> {
        Ok(Self(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .read_timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= 5 || public_url(attempt.url().as_str()).is_err()
                    {
                        attempt.error("Source redirect was rejected")
                    } else {
                        attempt.follow()
                    }
                }))
                .user_agent("Mochune/0.1 (+native media client)")
                .build()?,
        ))
    }
    pub async fn bytes(
        &self,
        url: &str,
        headers: &[(String, String)],
        cap: usize,
    ) -> Result<Vec<u8>> {
        public_url(url)?;
        let mut request = self.0.get(url).timeout(Duration::from_secs(20));
        for (k, v) in headers {
            request = request.header(k, v);
        }
        let response = request.send().await.context("Source connection failed")?;
        let status = response.status();
        ensure!(
            status.is_success(),
            "Source returned HTTP {}{}",
            status.as_u16(),
            if status.as_u16() == 403 {
                " (access denied)"
            } else {
                ""
            }
        );
        ensure!(
            response.content_length().is_none_or(|n| n <= cap as u64),
            "Source response exceeds the size limit"
        );
        let mut bytes = Vec::new();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.context("Source transfer interrupted")?;
            ensure!(
                bytes.len() + chunk.len() <= cap,
                "Source response exceeds the size limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    pub async fn text(&self, url: &str) -> Result<String> {
        Ok(String::from_utf8(self.bytes(url, &[], META_LIMIT).await?)
            .context("Invalid source text")?)
    }
    pub async fn json(&self, url: &str) -> Result<Value> {
        Ok(
            serde_json::from_slice(&self.bytes(url, &[], META_LIMIT).await?)
                .context("Source returned invalid JSON")?,
        )
    }
}
pub fn public_url(raw: &str) -> Result<Url> {
    let u = Url::parse(raw).context("Invalid source URL")?;
    ensure!(
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.port().is_none_or(|p| p == 443),
        "Unsupported source URL"
    );
    let host = u.host_str().context("Source URL has no host")?;
    ensure!(
        host != "localhost" && !host.ends_with(".localhost") && !host.ends_with(".local"),
        "Local source hosts are not allowed"
    );
    match u.host() {
        Some(url::Host::Ipv4(v)) => ensure!(
            !v.is_loopback() && !v.is_unspecified() && !v.is_private() && !v.is_link_local(),
            "Private source hosts are not allowed"
        ),
        Some(url::Host::Ipv6(v)) => ensure!(
            !v.is_loopback()
                && !v.is_unspecified()
                && !v.is_unique_local()
                && !v.is_unicast_link_local()
                && v.to_ipv4_mapped().is_none(),
            "Private source hosts are not allowed"
        ),
        _ => (),
    }
    Ok(u)
}
fn endpoint(base: &str, params: &[(&str, &str)]) -> String {
    let mut u = Url::parse(base).unwrap();
    u.query_pairs_mut().extend_pairs(params.iter().copied());
    u.into()
}
fn text(v: &Value, key: &str) -> String {
    v[key]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v[key].as_u64().map(|n| n.to_string()).unwrap_or_default())
}
fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("static CSS selector")
}
fn plain(s: &str) -> String {
    Html::parse_fragment(s)
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(8192)
        .collect()
}
fn link(url: String, resolution: String, provider: &str, referer: Option<String>) -> StreamLink {
    StreamLink {
        hls: url.contains(".m3u8"),
        url,
        resolution,
        provider: provider.into(),
        downloadable: false,
        headers: RequestHeaders {
            referer,
            origin: None,
            extra: BTreeMap::new(),
        },
        subtitles: Vec::new(),
    }
}

/// Only selected Dramachi MP4 files enter this path. A tiny range request
/// supplies exact length for pull-mode index reads, without changing the server URL.
pub async fn indexed_movie_url(c: &Client, raw: &str) -> Result<String> {
    let mut u = public_url(raw)?;
    let response =
        c.0.get(u.clone())
            .header("Range", "bytes=0-8191")
            .header("Accept-Encoding", "identity")
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .context("Movie server did not respond")?;
    ensure!(
        response.status() == 206,
        "Movie server does not support indexed playback (HTTP {})",
        response.status().as_u16()
    );
    let range = response
        .headers()
        .get("content-range")
        .and_then(|h| h.to_str().ok())
        .context("Movie server did not supply its file length")?;
    let total = range
        .rsplit('/')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0 && *n < i64::MAX as u64 - 100000)
        .context("Movie file length is invalid")?;
    ensure!(
        range.starts_with("bytes 0-"),
        "Movie server returned the wrong byte range"
    );
    let mut bytes = Vec::new();
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        ensure!(
            bytes.len() + chunk.len() <= 8192,
            "Movie server ignored the bounded range"
        );
        bytes.extend_from_slice(&chunk);
    }
    ensure!(
        bytes.len() <= 8192 && bytes.get(4..8) == Some(b"ftyp"),
        "Movie server did not return an MP4 file"
    );
    let mut at = 0usize;
    let mut index = 0u64;
    while at + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        if &bytes[at + 4..at + 8] == b"moov" {
            index = size as u64;
            break;
        }
        if size < 8 {
            break;
        }
        at = match at.checked_add(size) {
            Some(n) => n,
            None => break,
        };
    }
    ensure!(
        index <= 32 * 1024 * 1024,
        "Movie metadata index exceeds the 32 MiB limit. Try another provider"
    );
    u.set_fragment(Some(&format!("mochune-mp4={total};index={index}")));
    Ok(u.into())
}
fn hls_attr(line: &str, key: &str) -> Option<String> {
    let text = line.split_once(':')?.1;
    let mut quoted = false;
    let mut start = 0;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ',')))
    {
        if c == '"' {
            quoted = !quoted;
        }
        if c == ',' && !quoted {
            let part = text[start..i].trim();
            if let Some((k, v)) = part.split_once('=') {
                if k == key {
                    return Some(v.trim_matches('"').to_owned());
                }
            }
            start = i + 1;
        }
    }
    None
}
fn advertised_captions(text: &str, base: &str) -> Vec<ani_lib::SubtitleTrack> {
    let Ok(base) = Url::parse(base) else {
        return Vec::new();
    };
    text.lines()
        .filter(|s| s.starts_with("#EXT-X-MEDIA:") && s.contains("TYPE=SUBTITLES"))
        .filter_map(|s| {
            let u = base.join(&hls_attr(s, "URI")?).ok()?;
            if public_url(u.as_str()).is_err() {
                return None;
            }
            Some(ani_lib::SubtitleTrack {
                label: hls_attr(s, "NAME")
                    .or_else(|| hls_attr(s, "LANGUAGE"))
                    .unwrap_or_else(|| "Captions".into()),
                url: u.into(),
                default: s.contains("DEFAULT=YES"),
            })
        })
        .take(32)
        .collect()
}
pub async fn discover_hls_options(c: &Client, streams: &mut Vec<StreamLink>) -> Result<()> {
    if streams.len() != 1 || !streams[0].hls {
        return Ok(());
    }
    let source = streams[0].clone();
    let mut h = source
        .headers
        .extra
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<Vec<_>>();
    if let Some(v) = &source.headers.referer {
        h.push(("Referer".into(), v.clone()));
    }
    if let Some(v) = &source.headers.origin {
        h.push(("Origin".into(), v.clone()));
    }
    // Discovery failure must not prevent the original advertised stream from playing.
    let Ok(data) = c.bytes(&source.url, &h, META_LIMIT).await else {
        return Ok(());
    };
    let Ok(text) = String::from_utf8(data) else {
        return Ok(());
    };
    if !text.starts_with("#EXTM3U") {
        return Ok(());
    }
    let heights = text
        .lines()
        .filter(|s| s.starts_with("#EXT-X-STREAM-INF:"))
        .filter_map(hls_height)
        .collect::<std::collections::BTreeSet<_>>();
    let extra = advertised_captions(&text, &source.url);
    for t in extra {
        if !streams[0].subtitles.iter().any(|s| s.url == t.url) {
            streams[0].subtitles.push(t);
        }
    }
    if !heights.is_empty() {
        let source = streams[0].clone();
        *streams = heights
            .into_iter()
            .rev()
            .map(|h| {
                let mut s = source.clone();
                s.resolution = format!("{h}p");
                s
            })
            .collect();
    }
    Ok(())
}
pub async fn movie_search(c: &Client, query: &str, provider: i32) -> Result<Vec<Entry>> {
    if provider == 1 {
        if let Ok(id) = tmdb_id(query) {
            return Ok(vec![Entry {
                id: id.to_string(),
                title: tmdb_title_hint(query).unwrap_or_else(|| format!("Movie · TMDB {id}")),
                detail: "VixSrc".into(),
                ..Default::default()
            }]);
        }
        // Public metadata lookup, two bounded requests; no embedded/shared API secret.
        let found = c
            .json(&endpoint(
                "https://www.wikidata.org/w/api.php",
                &[
                    ("action", "wbsearchentities"),
                    ("search", query),
                    ("language", "en"),
                    ("format", "json"),
                    ("limit", "20"),
                ],
            ))
            .await?;
        let hits = found["search"]
            .as_array()
            .context("Movie title lookup is unavailable")?;
        let ids = hits
            .iter()
            .filter_map(|h| h["id"].as_str())
            .take(20)
            .collect::<Vec<_>>()
            .join("|");
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let details = c
            .json(&endpoint(
                "https://www.wikidata.org/w/api.php",
                &[
                    ("action", "wbgetentities"),
                    ("ids", &ids),
                    ("props", "claims"),
                    ("format", "json"),
                ],
            ))
            .await?;
        let mut seen = std::collections::HashSet::new();
        return Ok(hits
            .iter()
            .filter_map(|h| {
                let entity = h["id"].as_str()?;
                let id = details["entities"][entity]["claims"]["P4947"]
                    .as_array()?
                    .iter()
                    .find_map(|c| c["mainsnak"]["datavalue"]["value"].as_str())?;
                if tmdb_id(id).is_err() || !seen.insert(id.to_owned()) {
                    return None;
                }
                Some(Entry {
                    id: id.into(),
                    title: text(h, "label"),
                    detail: format!("{} · VixSrc", text(h, "description")),
                    ..Default::default()
                })
            })
            .collect());
    }
    let v = c
        .json(&endpoint(
            "https://api.nodeobjects.com/",
            &[
                ("interface", "search"),
                ("q", query),
                ("filter", "all"),
                ("page", "1"),
            ],
        ))
        .await?;
    Ok(v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| matches!(x["content"].as_str(), Some("movies" | "movie")))
        .take(40)
        .map(|x| Entry {
            id: text(x, "id"),
            title: text(x, "title"),
            detail: format!("{} · Dramachi", text(x, "year")),
            ..Default::default()
        })
        .filter(|e| !e.id.is_empty())
        .collect())
}
fn tmdb_title_hint(query: &str) -> Option<String> {
    let u = Url::parse(query.trim()).ok()?;
    let segment = u.path_segments()?.nth(1)?;
    let slug = segment.split_once('-')?.1;
    if slug.is_empty() || !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    Some(
        slug.split('-')
            .filter(|s| !s.is_empty())
            .map(|word| {
                let mut chars = word.chars();
                let first = chars.next().unwrap();
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}
pub fn tmdb_id(query: &str) -> Result<u64> {
    let raw = if query.trim().bytes().all(|b| b.is_ascii_digit()) {
        query.trim().to_owned()
    } else {
        let u = Url::parse(query.trim())
            .context("Paste a TMDB movie link or numeric movie ID for VixSrc")?;
        ensure!(
            matches!(u.host_str(), Some("www.themoviedb.org" | "themoviedb.org")),
            "Use a themoviedb.org movie link"
        );
        let mut parts = u.path_segments().context("Invalid TMDB movie link")?;
        ensure!(
            parts.next() == Some("movie"),
            "Use a TMDB movie link, not a series link"
        );
        parts
            .next()
            .unwrap_or("")
            .split('-')
            .next()
            .unwrap_or("")
            .into()
    };
    let id = raw.parse::<u64>().context("Invalid TMDB movie ID")?;
    ensure!(id > 0, "Invalid TMDB movie ID");
    Ok(id)
}
pub async fn movie_streams(c: &Client, mut e: Entry, provider: i32) -> Result<Streams> {
    if provider == 1 {
        let id = tmdb_id(&e.id)?;
        let origin = "https://vixsrc.to";
        let h = vec![
            ("Referer".into(), format!("{origin}/")),
            ("Origin".into(), origin.into()),
            ("User-Agent".into(), "Mozilla/5.0".into()),
        ];
        let v: Value = serde_json::from_slice(
            &c.bytes(&format!("{origin}/api/movie/{id}"), &h, META_LIMIT)
                .await?,
        )?;
        let u = Url::parse(origin)?.join(
            v["src"]
                .as_str()
                .context("VixSrc has no source for this movie")?,
        )?;
        ensure!(
            u.host_str() == Some("vixsrc.to"),
            "Unexpected movie embed host"
        );
        let html = String::from_utf8(c.bytes(u.as_str(), &h, META_LIMIT).await?)?;
        let title = {
            let doc = Html::parse_document(&html);
            doc.select(&selector("title"))
                .next()
                .map(|t| t.text().collect::<String>())
                .unwrap_or_default()
        };
        let title = title
            .trim()
            .trim_end_matches(" - VixSrc")
            .trim_end_matches(" | VixSrc")
            .trim();
        if title.chars().any(char::is_alphabetic)
            && title != "VixSrc"
            && !title.to_ascii_lowercase().starts_with("player")
        {
            e.title = title.chars().take(200).collect();
        }
        let config = html
            .split_once("window.masterPlaylist")
            .context("VixSrc player format changed")?
            .1
            .split_once("};")
            .context("VixSrc player format changed")?
            .0;
        let mut u = public_url(&js_field(config, "url").context("VixSrc playlist missing")?)?;
        ensure!(
            u.host_str() == Some("vixsrc.to"),
            "Unexpected playlist host"
        );
        if !u.path().ends_with(".m3u8") {
            u.set_path(&format!("{}.m3u8", u.path()));
        }
        u.query_pairs_mut()
            .append_pair(
                "token",
                &js_field(config, "token").context("Source token missing")?,
            )
            .append_pair(
                "expires",
                &js_field(config, "expires").context("Source expiry missing")?,
            )
            .append_pair("h", "1")
            .append_pair("lang", "en");
        let mut stream = link(
            u.into(),
            "Auto".into(),
            "VixSrc",
            Some(format!("{origin}/")),
        );
        stream.headers.origin = Some(origin.into());
        stream
            .headers
            .extra
            .insert("User-Agent".into(), "Mozilla/5.0".into());
        e.description =
            "Movie source: VixSrc. Stream availability can change independently of Mochune.".into();
        let manifest = String::from_utf8(c.bytes(&stream.url, &h, META_LIMIT).await?)?;
        ensure!(
            manifest.trim_start().starts_with("#EXTM3U"),
            "VixSrc returned an invalid playlist"
        );
        stream.subtitles = advertised_captions(&manifest, &stream.url);
        let mut streams = vec![stream.clone()];
        let mut heights = std::collections::BTreeSet::new();
        for line in manifest
            .lines()
            .filter(|l| l.starts_with("#EXT-X-STREAM-INF:"))
        {
            if let Some(height) = hls_height(line) {
                heights.insert(height);
            }
        }
        for height in heights.into_iter().rev() {
            let mut variant = stream.clone();
            variant.resolution = format!("{height}p");
            streams.push(variant);
        }
        return Ok(Streams { entry: e, streams });
    }
    let base = "https://api.nodeobjects.com/";
    let details = c
        .json(&endpoint(base, &[("interface", "title_v2"), ("id", &e.id)]))
        .await?;
    if let Some(album) = details["album"].as_array().and_then(|a| a.first()) {
        e.description = plain(&text(album, "storyline"));
    }
    let rip = details["seasons"]
        .as_object()
        .and_then(|o| {
            o.values().find_map(|g| {
                g["versions"]
                    .as_array()
                    .and_then(|v| v.first())
                    .and_then(|v| v["rip"].as_str())
            })
        })
        .unwrap_or("hd Rip");
    let files = c
        .json(&endpoint(
            base,
            &[("interface", "eplist"), ("season", rip), ("id", &e.id)],
        ))
        .await?;
    let mut streams = Vec::new();
    for file in files["episode_list"]
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
    {
        let v = c
            .json(&endpoint(
                base,
                &[
                    ("interface", "getFile"),
                    ("fid", &text(file, "fid")),
                    ("findex", &text(file, "disk")),
                ],
            ))
            .await?;
        let host = v["hostInfo"]["host"]
            .as_str()
            .context("Movie host missing")?;
        let f = v["fileInfo"]
            .as_array()
            .and_then(|a| a.first())
            .context("Movie file missing")?;
        let raw = f["url"].as_str().context("Movie URL missing")?;
        let u = format!(
            "https://{}/cdn/{}",
            host.trim_end_matches('/'),
            raw.trim_start_matches('/')
        );
        public_url(&u)?;
        let quality = if text(file, "quality").is_empty() {
            "Auto".into()
        } else {
            text(file, "quality")
        };
        streams.push(link(u, quality, "Dramachi", None));
    }
    ensure!(!streams.is_empty(), "This movie has no available stream");
    Ok(Streams { entry: e, streams })
}
fn js_field(config: &str, key: &str) -> Option<String> {
    for prefix in [format!("\"{key}\""), format!("'{key}'"), key.to_owned()] {
        if let Some((_, rest)) = config.split_once(&prefix) {
            let rest = rest.trim_start().strip_prefix(':')?.trim_start();
            let quote = rest.chars().next()?;
            if quote == '\'' || quote == '"' {
                return Some(rest[1..].split(quote).next()?.to_owned());
            }
        }
    }
    None
}

pub async fn manga_search(c: &Client, query: &str, provider: i32) -> Result<Vec<Entry>> {
    if provider == 0 {
        let v = c
            .json(&endpoint(
                "https://api.mangadex.org/manga",
                &[
                    ("title", query),
                    ("limit", "30"),
                    ("contentRating[]", "safe"),
                    ("availableTranslatedLanguage[]", "en"),
                ],
            ))
            .await?;
        return Ok(v["data"]
            .as_array()
            .into_iter()
            .flatten()
            .take(30)
            .map(|x| {
                let a = &x["attributes"];
                Entry {
                    id: text(x, "id"),
                    title: localized(&a["title"]),
                    description: plain(&localized(&a["description"])),
                    detail: "MangaDex · English".into(),
                }
            })
            .collect());
    }
    let html = c
        .text(&endpoint("https://mangapill.com/search", &[("q", query)]))
        .await?;
    let doc = Html::parse_document(&html);
    let mut seen = std::collections::HashSet::new();
    Ok(doc
        .select(&selector("a[href^='/manga/']"))
        .filter_map(|a| {
            let id = a.value().attr("href")?;
            let title = a.text().collect::<String>().trim().to_string();
            (!title.is_empty() && seen.insert(id.to_owned())).then(|| Entry {
                id: id.into(),
                title,
                detail: "MangaPill".into(),
                ..Default::default()
            })
        })
        .take(40)
        .collect())
}
fn localized(v: &Value) -> String {
    v["en"]
        .as_str()
        .or_else(|| {
            v.as_object()
                .and_then(|m| m.values().find_map(Value::as_str))
        })
        .unwrap_or("")
        .chars()
        .take(8192)
        .collect()
}
pub async fn chapters(
    c: &Client,
    e: &Entry,
    provider: i32,
    offset: usize,
) -> Result<(Vec<Entry>, bool)> {
    if provider == 0 {
        let off = offset.to_string();
        let v = c
            .json(&endpoint(
                &format!("https://api.mangadex.org/manga/{}/feed", e.id),
                &[
                    ("translatedLanguage[]", "en"),
                    ("limit", "100"),
                    ("offset", &off),
                    ("order[chapter]", "asc"),
                    ("includeFutureUpdates", "0"),
                ],
            ))
            .await?;
        let mut seen = std::collections::HashSet::new();
        let rows: Vec<_> = v["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["attributes"]["pages"].as_u64().unwrap_or(0) > 0)
            .filter_map(|v| {
                let a = &v["attributes"];
                let number = text(a, "chapter");
                seen.insert(number.clone()).then(|| Entry {
                    id: text(v, "id"),
                    title: format!("Chapter {number} {}", text(a, "title")),
                    detail: text(a, "scanlationGroup"),
                    ..Default::default()
                })
            })
            .collect();
        return Ok((
            rows,
            (offset + 100) < v["total"].as_u64().unwrap_or(0) as usize,
        ));
    }
    let url = Url::parse("https://mangapill.com")?.join(&e.id)?;
    ensure!(
        url.host_str() == Some("mangapill.com"),
        "Invalid manga link"
    );
    let html = c.text(url.as_str()).await?;
    let doc = Html::parse_document(&html);
    let mut rows: Vec<_> = doc
        .select(&selector("a.border[href^='/chapters/']"))
        .take(2000)
        .map(|a| Entry {
            id: a.value().attr("href").unwrap_or("").into(),
            title: a.text().collect::<String>().trim().into(),
            detail: String::new(),
            ..Default::default()
        })
        .collect();
    rows.reverse();
    Ok((rows, false))
}
pub async fn pages(c: &Client, e: Entry, provider: i32) -> Result<Pages> {
    if provider == 0 {
        let v = c
            .json(&format!("https://api.mangadex.org/at-home/server/{}", e.id))
            .await?;
        let base = v["baseUrl"]
            .as_str()
            .context("MangaDex image server unavailable")?;
        public_url(base)?;
        let hash = v["chapter"]["hash"]
            .as_str()
            .context("Manga chapter unavailable")?;
        let urls = v["chapter"]["dataSaver"]
            .as_array()
            .context("No manga pages available")?
            .iter()
            .take(400)
            .filter_map(Value::as_str)
            .map(|f| format!("{base}/data-saver/{hash}/{f}"))
            .collect();
        return Ok(Pages {
            entry: e,
            urls,
            referer: None,
        });
    }
    let url = Url::parse("https://mangapill.com")?.join(&e.id)?;
    ensure!(
        url.host_str() == Some("mangapill.com"),
        "Invalid chapter link"
    );
    let html = c.text(url.as_str()).await?;
    let doc = Html::parse_document(&html);
    let urls = doc
        .select(&selector(".js-page[data-src]"))
        .filter_map(|e| e.value().attr("data-src"))
        .take(400)
        .map(str::to_owned)
        .collect();
    Ok(Pages {
        entry: e,
        urls,
        referer: Some("https://mangapill.com/".into()),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tmdb_identity_does_not_accept_unrelated_urls() {
        assert_eq!(
            tmdb_id("https://www.themoviedb.org/movie/27205-inception").unwrap(),
            27205
        );
        assert!(tmdb_id("https://evil.org/movie/27205").is_err());
        assert!(tmdb_id("0").is_err());
    }
    #[test]
    fn source_urls_reject_local_targets() {
        for u in [
            "file:///etc/passwd",
            "http://example.com/a",
            "https://127.0.0.1/a",
            "https://192.168.1.1/a",
            "https://[::1]/a",
        ] {
            assert!(public_url(u).is_err());
        }
    }
    #[test]
    fn player_fields_are_not_taken_from_unrelated_text() {
        assert_eq!(
            js_field("{url: 'https://vixsrc.to/a', token: 'abc'}", "token").as_deref(),
            Some("abc")
        );
    }
}

pub(super) fn hls_height(line: &str) -> Option<u32> {
    line.split("RESOLUTION=")
        .nth(1)?
        .split(',')
        .next()?
        .split('x')
        .nth(1)?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod advertised_tests {
    use super::*;
    #[test]
    fn subtitle_renditions_include_playlist_tracks_without_guessing() {
        let text =
            "#EXTM3U\n#EXT-X-MEDIA:TYPE=SUBTITLES,NAME=\"English [CC]\",URI=\"/subs/playlist\"\n";
        let tracks = advertised_captions(text, "https://example.org/master.m3u8");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].label, "English [CC]");
        assert_eq!(tracks[0].url, "https://example.org/subs/playlist");
    }
}
