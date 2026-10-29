//! Experimental appearance-only music skins. One worker, one queued request/result,
//! lazy GIF frames, no downloaded assets, layout or playback callbacks in skin data.
use crate::{MusicSkin, SetupWindow};
mod catalog;
mod fonts;
use anyhow::{Context, Result, bail, ensure};
use image::{AnimationDecoder, ImageDecoder};
use serde::Deserialize;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    io::Cursor,
    path::{Component, Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
const SLOTS: [&str; 20] = [
    "panel",
    "card",
    "artwork_frame",
    "track",
    "fill",
    "thumb",
    "play",
    "pause",
    "previous",
    "next",
    "shuffle",
    "repeat",
    "volume",
    "muted",
    "heart",
    "heart_filled",
    "plus",
    "compact",
    "captions",
    "button",
];
const COLORS: [&str; 12] = [
    "panel",
    "surface",
    "text",
    "muted",
    "accent",
    "accent_text",
    "border",
    "track",
    "played",
    "button",
    "button_hover",
    "liked",
];
static FRAMES: AtomicU64 = AtomicU64::new(0);
static BUDGET: AtomicUsize = AtomicUsize::new(0);
static ANIMATING: AtomicBool = AtomicBool::new(false);
pub(crate) fn themes_folder() -> PathBuf {
    catalog::root()
}
pub(crate) fn stats() -> serde_json::Value {
    serde_json::json!({"frames_decoded":FRAMES.load(Ordering::Relaxed),"reserved_artwork_bytes":BUDGET.load(Ordering::Relaxed),"animation_active":ANIMATING.load(Ordering::Relaxed),"queue_capacity":1})
}
const WORKING_LIMIT: usize = 8 * 1024 * 1024;
const ENCODED_LIMIT: usize = 8 * 1024 * 1024;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    fonts: Vec<fonts::Font>,
    #[serde(skip)]
    prepared_fonts: Vec<fonts::Prepared>,
    #[serde(skip)]
    font_warnings: Vec<String>,
    version: u32,
    name: String,
    palette: BTreeMap<String, String>,
    #[serde(default)]
    style: Style,
    #[serde(default)]
    assets: BTreeMap<String, Asset>,
    #[serde(default)]
    sections: BTreeMap<String, crate::music_theme::Section>,
    #[serde(default)]
    theme_assets: BTreeMap<String, Asset>,
}
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Style {
    radius: f32,
    border_width: f32,
    font_family: String,
    title_size: f32,
    icon_scale: f32,
    pixelated: bool,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            radius: 12.,
            border_width: 0.,
            font_family: "DejaVu Sans".into(),
            title_size: 17.,
            icon_scale: 1.,
            pixelated: false,
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Asset {
    file: String,
    #[serde(default = "contain")]
    fit: String,
}
fn contain() -> String {
    "contain".into()
}
#[derive(Clone)]
enum Source {
    Default,
    File(PathBuf),
    DefaultCard,
}
pub(crate) fn color(s: &str) -> Result<slint::Color> {
    let hex = s
        .strip_prefix('#')
        .context("Colors must be #RRGGBB or #RRGGBBAA")?;
    ensure!(
        hex.is_ascii() && (hex.len() == 6 || hex.len() == 8),
        "Colors must be #RRGGBB or #RRGGBBAA"
    );
    let n = u32::from_str_radix(hex, 16).context("Invalid color")?;
    Ok(if hex.len() == 6 {
        slint::Color::from_rgb_u8((n >> 16) as u8, (n >> 8) as u8, n as u8)
    } else {
        slint::Color::from_argb_u8(n as u8, (n >> 24) as u8, (n >> 16) as u8, (n >> 8) as u8)
    })
}
fn validate_asset(a: &Asset) -> Result<()> {
    ensure!(
        ["contain", "cover", "fill"].contains(&a.fit.as_str()),
        "Unknown artwork fit"
    );
    let p = Path::new(&a.file);
    ensure!(
        !a.file.is_empty()
            && a.file.len() <= 240
            && !a.file.contains(['\\', ':'])
            && p.components().all(|p| matches!(p, Component::Normal(_))),
        "Artwork paths must be relative files inside the skin folder"
    );
    Ok(())
}
fn config(bytes: &[u8]) -> Result<Config> {
    ensure!(bytes.len() <= 64 * 1024, "Skin JSON exceeds 64 KiB");
    let c: Config = serde_json::from_slice(bytes).context("Invalid skin JSON")?;
    ensure!(
        matches!(c.version, 1 | 2),
        "Unsupported skin version (expected 1 or 2)"
    );
    ensure!(
        c.version == 2 || (c.sections.is_empty() && c.theme_assets.is_empty()),
        "Sections require version 2"
    );
    crate::music_theme::validate(&c.sections, &c.theme_assets)?;
    fonts::declarations(&c.fonts)?;
    let valid_family = |name: &str| -> Result<()> {
        ensure!(
            crate::music_theme::FONT_FAMILIES.contains(&name)
                || c.fonts.iter().any(|font| font.family == name),
            "Font family must be built-in or declared in fonts: {name}"
        );
        Ok(())
    };
    valid_family(&c.style.font_family)?;
    for section in c.sections.values() {
        if let Some(f) = &section.defaults.font_family {
            valid_family(f)?;
        }
        for component in section.components.values() {
            if let Some(f) = &component.paint.font_family {
                valid_family(f)?;
            }
            for paint in component.states.values() {
                if let Some(f) = &paint.font_family {
                    valid_family(f)?;
                }
            }
        }
    }
    for name in c.theme_assets.keys() {
        ensure!(
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Invalid named asset identifier"
        );
    }
    ensure!(
        !c.name.trim().is_empty()
            && c.name.chars().count() <= 80
            && !c.name.chars().any(char::is_control),
        "Skin name must contain 1–80 printable characters"
    );
    ensure!(
        c.palette.len() == COLORS.len() && COLORS.iter().all(|k| c.palette.contains_key(*k)),
        "Palette must contain exactly: {}",
        COLORS.join(", ")
    );
    for v in c.palette.values() {
        color(v)?;
    }
    for (label, n, min, max) in [
        ("radius", c.style.radius, 0., 16.),
        ("border_width", c.style.border_width, 0., 2.),
        ("title_size", c.style.title_size, 14., 19.),
        ("icon_scale", c.style.icon_scale, 0.8, 1.15),
    ] {
        ensure!(
            n.is_finite() && (min..=max).contains(&n),
            "{label} must be between {min} and {max}"
        );
    }
    for (slot, a) in c.assets.iter().chain(c.theme_assets.iter()) {
        ensure!(
            !c.assets.contains_key(slot) || SLOTS.contains(&slot.as_str()),
            "Unknown artwork slot: {slot}"
        );
        validate_asset(a)?;
    }
    ensure!(
        c.assets.keys().all(|s| SLOTS.contains(&s.as_str())),
        "Unknown legacy artwork slot"
    );
    Ok(c)
}

/// Offline authoring report: the same loader and limits as the live UI. No
/// playback, account, network, renderer, file writes or frame cache involved.
pub fn validate_file(path: &Path) -> Result<serde_json::Value> {
    let source = Source::File(path.to_path_buf());
    let (config, frames, animations, memory) = load(&source)?;
    let c = config.context("Missing theme configuration")?;
    let mut reports = Vec::new();
    let mut seen = BTreeMap::new();
    for (slot, asset) in asset_entries(&c) {
        let name = if slot < SLOTS.len() {
            SLOTS[slot].to_string()
        } else {
            c.theme_assets
                .keys()
                .nth(slot - SLOTS.len())
                .unwrap()
                .clone()
        };
        if let Some(primary) = seen.get(&asset.file) {
            reports.push(serde_json::json!({"slot":name,"file":asset.file,"shared_with":primary,"reserved_bytes":0}));
            continue;
        }
        let frame = frames
            .iter()
            .find(|f| f.slot == slot)
            .context("Missing decoded asset")?;
        let bytes = asset_bytes(&source, &asset.file)?;
        let gif = image::guess_format(&bytes)? == image::ImageFormat::Gif;
        let (width, height) = frame.image.dimensions();
        reports.push(serde_json::json!({"slot":name,"file":asset.file,"width":width,"height":height,"encoded_bytes":bytes.len(),"reserved_bytes":width as usize*height as usize*4*if gif {8}else{2},"frames":if gif {gif_structure(&bytes,width,height)?}else{1}}));
        seen.insert(asset.file.clone(), name);
    }
    Ok(
        serde_json::json!({"valid":true,"version":c.version,"name":c.name,"sections":if c.version==1 {vec!["player"]}else{c.sections.keys().map(String::as_str).collect()},"reserved_artwork_bytes":memory,"working_limit_bytes":WORKING_LIMIT,"active_animations":animations.len(),"assets":reports,"font_faces_ready":c.prepared_fonts.len(),"font_warnings":c.font_warnings,"warnings":["Artwork reservation is not a total process-RAM guarantee.","Animation redraw cost remains experimental; measure on the target platform."]}),
    )
}
#[cfg(test)]
fn embedded(kind: usize, file: &str) -> Result<&'static [u8]> {
    macro_rules! files {
        ($dir:literal) => {
            match file {
                "skin.json" => include_bytes!("music_skin/fixtures/skin.json").as_slice(),
                "panel.png" => include_bytes!(concat!("../themes/", $dir, "/panel.png")).as_slice(),
                "frame.png" => include_bytes!(concat!("../themes/", $dir, "/frame.png")).as_slice(),
                "track.png" => include_bytes!(concat!("../themes/", $dir, "/track.png")).as_slice(),
                "fill.png" => include_bytes!(concat!("../themes/", $dir, "/fill.png")).as_slice(),
                "thumb.png" => include_bytes!(concat!("../themes/", $dir, "/thumb.png")).as_slice(),
                "mochune.gif" => {
                    include_bytes!(concat!("../themes/", $dir, "/mochune.gif")).as_slice()
                }
                _ => bail!("Unknown bundled asset: {file}"),
            }
        };
    }
    Ok(if kind == 1 {
        files!("crt-full")
    } else {
        files!("pixelduck")
    })
}
fn limited_read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    ensure!(
        std::fs::metadata(path)?.is_file(),
        "Skin resources must be regular files"
    );
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut data)?;
    ensure!(
        data.len() <= limit,
        "{} exceeds its file-size limit",
        path.display()
    );
    Ok(data)
}
fn asset_bytes(source: &Source, file: &str) -> Result<Arc<[u8]>> {
    Ok(match source {
        Source::File(path) => {
            let root = path
                .parent()
                .context("Skin directory missing")?
                .canonicalize()?;
            let candidate = root.join(file).canonicalize()?;
            ensure!(
                candidate.starts_with(&root),
                "Artwork escapes the skin directory"
            );
            limited_read(&candidate, 2 * 1024 * 1024)?.into()
        }
        Source::Default | Source::DefaultCard => bail!("No skin selected"),
    })
}
fn limits() -> image::Limits {
    let mut l = image::Limits::default();
    l.max_image_width = Some(1024);
    l.max_image_height = Some(1024);
    l.max_alloc = Some(WORKING_LIMIT as u64);
    l
}
struct Raster {
    slot: usize,
    image: image::RgbaImage,
}
struct Gif {
    slot: usize,
    bytes: Arc<[u8]>,
    frames: image::Frames<'static>,
    due: Instant,
    count: usize,
    pending: Option<image::Frame>,
    pixels: u64,
}
// Validate frame rectangles and counts without decompressing the animation.
// A small logical canvas must not hide a huge local frame allocation/workload.
fn gif_structure(data: &[u8], width: u32, height: u32) -> Result<usize> {
    fn take<'a>(data: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
        let end = at.checked_add(n).context("GIF offset overflow")?;
        let bytes = data.get(*at..end).context("Truncated GIF structure")?;
        *at = end;
        Ok(bytes)
    }
    fn blocks(data: &[u8], at: &mut usize) -> Result<()> {
        loop {
            let n = take(data, at, 1)?[0] as usize;
            if n == 0 {
                return Ok(());
            }
            take(data, at, n)?;
        }
    }
    fn table(data: &[u8], at: &mut usize, packed: u8) -> Result<()> {
        if packed & 0x80 != 0 {
            take(data, at, 3 * (1usize << ((packed & 7) + 1)))?;
        }
        Ok(())
    }
    let mut at = 0;
    let header = take(data, &mut at, 13)?;
    ensure!(
        &header[..6] == b"GIF89a" || &header[..6] == b"GIF87a",
        "Invalid GIF signature"
    );
    table(data, &mut at, header[10])?;
    let mut frames = 0;
    loop {
        match take(data, &mut at, 1)?[0] {
            0x21 => {
                take(data, &mut at, 1)?;
                blocks(data, &mut at)?;
            }
            0x2c => {
                let d = take(data, &mut at, 9)?;
                let u = |i| u32::from(u16::from_le_bytes([d[i], d[i + 1]]));
                let (x, y, w, h) = (u(0), u(2), u(4), u(6));
                ensure!(
                    w > 0 && h > 0 && x + w <= width && y + h <= height,
                    "GIF frame exceeds its logical canvas"
                );
                table(data, &mut at, d[8])?;
                let code = take(data, &mut at, 1)?[0];
                ensure!((1..=8).contains(&code), "Invalid GIF code size");
                blocks(data, &mut at)?;
                frames += 1;
                ensure!(frames <= 300, "GIF exceeds 300 frames per loop");
            }
            0x3b => {
                ensure!(frames > 0, "GIF has no image frames");
                return Ok(frames);
            }
            _ => bail!("Invalid GIF record"),
        }
    }
}
fn gif_frames(bytes: Arc<[u8]>) -> Result<image::Frames<'static>> {
    let mut d = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
    d.set_limits(limits())?;
    Ok(d.into_frames())
}
fn delay(f: &image::Frame) -> Duration {
    let (n, d) = f.delay().numer_denom_ms();
    Duration::from_millis((u64::from(n) / u64::from(d.max(1))).clamp(67, 5000))
}
impl Gif {
    fn next(&mut self) -> Result<Raster> {
        let cached = self.pending.take();
        let was_cached = cached.is_some();
        let mut frame = if let Some(frame) = cached {
            Some(Ok(frame))
        } else {
            self.frames.next()
        };
        if frame.is_none() {
            self.frames = gif_frames(self.bytes.clone())?;
            self.count = 0;
            frame = self.frames.next();
        }
        let f = frame.context("GIF has no frames")??;
        self.count += 1;
        ensure!(self.count <= 300, "GIF exceeds 300 frames");
        let now = Instant::now();
        self.due = if now.saturating_duration_since(self.due) > Duration::from_millis(250) {
            now
        } else {
            self.due
        } + delay(&f);
        if !was_cached {
            FRAMES.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Raster {
            slot: self.slot,
            image: f.into_buffer(),
        })
    }
}
fn check_dimensions(slot: usize, w: u32, h: u32, animation: bool) -> Result<()> {
    let (maxw, maxh) = if animation {
        (256, 256)
    } else if slot == 3 || slot == 4 {
        (1024, 64)
    } else if slot <= 2 || slot >= SLOTS.len() {
        (512, 512)
    } else {
        (128, 128)
    };
    ensure!(
        w > 0 && h > 0 && w <= maxw && h <= maxh,
        "{}: {w}×{h} exceeds {maxw}×{maxh}",
        SLOTS.get(slot).copied().unwrap_or("section asset")
    );
    Ok(())
}
fn aliases(c: &Config) -> Vec<usize> {
    let mut result: Vec<_> = (0..SLOTS.len() + c.theme_assets.len()).collect();
    let mut seen = BTreeMap::new();
    for (slot, a) in asset_entries(c) {
        result[slot] = *seen.entry(a.file.as_str()).or_insert(slot);
    }
    result
}
fn asset_entries(c: &Config) -> Vec<(usize, &Asset)> {
    c.assets
        .iter()
        .map(|(n, a)| (SLOTS.iter().position(|s| s == n).unwrap(), a))
        .chain(
            c.theme_assets
                .values()
                .enumerate()
                .map(|(i, a)| (SLOTS.len() + i, a)),
        )
        .collect()
}
fn default_source(w: &SetupWindow) -> Source {
    if w.get_music_layout() && !w.get_mini_music() && w.get_skin_card_visible() {
        Source::DefaultCard
    } else {
        Source::Default
    }
}
fn load_default_card() -> Result<(Option<Config>, Vec<Raster>, Vec<Gif>, usize)> {
    let bytes: Arc<[u8]> = Arc::from(include_bytes!("../ui/assets/mochune-card.gif").as_slice());
    ensure!(
        bytes.len() <= ENCODED_LIMIT,
        "Default card exceeds artwork budget"
    );
    let (w, h) = image::ImageReader::new(Cursor::new(bytes.clone()))
        .with_guessed_format()?
        .into_dimensions()?;
    check_dimensions(1, w, h, true)?;
    gif_structure(&bytes, w, h)?;
    let memory = w as usize * h as usize * 4 * 8;
    ensure!(
        memory <= WORKING_LIMIT,
        "Default card exceeds working budget"
    );
    let mut gif = Gif {
        slot: 1,
        bytes: bytes.clone(),
        frames: gif_frames(bytes)?,
        due: Instant::now(),
        count: 0,
        pending: None,
        pixels: u64::from(w) * u64::from(h),
    };
    let first = gif.next()?;
    let mut animations = Vec::new();
    if let Some(frame) = gif.frames.next() {
        gif.pending = Some(frame?);
        FRAMES.fetch_add(1, Ordering::Relaxed);
        animations.push(gif);
    }
    Ok((None, vec![first], animations, memory))
}
fn load(source: &Source) -> Result<(Option<Config>, Vec<Raster>, Vec<Gif>, usize)> {
    if let Source::File(path) = source {
        catalog::confined(path, &catalog::root())?;
    }
    load_unconfined(source)
}
// Private decoding helper for isolated safety tests and the already-confined
// catalog scan. All user-facing entry points call load().
fn load_unconfined(source: &Source) -> Result<(Option<Config>, Vec<Raster>, Vec<Gif>, usize)> {
    if matches!(source, Source::DefaultCard) {
        return load_default_card();
    }
    if matches!(source, Source::Default) {
        return Ok((None, Vec::new(), Vec::new(), 0));
    }
    let bytes = match source {
        Source::File(p) => limited_read(p, 64 * 1024)?,
        Source::Default | Source::DefaultCard => unreachable!(),
    };
    let mut c = config(&bytes)?;
    fonts::prepare(&mut c, source)?;
    let mut first = Vec::new();
    let mut gifs = Vec::new();
    let (mut memory, mut encoded) = (0usize, 0usize);
    let mut seen = BTreeMap::<String, (u32, u32, bool)>::new();
    for (slot, a) in asset_entries(&c) {
        if let Some(&(w, h, animation)) = seen.get(&a.file) {
            check_dimensions(slot, w, h, animation)?;
            continue;
        }
        let bytes = asset_bytes(source, &a.file)?;
        encoded += bytes.len();
        ensure!(
            encoded <= ENCODED_LIMIT,
            "Combined encoded artwork exceeds 8 MiB"
        );
        let format = image::guess_format(&bytes).context("Unrecognized artwork format")?;
        ensure!(
            matches!(
                format,
                image::ImageFormat::Png
                    | image::ImageFormat::Jpeg
                    | image::ImageFormat::WebP
                    | image::ImageFormat::Gif
            ),
            "Use PNG, JPEG, WebP or GIF artwork"
        );
        let (w, h) = image::ImageReader::new(Cursor::new(bytes.clone()))
            .with_guessed_format()?
            .into_dimensions()?;
        let animation = format == image::ImageFormat::Gif;
        check_dimensions(slot, w, h, animation)?;
        if animation {
            gif_structure(&bytes, w, h)?;
        }
        seen.insert(a.file.clone(), (w, h, animation));
        // Reserve for decoder compositing, UI publication and retained renderer images.
        memory += (w as usize) * (h as usize) * 4 * if animation { 8 } else { 2 };
        ensure!(
            memory <= WORKING_LIMIT,
            "Decoded artwork working budget exceeds 8 MiB"
        );
        if animation {
            let frames = gif_frames(bytes.clone())?;
            let mut gif = Gif {
                slot,
                bytes,
                frames,
                due: Instant::now(),
                count: 0,
                pending: None,
                pixels: u64::from(w) * u64::from(h),
            };
            first.push(gif.next()?);
            // Probe one additional frame: a one-frame GIF is static and never
            // starts an animation timer. Keep that frame instead of decoding it twice.
            if let Some(frame) = gif.frames.next() {
                gif.pending = Some(frame?);
                FRAMES.fetch_add(1, Ordering::Relaxed);
                gifs.push(gif);
            }
        } else {
            let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
            reader.limits(limits());
            first.push(Raster {
                slot,
                image: reader.decode()?.into_rgba8(),
            });
        }
    }
    Ok((Some(c), first, gifs, memory))
}
enum Command {
    Scan,
    Load(u64, Source),
    Tick(u64, u64),
    Stop,
}
enum Reply {
    Catalog(Result<Vec<catalog::Entry>, String>),
    Loaded(
        u64,
        Result<(Option<Config>, Vec<Raster>, u64, usize), String>,
    ),
    Frames(u64, Vec<Raster>),
    Error(u64, String),
}
fn reply(tx: &mpsc::SyncSender<Reply>, stop: &AtomicBool, mut message: Reply) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match tx.try_send(message) {
            Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => return,
            Err(mpsc::TrySendError::Full(value)) => {
                message = value;
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}
fn work(rx: mpsc::Receiver<Command>, tx: mpsc::SyncSender<Reply>, stop: Arc<AtomicBool>) {
    let mut gifs = Vec::<Gif>::new();
    let mut epoch = 0;
    let mut tick = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let Ok(command) = rx.recv() else {
            break;
        };
        match command {
            Command::Scan => {
                let result = catalog::scan(&catalog::root(), &stop).map_err(|e| format!("{e:#}"));
                reply(&tx, &stop, Reply::Catalog(result));
            }
            Command::Stop => break,
            Command::Load(n, source) => {
                let result = load(&source)
                    .map(|(c, frames, g, m)| {
                        let animated = g.iter().fold(0u64, |mask, gif| mask | (1 << gif.slot));
                        gifs = g;
                        epoch = n;
                        (c, frames, animated, m)
                    })
                    .map_err(|e| format!("{e:#}"));
                reply(&tx, &stop, Reply::Loaded(n, result));
            }
            Command::Tick(n, visible) if n == epoch => {
                let pixels = gifs
                    .iter()
                    .filter(|g| visible & (1 << g.slot) != 0)
                    .map(|g| g.pixels)
                    .sum::<u64>();
                let cadence = Duration::from_millis((pixels * 1000 / 2_000_000).max(67));
                if tick.elapsed() < cadence {
                    continue;
                }
                tick = Instant::now();
                let mut frames = Vec::new();
                let mut error = None;
                for g in &mut gifs {
                    if visible & (1 << g.slot) != 0 && Instant::now() >= g.due {
                        match g.next() {
                            Ok(f) => frames.push(f),
                            Err(e) => {
                                error = Some(format!(
                                    "{}: {e}",
                                    SLOTS.get(g.slot).copied().unwrap_or("section asset")
                                ));
                                break;
                            }
                        }
                    }
                }
                if let Some(e) = error {
                    gifs.clear();
                    reply(&tx, &stop, Reply::Error(n, e));
                } else if !frames.is_empty() {
                    reply(&tx, &stop, Reply::Frames(n, frames));
                }
            }
            _ => {}
        }
    }
}
fn publish(model: &Rc<VecModel<slint::Image>>, frames: Vec<Raster>, aliases: &[usize]) {
    for Raster { slot, image } in frames {
        let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
            image.as_raw(),
            image.width(),
            image.height(),
        );
        let frame = slint::Image::from_rgba8(pixels);
        for (i, source) in aliases.iter().enumerate() {
            if *source == slot {
                model.set_row_data(i, frame.clone());
            }
        }
    }
}
#[cfg(any(target_os = "windows", feature = "native-overlay-test"))]
pub(crate) fn registered_font_bytes() -> Vec<&'static [u8]> {
    fonts::registered_bytes()
}
fn apply(w: &SetupWindow, mut c: Option<Config>, frames: Vec<Raster>, memory: usize) -> Vec<usize> {
    if let Some(c) = &mut c {
        fonts::register(w, c);
    }
    let aliases = c
        .as_ref()
        .map(aliases)
        .unwrap_or_else(|| (0..SLOTS.len()).collect());
    BUDGET.store(memory, Ordering::Relaxed);
    let skin = w.global::<MusicSkin>();
    let model = Rc::new(VecModel::from(vec![slint::Image::default(); aliases.len()]));
    publish(&model, frames, &aliases);
    let theme = w.global::<crate::MusicTheme>();
    let sections = c.as_ref().map(|c| c.sections.clone()).unwrap_or_default();
    let legacy_player = c
        .as_ref()
        .is_some_and(|c| c.version == 1 || !c.assets.is_empty());
    theme.set_defined(ModelRc::new(VecModel::from(
        crate::music_theme::SECTIONS
            .iter()
            .map(|s| sections.contains_key(*s))
            .collect::<Vec<_>>(),
    )));
    theme.set_available(ModelRc::new(VecModel::from(
        crate::music_theme::SECTIONS
            .iter()
            .map(|s| sections.contains_key(*s) || (*s == "player" && legacy_player))
            .collect::<Vec<_>>(),
    )));
    let names: Vec<_> = c
        .as_ref()
        .map(|c| c.theme_assets.keys().cloned().collect())
        .unwrap_or_default();
    let live_images = model.clone();
    theme.on_resolve(move |section, key, kind, state, _| {
        let Some(section) = usize::try_from(section)
            .ok()
            .and_then(|i| crate::music_theme::SECTIONS.get(i))
            .and_then(|n| sections.get(*n))
        else {
            return crate::ThemePaint::default();
        };
        let paint =
            crate::music_theme::resolve(section, key.as_str(), kind.as_str(), state.as_str());
        let mut images = BTreeMap::new();
        for name in [&paint.image, &paint.frame].into_iter().flatten() {
            if let Some(i) = names.iter().position(|n| n == name) {
                images.insert(
                    name.clone(),
                    live_images.row_data(SLOTS.len() + i).unwrap_or_default(),
                );
            }
        }
        crate::music_theme::ui_paint(paint, &images)
    });
    theme.set_revision(theme.get_revision().wrapping_add(1));
    skin.set_images(model.into());
    let Some(c) = c else {
        skin.set_loaded(false);
        skin.set_font_family("DejaVu Sans".into());
        skin.set_title_size(17.);
        skin.set_icon_scale(1.);
        skin.set_radius(12.);
        skin.set_border_width(0.);
        skin.set_pixelated(false);
        skin.set_fits(ModelRc::default());
        w.set_skin_status(
            if memory > 0 {
                "Mochune default · card animation"
            } else {
                "Mochune default · no skin animation work"
            }
            .into(),
        );
        crate::repaint_window(w.window());
        return aliases;
    };
    macro_rules! colors {($($key:literal=>$setter:ident),*)=>{$(skin.$setter(color(&c.palette[$key]).unwrap());)*};}
    colors!("panel"=>set_custom_panel,"surface"=>set_custom_surface,"text"=>set_custom_text,"muted"=>set_custom_muted,"accent"=>set_custom_accent,"accent_text"=>set_custom_accent_text,"border"=>set_custom_border,"track"=>set_custom_track,"played"=>set_custom_played,"button"=>set_custom_button,"button_hover"=>set_custom_button_hover,"liked"=>set_custom_liked);
    skin.set_radius(c.style.radius);
    skin.set_border_width(c.style.border_width);
    skin.set_font_family(c.style.font_family.into());
    skin.set_title_size(c.style.title_size);
    skin.set_icon_scale(c.style.icon_scale);
    skin.set_pixelated(c.style.pixelated);
    skin.set_fits(ModelRc::new(VecModel::from(
        SLOTS
            .iter()
            .map(|s| {
                c.assets
                    .get(*s)
                    .map_or("contain", |a| a.fit.as_str())
                    .into()
            })
            .collect::<Vec<slint::SharedString>>(),
    )));
    skin.set_loaded(c.version == 1 || !c.assets.is_empty() || c.sections.contains_key("player"));
    w.set_skin_status(
        format!(
            "{} · artwork budget {:.0} KiB / 8192 KiB{}",
            c.name,
            memory as f64 / 1024.,
            if c.font_warnings.is_empty() {
                String::new()
            } else {
                format!(" · {}", c.font_warnings.join("; "))
            }
        )
        .into(),
    );
    crate::repaint_window(w.window());
    aliases
}
pub struct Guard {
    timer: Rc<slint::Timer>,
    tx: mpsc::SyncSender<Command>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.timer.stop();
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.try_send(Command::Stop);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
pub fn install(w: &SetupWindow) -> Guard {
    let (tx, rx) = mpsc::sync_channel(1);
    let (out, events) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let worker = thread::Builder::new()
        .name("mochune-skin".into())
        .spawn(move || work(rx, out, flag))
        .expect("skin worker");
    let timer = Rc::new(slint::Timer::default());
    let epoch = Rc::new(Cell::new(0u64));
    let active = Rc::new(Cell::new(0u64));
    let pending = Rc::new(RefCell::new(None::<(u64, Source)>));
    let waiting = Rc::new(Cell::new(false));
    let catalog = Rc::new(RefCell::new(Vec::<catalog::Entry>::new()));
    let rebuilding = Rc::new(Cell::new(true));
    let scan_pending = Rc::new(Cell::new(true));
    let catalog_tick = catalog.clone();
    let rebuilding_tick = rebuilding.clone();
    let scan_tick = scan_pending.clone();
    let animated = Rc::new(Cell::new(0u64));
    let image_aliases = Rc::new(RefCell::new((0..SLOTS.len()).collect::<Vec<_>>()));
    let weak = w.as_weak();
    let pending_tick = pending.clone();
    let epoch_tick = epoch.clone();
    let active_tick = active.clone();
    let waiting_tick = waiting.clone();
    let animated_tick = animated.clone();
    let send = tx.clone();
    let timer_weak = Rc::downgrade(&timer);
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(67),
        move || {
            let Some(w) = weak.upgrade() else { return };
            while let Ok(reply) = events.try_recv() {
                match reply {
                    Reply::Catalog(result) => {
                        waiting_tick.set(false);
                        match result {
                            Ok(entries) => {
                                rebuilding_tick.set(true);
                                let path = PathBuf::from(w.get_music_skin_path().as_str());
                                let selected = path.canonicalize().ok().and_then(|p| entries.iter().position(|e| e.path == p));
                                let requested = !w.get_music_skin_path().is_empty();
                                let mut names = vec![slint::SharedString::from("Mochune default")];
                                names.extend(entries.iter().map(|e| slint::SharedString::from(format!("{} · {}", e.name, e.path.parent().and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy()))));
                                w.set_skin_names(ModelRc::new(VecModel::from(names)));
                                *catalog_tick.borrow_mut() = entries;
                                w.set_music_skin_choice(selected.map_or(0, |i| i as i32 + 1));
                                rebuilding_tick.set(false);
                                let source = selected.map(|i| Source::File(catalog_tick.borrow()[i].path.clone())).unwrap_or_else(|| default_source(&w));
                                if requested && selected.is_none() {
                                    w.set_skin_status("Saved/requested theme is not valid inside the themes folder; using default".into());
                                }
                                w.set_music_skin_path(selected.map(|i| catalog_tick.borrow()[i].path.to_string_lossy().into_owned()).unwrap_or_default().into());
                                let n = epoch_tick.get().wrapping_add(1);
                                epoch_tick.set(n);
                                *pending_tick.borrow_mut() = Some((n, source));
                            }
                            Err(error) => {
                                rebuilding_tick.set(false);
                                w.set_skin_loading(false);
                                w.set_skin_error(true);
                                w.set_skin_status(format!("Theme scan failed: {error}").into());
                            }
                        }
                    }
                    Reply::Loaded(n, result) if n == epoch_tick.get() => {
                        waiting_tick.set(false);
                        w.set_skin_loading(false);
                        match result {
                            Ok((c, frames, gifs, memory)) => {
                                active_tick.set(n);
                                animated_tick.set(gifs);
                                *image_aliases.borrow_mut() = apply(&w, c, frames, memory);
                                w.set_skin_error(false);
                            }
                            Err(e) => {
                                w.set_skin_error(true);
                                w.set_skin_status(format!("Skin not applied: {e}").into());
                            }
                        }
                    }
                    Reply::Loaded(_, _) => waiting_tick.set(false),
                    Reply::Frames(n, frames) if n == active_tick.get() => {
                        let named_changed=frames.iter().any(|f| image_aliases.borrow().iter().skip(SLOTS.len()).any(|slot|*slot==f.slot));
                        let images = w.global::<MusicSkin>().get_images();
                        if let Some(model) =
                            images.as_any().downcast_ref::<VecModel<slint::Image>>()
                        {
                            for Raster { slot, image } in frames {
                                let p =
                                    slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                        image.as_raw(),
                                        image.width(),
                                        image.height(),
                                    );
                                let frame = slint::Image::from_rgba8(p);
                                for (i, source) in image_aliases.borrow().iter().enumerate() {
                                    if *source == slot {
                                        model.set_row_data(i, frame.clone());
                                    }
                                }
                            }
                        }
                        if named_changed { let theme=w.global::<crate::MusicTheme>();theme.set_revision(theme.get_revision().wrapping_add(1)); }
                    }
                    Reply::Error(n, e) if n == active_tick.get() => {
                        animated_tick.set(0);
                        w.set_skin_error(true);
                        w.set_skin_status(format!("Animation stopped: {e}").into());
                    }
                    _ => {}
                }
            }
            if scan_tick.get() && !waiting_tick.get() {
                if send.try_send(Command::Scan).is_ok() {
                    scan_tick.set(false);
                    waiting_tick.set(true);
                }
            }
            let request = if waiting_tick.get() { None } else { pending_tick.borrow_mut().take() };
            if let Some((n, source)) = request {
                match send.try_send(Command::Load(n, source)) {
                    Ok(()) => waiting_tick.set(true),
                    Err(mpsc::TrySendError::Full(Command::Load(n, s))) => {
                        *pending_tick.borrow_mut() = Some((n, s))
                    }
                    _ => {}
                }
            }
            let preview = w.get_skin_preview_visible() && w.get_music_skin_choice() > 0;
            let video = !w.get_music_layout() && w.get_theme_video() && w.global::<crate::MusicTheme>().get_defined().row_data(4).unwrap_or(false);
            let visible = w.get_skin_window_active() && (((w.get_theme_player() || w.get_music_skin_path().is_empty()) && w.get_music_layout() && !w.get_settings_open()) || (video && !w.get_settings_open()) || preview);
            let mut mask = if preview {
                (1 << 0) | (1 << 1) | (1 << 6) | (1 << 14)
            } else {
                if video { ((1u64 << image_aliases.borrow().len()) - 1) & !((1u64 << SLOTS.len()) - 1) } else { (1u64 << image_aliases.borrow().len()) - 1 }
            };
            if !preview {
                if !w.get_skin_card_visible() {
                    mask &= !(1 << 1);
                }
                if w.get_resolving() {
                    mask &= !((1 << 6) | (1 << 7));
                } else if w.get_paused() || w.get_position() <= 0. {
                    mask &= !(1 << 7);
                } else {
                    mask &= !(1 << 6);
                }
                if w.get_volume() > 0. {
                    mask &= !(1 << 13);
                } else {
                    mask &= !(1 << 12);
                }
                if w.invoke_song_liked(w.get_selected_id(), w.get_likes_revision()) {
                    mask &= !(1 << 14);
                } else {
                    mask &= !(1 << 15);
                }
                if w.get_compact_music() {
                    mask &= !(1 << 17);
                }
                if w.get_mini_music() {
                    // The strip uses static artwork plus the shared transport only.
                    mask &= (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7) | (1 << 8) | (1 << 9)
                        | (1 << 10) | (1 << 11) | (1 << 12) | (1 << 13)
                        | (1 << 18) | (1 << 19);
                }
            }
            let mask = image_aliases
                .borrow()
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .fold(0, |mask, (_, source)| mask | (1 << source));
            let eligible = visible && w.get_skin_animations() && (mask & animated_tick.get() != 0);
            ANIMATING.store(eligible, Ordering::Relaxed);
            if eligible && !waiting_tick.get() {
                let _ = send.try_send(Command::Tick(active_tick.get(), mask));
            }
            if pending_tick.borrow().is_none() && !waiting_tick.get() && !eligible {
                if let Some(t) = timer_weak.upgrade() {
                    t.stop();
                }
            }
        },
    );
    timer.stop();
    let choose = {
        let weak = w.as_weak();
        let pending = pending.clone();
        let epoch = epoch.clone();
        let timer = timer.clone();
        let catalog = catalog.clone();
        let rebuilding = rebuilding.clone();
        move |use_saved_path: bool| {
            if rebuilding.get() {
                return;
            }
            if let Some(w) = weak.upgrade() {
                let entries = catalog.borrow();
                let index = if use_saved_path {
                    PathBuf::from(w.get_music_skin_path().as_str())
                        .canonicalize()
                        .ok()
                        .and_then(|p| entries.iter().position(|e| e.path == p))
                } else {
                    usize::try_from(w.get_music_skin_choice() - 1)
                        .ok()
                        .filter(|i| *i < entries.len())
                };
                let source = index
                    .map(|i| Source::File(entries[i].path.clone()))
                    .unwrap_or_else(|| default_source(&w));
                rebuilding.set(true);
                w.set_music_skin_choice(index.map_or(0, |i| i as i32 + 1));
                w.set_music_skin_path(
                    index
                        .map(|i| entries[i].path.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into(),
                );
                rebuilding.set(false);
                w.set_skin_loading(true);
                w.set_skin_error(false);
                w.set_skin_status("Loading theme…".into());
                let n = epoch.get().wrapping_add(1);
                epoch.set(n);
                *pending.borrow_mut() = Some((n, source));
                timer.restart();
            }
        }
    };
    let choose = Rc::new(choose);
    let select = choose.clone();
    w.on_skin_change(move || select(false));
    let select = choose.clone();
    w.on_skin_reload(move || select(true));
    let t = timer.clone();
    let weak = w.as_weak();
    let select = choose.clone();
    let last_card = Cell::new(false);
    w.on_skin_visibility_change(move || {
        if let Some(w) = weak.upgrade() {
            let visible = matches!(default_source(&w), Source::DefaultCard);
            if visible != last_card.replace(visible) {
                if w.get_music_skin_path().is_empty() {
                    select(true);
                }
            }
        }
        t.restart();
    });
    let weak = w.as_weak();
    let t = timer.clone();
    w.on_skin_open(move || {
        if let Some(w) = weak.upgrade() {
            w.set_skin_loading(true);
            w.set_skin_status("Scanning themes folder…".into());
            scan_pending.set(true);
            t.restart();
        }
    });
    if let Ok(value) = std::env::var("MOCHUNE_MUSIC_SKIN") {
        w.set_music_skin_path(
            if value == "default" {
                String::new()
            } else if Path::new(&value).components().count() == 1 {
                catalog::root()
                    .join(value)
                    .join("skin.json")
                    .to_string_lossy()
                    .into_owned()
            } else {
                value
            }
            .into(),
        );
    } else if w.get_music_skin_path().is_empty() {
        // Migrate old numeric presets once; new selections persist their path.
        let folder = match w.get_music_skin_choice() {
            1 => Some("pastelduck"),
            2 => Some("pixelduck"),
            _ => None,
        };
        if let Some(folder) = folder {
            w.set_music_skin_path(
                catalog::root()
                    .join(folder)
                    .join("skin.json")
                    .to_string_lossy()
                    .into_owned()
                    .into(),
            );
        }
    }
    w.set_skin_loading(true);
    w.set_skin_status("Scanning themes folder…".into());
    timer.restart();
    Guard {
        timer,
        tx,
        stop,
        worker: Some(worker),
    }
}
#[cfg(any(feature = "native-overlay-test", feature = "showcase"))]
pub(crate) fn preview_file(w: &SetupWindow, path: &Path) -> Result<usize> {
    let source = if path == Path::new("default") {
        Source::DefaultCard
    } else {
        Source::File(path.to_path_buf())
    };
    let (c, mut frames, mut gifs, memory) = load(&source)?;
    let step = std::env::var("MOCHUNE_SHOWCASE_STEP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .min(300);
    for gif in &mut gifs {
        for _ in 0..step {
            let frame = gif.next()?;
            if let Some(current) = frames.iter_mut().find(|f| f.slot == gif.slot) {
                *current = frame;
            }
        }
    }
    apply(w, c, frames, memory);
    Ok(memory)
}
#[cfg(feature = "native-overlay-test")]
pub(crate) fn preview(w: &SetupWindow, kind: usize, step: usize) -> Result<usize> {
    let source = if kind == 0 {
        Source::Default
    } else {
        Source::File(
            catalog::root()
                .join(if kind == 1 { "pastelduck" } else { "pixelduck" })
                .join("skin.json"),
        )
    };
    let (c, mut frames, mut gifs, memory) = load(&source)?;
    for gif in &mut gifs {
        for _ in 0..step {
            let f = gif.next()?;
            if let Some(current) = frames.iter_mut().find(|f| f.slot == gif.slot) {
                *current = f;
            }
        }
    }
    apply(w, c, frames, memory);
    Ok(memory)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_themes_load_and_v1_remains_player_only() {
        for name in [
            "reference/skin.json",
            "crt-full/skin.json",
            "pixelduck/skin.json",
            "crt-full/skin.json",
        ] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("themes")
                .join(name);
            let report = validate_file(&path).unwrap();
            assert_eq!(report["valid"], true);
            assert!(report["reserved_artwork_bytes"].as_u64().unwrap() <= WORKING_LIMIT as u64);
        }
        let (c, _, _, _) =
            load_unconfined(&Source::File(catalog::root().join("crt-full/skin.json"))).unwrap();
        assert!(!c.unwrap().sections.is_empty());
        let mut json: serde_json::Value =
            serde_json::from_slice(embedded(1, "skin.json").unwrap()).unwrap();
        json["sections"] = serde_json::json!({"header":{}});
        assert!(config(&serde_json::to_vec(&json).unwrap()).is_err());
    }
    #[test]
    fn full_themes_cover_public_keys_and_preserve_legacy_artwork() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("themes");
        let map: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("component-map.json")).unwrap())
                .unwrap();
        for (name, family) in [
            ("pastelduck", "PixelMplus10"),
            ("pixelduck", "DuckPixel"),
            ("crt-full", "DejaVu Sans Mono"),
        ] {
            let folder = root.join(name);
            let full: serde_json::Value =
                serde_json::from_slice(&std::fs::read(folder.join("skin.json")).unwrap()).unwrap();
            let header = &full["sections"]["header"]["components"]["header"];
            assert_eq!(header["border"], "#00000000");
            assert_eq!(header["border_width"], 0);
            assert!(
                header.get("frame").is_none(),
                "Header frame collides with button borders"
            );
            for (section, key) in [("list", "list-icon-3"), ("player", "album-image")] {
                let rule = &full["sections"][section]["components"][key];
                assert_eq!(
                    rule["tint"], "#00000000",
                    "Photos must retain original colors"
                );
                assert_eq!(rule["pixelated"], false);
            }
            for section in crate::music_theme::SECTIONS {
                assert_eq!(full["sections"][section]["defaults"]["font_family"], family);
            }
            for (key, meta) in map.as_object().unwrap() {
                let section = meta["section"].as_str().unwrap();
                let kind = meta["kind"].as_str().unwrap();
                assert!(
                    full["sections"][section]["components"].get(key).is_some(),
                    "{name}: {section}/{key}"
                );
                // An explicit keyed rule may replace a generic kind rule (the
                // borderless Pastel switch is intentional). Public keys above
                // still require coverage and the complete theme is validated.
                let _ = kind;
            }
            let report = validate_file(&folder.join("skin.json")).unwrap();
            assert_eq!(
                report["sections"].as_array().unwrap().len(),
                crate::music_theme::SECTIONS.len()
            );
            assert!(report["reserved_artwork_bytes"].as_u64().unwrap() <= WORKING_LIMIT as u64);
        }
    }
    #[test]
    fn named_assets_share_the_legacy_working_budget_and_gif_decoder() {
        let dir = Temp::new();
        std::fs::write(dir.0.join("frame.png"), embedded(1, "frame.png").unwrap()).unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(embedded(1, "skin.json").unwrap()).unwrap();
        json["version"] = 2.into();
        json["assets"] = serde_json::json!({"artwork_frame":{"file":"frame.png"}});
        json["theme_assets"] = serde_json::json!({"shared":{"file":"frame.png"}});
        json["sections"] =
            serde_json::json!({"library":{"components":{"surface":{"frame":"shared"}}}});
        let (c, frames, gifs, bytes) = load_unconfined(&dir.source(json.clone())).unwrap();
        assert_eq!(frames.len(), 1);
        assert!(gifs.is_empty());
        let aliases = aliases(&c.unwrap());
        assert_eq!(aliases[2], aliases[SLOTS.len()]);
        assert_eq!(
            bytes,
            frames[0].image.width() as usize * frames[0].image.height() as usize * 8
        );
        std::fs::write(
            dir.0.join("mochune.gif"),
            embedded(1, "mochune.gif").unwrap(),
        )
        .unwrap();
        json["theme_assets"]["shared"]["file"] = "mochune.gif".into();
        let (_, frames, gifs, bytes) = load_unconfined(&dir.source(json)).unwrap();
        assert!(!frames.is_empty());
        assert!(!gifs.is_empty(), "named GIF artwork is supported");
        assert!(bytes <= WORKING_LIMIT);
    }
    #[test]
    fn bundled_skins_decode_within_budget() {
        for k in [1, 2] {
            let (c, frames, mut gifs, memory) =
                load_unconfined(&Source::File(catalog::root().join(if k == 1 {
                    "crt-full/skin.json"
                } else {
                    "pixelduck/skin.json"
                })))
                .unwrap();
            assert!(c.is_some() && frames.len() >= 6 && memory < WORKING_LIMIT);
            for _ in 0..24 {
                let f = gifs[0].next().unwrap();
                assert_eq!(f.image.dimensions(), (160, 80));
            }
        }
    }
    #[test]
    fn rejects_behavior_paths_unknown_slots_and_unbounded_style() {
        let data = embedded(1, "skin.json").unwrap();
        for field in ["layout", "script", "position"] {
            let mut j: serde_json::Value = serde_json::from_slice(data).unwrap();
            j[field] = true.into();
            assert!(config(&serde_json::to_vec(&j).unwrap()).is_err());
        }
        for path in [
            "../oops.png",
            "/tmp/oops.png",
            "https://example.com/x.png",
            "C:\\bad.png",
        ] {
            let mut j: serde_json::Value = serde_json::from_slice(data).unwrap();
            j["assets"]["panel"]["file"] = path.into();
            assert!(config(&serde_json::to_vec(&j).unwrap()).is_err());
        }
        let mut j: serde_json::Value = serde_json::from_slice(data).unwrap();
        j["style"]["title_size"] = 100.into();
        assert!(config(&serde_json::to_vec(&j).unwrap()).is_err());
    }
    #[test]
    fn color_alpha_is_rgba() {
        let c = color("#12345678").unwrap();
        assert_eq!(c.alpha(), 0x78);
        assert_eq!(c.red(), 0x12);
        assert!(color("#bad").is_err());
    }
    pub(super) struct Temp(pub(super) PathBuf);
    impl Temp {
        pub(super) fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "mochune-skin-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn source(&self, j: serde_json::Value) -> Source {
            let p = self.0.join("skin.json");
            std::fs::write(&p, serde_json::to_vec(&j).unwrap()).unwrap();
            Source::File(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn shared_gifs_have_one_decoder_and_budget() {
        let dir = Temp::new();
        std::fs::write(
            dir.0.join("mochune.gif"),
            embedded(1, "mochune.gif").unwrap(),
        )
        .unwrap();
        let mut j: serde_json::Value =
            serde_json::from_slice(embedded(1, "skin.json").unwrap()).unwrap();
        j["assets"] = serde_json::json!({"card":{"file":"mochune.gif"},"thumb":{"file":"mochune.gif"},"play":{"file":"mochune.gif"}});
        let (c, frames, gifs, memory) = load_unconfined(&dir.source(j)).unwrap();
        assert_eq!(gifs.len(), 1);
        assert_eq!(frames.len(), 1);
        assert_eq!(memory, 160 * 80 * 4 * 8);
        let a = aliases(&c.unwrap());
        assert_eq!(a[1], a[5]);
        assert_eq!(a[5], a[6]);
    }
    #[test]
    fn rejects_canvas_bombs_and_combined_gif_budget() {
        let dir = Temp::new();
        let mut out = Vec::new();
        image::codecs::gif::GifEncoder::new(&mut out)
            .encode_frame(image::Frame::new(image::RgbaImage::new(256, 256)))
            .unwrap();
        std::fs::write(dir.0.join("one.gif"), &out).unwrap();
        let mut j: serde_json::Value =
            serde_json::from_slice(embedded(1, "skin.json").unwrap()).unwrap();
        j["assets"] = serde_json::json!({});
        for (i, slot) in ["panel", "card", "artwork_frame", "play", "pause"]
            .iter()
            .enumerate()
        {
            let f = format!("{i}.gif");
            std::fs::write(dir.0.join(&f), &out).unwrap();
            j["assets"][*slot] = serde_json::json!({"file":f});
        }
        assert!(
            load_unconfined(&dir.source(j.clone()))
                .err()
                .unwrap()
                .to_string()
                .contains("budget")
        );
        out[6..10].copy_from_slice(&[255, 255, 255, 255]);
        std::fs::write(dir.0.join("bomb.gif"), &out).unwrap();
        j["assets"] = serde_json::json!({"card":{"file":"bomb.gif"}});
        assert!(load_unconfined(&dir.source(j)).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_asset_symlink_escape() {
        let dir = Temp::new();
        let outside = Temp::new();
        std::fs::write(outside.0.join("x.png"), embedded(1, "panel.png").unwrap()).unwrap();
        std::os::unix::fs::symlink(outside.0.join("x.png"), dir.0.join("link.png")).unwrap();
        let mut j: serde_json::Value =
            serde_json::from_slice(embedded(1, "skin.json").unwrap()).unwrap();
        j["assets"] = serde_json::json!({"panel":{"file":"link.png"}});
        assert!(load_unconfined(&dir.source(j)).is_err());
    }
    #[test]
    fn shutdown_cancels_full_result_queue() {
        let (tx, rx) = mpsc::sync_channel(1);
        let (out, _events) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let worker = thread::spawn(move || work(rx, out, flag));
        tx.send(Command::Load(1, Source::Default)).unwrap();
        tx.send(Command::Load(2, Source::Default)).unwrap();
        thread::sleep(Duration::from_millis(20));
        let start = Instant::now();
        stop.store(true, Ordering::Relaxed);
        let _ = tx.try_send(Command::Stop);
        worker.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn validates_all_gif_frame_bounds_counts_and_truncation_before_decoding() {
        let mut base = b"GIF89a".to_vec();
        base.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
        let frame = [0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x44, 0x01, 0];
        let mut valid = base.clone();
        valid.extend_from_slice(&frame);
        valid.push(0x3b);
        assert_eq!(gif_structure(&valid, 1, 1).unwrap(), 1);
        let mut huge = frame;
        huge[5] = 2;
        let mut bad = base.clone();
        bad.extend_from_slice(&huge);
        bad.push(0x3b);
        assert!(gif_structure(&bad, 1, 1).is_err());
        let mut many = base;
        for _ in 0..301 {
            many.extend_from_slice(&frame);
        }
        many.push(0x3b);
        assert!(gif_structure(&many, 1, 1).is_err());
        valid.pop();
        assert!(gif_structure(&valid, 1, 1).is_err());
    }
    #[test]
    fn invalid_reload_keeps_the_previous_animation_epoch() {
        let dir = Temp::new();
        let (tx, rx) = mpsc::sync_channel(1);
        let (out, events) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let worker = thread::spawn(move || work(rx, out, flag));
        tx.send(Command::Load(
            1,
            Source::File(catalog::root().join("crt-full/skin.json")),
        ))
        .unwrap();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(2)),
            Ok(Reply::Loaded(1, Ok(_)))
        ));
        tx.send(Command::Load(2, Source::File(dir.0.join("missing.json"))))
            .unwrap();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(2)),
            Ok(Reply::Loaded(2, Err(_)))
        ));
        // GIF encoders may merge equal frames into a longer first frame.
        // Wait for the selected animation's real delay, not an old duck timing.
        let source = Source::File(catalog::root().join("crt-full/skin.json"));
        let (_, _, animations, _) = load(&source).unwrap();
        let wait = animations
            .iter()
            .find(|g| g.slot == 1)
            .unwrap()
            .due
            .saturating_duration_since(Instant::now())
            + Duration::from_millis(30);
        thread::sleep(wait);
        tx.send(Command::Tick(1, 1 << 1)).unwrap();
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(2)),Ok(Reply::Frames(1,frames)) if !frames.is_empty())
        );
        stop.store(true, Ordering::Relaxed);
        let _ = tx.try_send(Command::Stop);
        worker.join().unwrap();
    }
    #[test]
    fn bundled_themes_have_one_mochune_card_and_no_duck_variant() {
        for folder in ["pastelduck", "pixelduck", "crt-full"] {
            let path = catalog::root().join(folder).join("skin.json");
            let (config, _, active, memory) = load(&Source::File(path)).unwrap();
            assert_eq!(config.unwrap().assets["card"].file, "mochune.gif");
            assert_eq!(active.len(), 1);
            assert!(memory < WORKING_LIMIT);
        }
    }

    #[test]
    fn default_card_is_optional_bounded_and_streamed() {
        let (_, frames, animations, memory) = load(&Source::Default).unwrap();
        assert!(frames.is_empty() && animations.is_empty() && memory == 0);
        let (config, frames, animations, memory) = load(&Source::DefaultCard).unwrap();
        assert!(config.is_none());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].slot, 1);
        assert_eq!(animations.len(), 1);
        assert!(memory < WORKING_LIMIT);
        assert!(gif_structure(&animations[0].bytes, 160, 80).unwrap() <= 300);
    }
}
