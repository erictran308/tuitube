//! Video thumbnails and channel photos.
//!
//! Every frame, the grid asks for the images it has on screen. After the
//! frame, [`Images::fetch`] starts whatever is missing: a download (only
//! from YouTube's image hosts, at most [`MAX_BYTES`], kept in a disk cache),
//! then decode and encode for the terminal on a blocking thread, within
//! size limits, with a panic in a decoder caught.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};
use ratatui::layout::Size;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FontSize, Resize};
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppEvent;
use crate::ids::{ChannelId, VideoId, image_url_allowed};

/// The biggest image downloaded. Thumbnails are 20–150 KB.
const MAX_BYTES: usize = 2 * 1024 * 1024;
/// At most this many images decode at once.
const MAX_BUILDING: usize = 4;
/// Images downloading at once.
const MAX_DOWNLOADS: usize = 6;
/// Encoded images kept; the ones drawn longest ago go first.
const MAX_READY: usize = 400;
/// An image still building after this long counts as failed.
const BUILD_TIMEOUT: Duration = Duration::from_secs(40);

/// Which picture: a video's thumbnail or a channel's photo.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    Thumbnail(VideoId),
    Avatar(ChannelId),
}

/// One encoded image: a picture at one size in cells.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub subject: Subject,
    pub cols: u16,
    pub rows: u16,
}

/// An image finished encoding (or failed) on a background thread.
pub struct ImageEvent {
    key: Key,
    result: Result<Protocol>,
}

impl std::fmt::Debug for ImageEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageEvent")
            .field("key", &self.key)
            .finish()
    }
}

/// Caps for decoding: YouTube's images are at most 1280×720 (thumbnails)
/// and 900×900 (channel photos), so a much bigger one is only a way to run
/// memory out.
fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits
}

/// Decodes a JPEG, PNG or WebP picture, the formats YouTube serves, and no
/// other: a dependency may compile in more decoders (arboard's TIFF) than
/// tuitube wants to run on downloaded bytes.
fn decode(data: &[u8]) -> Result<DynamicImage> {
    use image::ImageFormat::{Jpeg, Png, WebP};
    let mut reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format()?;
    if !matches!(reader.format(), Some(Jpeg | Png | WebP)) {
        bail!("not a JPEG, PNG or WebP picture");
    }
    reader.limits(limits());
    Ok(reader.decode()?)
}

thread_local! {
    /// Set while this thread decodes a downloaded image.
    static DECODING: Cell<bool> = const { Cell::new(false) };
}

/// The panic is in an image decoder, where it's caught: the image shows as
/// broken and the app carries on.
pub fn panic_is_contained() -> bool {
    DECODING.with(Cell::get)
}

fn contained<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    DECODING.with(|d| d.set(true));
    let result = catch_unwind(AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("the image couldn't be decoded")));
    DECODING.with(|d| d.set(false));
    result
}

/// `photo` cut to a circle in the middle of a `width`×`height` canvas,
/// transparent around it, with a soft edge.
pub fn circle(photo: &DynamicImage, width: u32, height: u32) -> RgbaImage {
    let mut canvas = RgbaImage::new(width, height);
    let size = width.min(height);
    let side = photo.width().min(photo.height());
    if size == 0 || side == 0 {
        return canvas;
    }
    let square = photo
        .crop_imm(
            (photo.width() - side) / 2,
            (photo.height() - side) / 2,
            side,
            side,
        )
        .resize_exact(size, size, FilterType::Triangle)
        .to_rgba8();
    let (left, top) = ((width - size) / 2, (height - size) / 2);
    let radius = size as f32 / 2.0;
    for (x, y, pixel) in square.enumerate_pixels() {
        let dx = x as f32 + 0.5 - radius;
        let dy = y as f32 + 0.5 - radius;
        let inside = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
        let mut pixel = *pixel;
        pixel[3] = (f32::from(pixel[3]) * inside).round() as u8;
        canvas.put_pixel(left + x, top + y, pixel);
    }
    canvas
}

/// `photo` cropped to fill `width`×`height` from its middle: a 4:3
/// thumbnail with black bars loses the bars.
pub fn fill(photo: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    if width == 0 || height == 0 {
        return DynamicImage::new_rgba8(width.max(1), height.max(1));
    }
    photo.resize_to_fill(width, height, FilterType::Triangle)
}

/// Which way images are drawn: what the terminal said it can do, unless
/// the settings (`images`) or `TT_IMAGES` say otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageMode {
    #[default]
    Auto,
    Kitty,
    Sixel,
    Iterm2,
    /// Colored half blocks: works in any terminal, coarse.
    Blocks,
}

impl ImageMode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "kitty" => Self::Kitty,
            "sixel" => Self::Sixel,
            "iterm2" => Self::Iterm2,
            "blocks" | "halfblocks" => Self::Blocks,
            _ => return None,
        })
    }
}

/// Asks the terminal how it draws images, then applies `mode`. Call after
/// the terminal is set up and before keys are read.
pub fn picker(mode: ImageMode) -> Picker {
    let detected = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    choose(detected, mode, |name| std::env::var(name).ok(), cell_size())
}

/// Whether the terminal answered the image query at all: a terminal that
/// doesn't draw images still reports its cell size or sixel support.
fn answered(picker: &Picker) -> bool {
    picker.protocol_type() != ProtocolType::Halfblocks || !picker.capabilities().is_empty()
}

/// The picker to use. A terminal that didn't answer the query, but says
/// by its environment that it's Ghostty or kitty, gets the kitty protocol:
/// both draw it, and an answer can go missing (a slow start, something
/// between tuitube and the terminal). A terminal that answered is believed.
fn choose(
    detected: Picker,
    mode: ImageMode,
    env: impl Fn(&str) -> Option<String>,
    cell: Option<FontSize>,
) -> Picker {
    let font = if answered(&detected) {
        detected.font_size()
    } else {
        cell.unwrap_or(detected.font_size())
    };
    let forced = |protocol| {
        // Deprecated in favor of asking the terminal, which is what failed
        // here; it's the only way to give a picker the cell size.
        #[allow(deprecated)]
        let mut picker = Picker::from_fontsize(font);
        picker.set_protocol_type(protocol);
        picker
    };
    match mode {
        ImageMode::Kitty => forced(ProtocolType::Kitty),
        ImageMode::Sixel => forced(ProtocolType::Sixel),
        ImageMode::Iterm2 => forced(ProtocolType::Iterm2),
        ImageMode::Blocks => forced(ProtocolType::Halfblocks),
        ImageMode::Auto if answered(&detected) || detected.tmux_detected() => detected,
        ImageMode::Auto if kitty_terminal(&env) && cell.is_some() => forced(ProtocolType::Kitty),
        ImageMode::Auto => detected,
    }
}

/// The environment says the terminal is Ghostty or kitty.
fn kitty_terminal(env: &impl Fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| env(name).filter(|v| !v.is_empty());
    set("TERM_PROGRAM").is_some_and(|p| p.eq_ignore_ascii_case("ghostty"))
        || set("TERM").is_some_and(|t| t == "xterm-ghostty" || t == "xterm-kitty")
        || set("KITTY_WINDOW_ID").is_some()
}

/// The terminal's cell size in pixels, from the window size it reports.
#[cfg(unix)]
fn cell_size() -> Option<FontSize> {
    // SAFETY: TIOCGWINSZ only fills in the winsize given; on a stdout that
    // isn't a terminal it fails and nothing is read.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } != 0 {
        return None;
    }
    let (w, h) = (size.ws_xpixel, size.ws_ypixel);
    let (cols, rows) = (size.ws_col, size.ws_row);
    (w > 0 && h > 0 && cols > 0 && rows > 0).then(|| FontSize::new(w / cols, h / rows))
}

#[cfg(not(unix))]
fn cell_size() -> Option<FontSize> {
    None
}

/// What `tuitube --check` says about images: what the terminal answered,
/// and what's used.
pub fn check(mode: ImageMode) -> String {
    let detected = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let answer = if answered(&detected) {
        format!("the terminal answered {}", name(detected.protocol_type()))
    } else {
        "no answer from the terminal".into()
    };
    let used = choose(detected, mode, |name| std::env::var(name).ok(), cell_size());
    let font = used.font_size();
    let mut out = format!(
        "{}; {answer}; cells {}×{} px",
        name(used.protocol_type()),
        font.width,
        font.height
    );
    if used.protocol_type() == ProtocolType::Halfblocks {
        out.push_str(
            "\n             (thumbnails are coarse and channel photos aren't shown; \
             if your terminal draws images, set images = \"kitty\" in settings.toml)",
        );
    }
    out
}

fn name(protocol: ProtocolType) -> &'static str {
    match protocol {
        ProtocolType::Kitty => "kitty graphics",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Iterm2 => "iTerm2 images",
        ProtocolType::Halfblocks => "colored blocks",
    }
}

pub struct Images {
    picker: Picker,
    tx: UnboundedSender<AppEvent>,
    http: reqwest::Client,
    cache_dir: Option<PathBuf>,
    downloads: Arc<Semaphore>,
    /// At most `MAX_BUILDING` pictures decode at once, however many are
    /// downloading.
    decoding: Arc<Semaphore>,
    /// Encoded images, with the frame each was last drawn in.
    ready: HashMap<Key, (Protocol, u64)>,
    frame: u64,
    building: HashMap<Key, Instant>,
    failed: HashSet<Key>,
    /// Images the last frame showed, with the URLs to try, best first.
    wanted: Vec<(Key, Vec<String>)>,
    /// Pictures already at hand (the demo's), used instead of downloads.
    preloaded: HashMap<Subject, Arc<DynamicImage>>,
    /// Nothing is downloaded: only preloaded pictures show (`--demo`).
    offline: bool,
    /// Where the last frame drew each image, for the demo's screenshot.
    pub placed: Vec<Placement>,
}

/// An image drawn in a frame: the area it was made for, and the part of it
/// on screen (less, for a card cut off by the window's bottom).
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub subject: Subject,
    pub area: ratatui::layout::Rect,
    pub shown: ratatui::layout::Rect,
}

impl Images {
    pub fn new(
        picker: Picker,
        tx: UnboundedSender<AppEvent>,
        http: reqwest::Client,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        if let Some(dir) = cache_dir.clone() {
            std::thread::spawn(move || tidy_cache(&dir));
        }
        Self {
            picker,
            tx,
            http,
            cache_dir,
            downloads: Arc::new(Semaphore::new(MAX_DOWNLOADS)),
            decoding: Arc::new(Semaphore::new(MAX_BUILDING)),
            ready: HashMap::new(),
            frame: 0,
            building: HashMap::new(),
            failed: HashSet::new(),
            wanted: Vec::new(),
            preloaded: HashMap::new(),
            offline: false,
            placed: Vec::new(),
        }
    }

    /// Downloads nothing from now on: pictures not preloaded stay missing.
    pub fn go_offline(&mut self) {
        self.offline = true;
    }

    /// A picture to use for `subject` instead of downloading it.
    pub fn preload(&mut self, subject: Subject, image: DynamicImage) {
        self.preloaded.insert(subject, Arc::new(image));
    }

    /// Notes where an image was drawn this frame.
    pub fn place(
        &mut self,
        subject: &Subject,
        area: ratatui::layout::Rect,
        shown: ratatui::layout::Rect,
    ) {
        self.placed.push(Placement {
            subject: subject.clone(),
            area,
            shown,
        });
    }

    /// Builds every preloaded picture the last frame wanted, now, on this
    /// thread: for drawing a frame with them in a test.
    #[cfg(test)]
    pub fn build_wanted_now(&mut self) {
        let font = self.picker.font_size();
        for (key, _) in std::mem::take(&mut self.wanted) {
            if let Some(photo) = self.preloaded.get(&key.subject).cloned()
                && let Ok(image) = encode(&self.picker, font, &key, &photo)
            {
                self.ready.insert(key, (image, self.frame));
            }
        }
    }

    /// False when the terminal shows no real images, only colored blocks
    /// (halfblocks), which are too coarse for a channel photo in a few cells.
    pub fn draws_photos(&self) -> bool {
        self.picker.protocol_type() != ProtocolType::Halfblocks
    }

    /// The terminal paints each image from one cell, over anything drawn
    /// on top since: sixel and iTerm2's protocol. Nothing may be drawn over
    /// such images, and they mustn't be drawn under popups.
    pub fn paints_over(&self) -> bool {
        matches!(
            self.picker.protocol_type(),
            ProtocolType::Sixel | ProtocolType::Iterm2
        )
    }

    pub fn font_size(&self) -> FontSize {
        self.picker.font_size()
    }

    /// The image ready to draw, if it is; else it's fetched after the frame,
    /// from the first of `urls` that has it.
    pub fn get(&mut self, key: &Key, urls: &[String]) -> Option<&Protocol> {
        if !self.ready.contains_key(key) {
            self.wanted.push((key.clone(), urls.to_vec()));
            return None;
        }
        let frame = self.frame;
        let (image, used) = self.ready.get_mut(key)?;
        *used = frame;
        Some(image)
    }

    pub fn is_broken(&self, key: &Key) -> bool {
        self.failed.contains(key)
    }

    /// Starts downloads and encodes for what the last frame wanted.
    pub fn fetch(&mut self) {
        self.frame += 1;
        let failed = &mut self.failed;
        self.building.retain(|key, started| {
            let alive = started.elapsed() < BUILD_TIMEOUT;
            if !alive {
                failed.insert(key.clone());
            }
            alive
        });
        for (key, mut urls) in std::mem::take(&mut self.wanted) {
            if self.ready.contains_key(&key)
                || self.building.contains_key(&key)
                || self.failed.contains(&key)
            {
                continue;
            }
            if self.building.len() >= MAX_BUILDING * 3 {
                break;
            }
            if let Some(photo) = self.preloaded.get(&key.subject).cloned() {
                self.building.insert(key.clone(), Instant::now());
                self.spawn_preloaded(key, photo);
                continue;
            }
            if self.offline {
                self.failed.insert(key);
                continue;
            }
            urls.retain(|url| image_url_allowed(url));
            if urls.is_empty() {
                self.failed.insert(key);
                continue;
            }
            self.building.insert(key.clone(), Instant::now());
            self.spawn(key, urls);
        }
    }

    fn spawn_preloaded(&self, key: Key, photo: Arc<DynamicImage>) {
        let picker = self.picker.clone();
        let tx = self.tx.clone();
        let font = self.picker.font_size();
        let decoding = self.decoding.clone();
        tokio::spawn(async move {
            let Ok(_permit) = decoding.acquire_owned().await else {
                return;
            };
            let build_key = key.clone();
            let result = tokio::task::spawn_blocking(move || {
                contained(|| encode(&picker, font, &build_key, &photo))
            })
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("the image couldn't be drawn")));
            let _ = tx.send(AppEvent::Image(ImageEvent { key, result }));
        });
    }

    fn spawn(&self, key: Key, urls: Vec<String>) {
        let picker = self.picker.clone();
        let tx = self.tx.clone();
        let http = self.http.clone();
        let downloads = self.downloads.clone();
        let cache = self
            .cache_dir
            .as_ref()
            .map(|dir| dir.join(cache_name(&key.subject)));
        let font = self.picker.font_size();
        let decoding = self.decoding.clone();
        tokio::spawn(async move {
            let (data, from_cache) = match load(&http, &downloads, &urls, cache.as_deref()).await {
                Ok(loaded) => loaded,
                Err(e) => {
                    let _ = tx.send(AppEvent::Image(ImageEvent {
                        key,
                        result: Err(e),
                    }));
                    return;
                }
            };
            let Ok(_permit) = decoding.acquire_owned().await else {
                return;
            };
            let build_key = key.clone();
            let data = Arc::new(data);
            let bytes = data.clone();
            let result = tokio::task::spawn_blocking(move || {
                contained(|| build(&picker, font, &build_key, &bytes))
            })
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("the image couldn't be decoded")));
            // Only a picture that decoded is kept, and a kept one that no
            // longer does is thrown away, to be fetched again.
            if let Some(path) = &cache {
                match (&result, from_cache) {
                    (Ok(_), false) => {
                        let _ = write_private(path, &data).await;
                    }
                    (Err(_), true) => {
                        let _ = tokio::fs::remove_file(path).await;
                    }
                    _ => {}
                }
            }
            let _ = tx.send(AppEvent::Image(ImageEvent { key, result }));
        });
    }

    pub fn on_built(&mut self, event: ImageEvent) {
        self.building.remove(&event.key);
        match event.result {
            Ok(image) => {
                if self.ready.len() >= MAX_READY
                    && let Some(oldest) = self
                        .ready
                        .iter()
                        .min_by_key(|(_, (_, used))| *used)
                        .map(|(k, _)| k.clone())
                {
                    self.ready.remove(&oldest);
                }
                self.ready.insert(event.key, (image, self.frame));
            }
            Err(_) => {
                self.failed.insert(event.key);
            }
        }
    }

    /// Forgets failures, so a refresh tries those images again.
    pub fn retry_failed(&mut self) {
        self.failed.clear();
    }
}

/// Decodes `data` and encodes it for the terminal at `key`'s size: a
/// thumbnail cropped to fill, a channel photo cut to a circle.
fn build(picker: &Picker, font: FontSize, key: &Key, data: &[u8]) -> Result<Protocol> {
    encode(picker, font, key, &decode(data)?)
}

/// Encodes a decoded picture for the terminal at `key`'s size.
fn encode(picker: &Picker, font: FontSize, key: &Key, photo: &DynamicImage) -> Result<Protocol> {
    let (width, height) = (
        u32::from(key.cols) * u32::from(font.width),
        u32::from(key.rows) * u32::from(font.height),
    );
    let image = match key.subject {
        Subject::Thumbnail(_) => fill(photo, width, height),
        Subject::Avatar(_) => circle(photo, width, height).into(),
    };
    Ok(picker.new_protocol(image, Size::new(key.cols, key.rows), Resize::Fit(None))?)
}

/// The cache file for a picture: named by its checked id only.
fn cache_name(subject: &Subject) -> String {
    match subject {
        Subject::Thumbnail(id) => format!("t-{id}"),
        Subject::Avatar(id) => format!("c-{id}"),
    }
}

/// How long a cached picture is kept after it was fetched.
const CACHE_FOR: Duration = Duration::from_secs(30 * 24 * 3600);

/// Deletes cached pictures fetched more than `CACHE_FOR` ago, leftovers of
/// interrupted writes, and the 480×360 thumbnails the first version cached
/// (`v-…`), so the cache doesn't grow without end.
fn tidy_cache(dir: &std::path::Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_none_or(|age| age > CACHE_FOR);
        if name.starts_with("v-") || name.ends_with(".new") || old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How long a cached channel photo is used before it's fetched again.
const AVATAR_CACHE: Duration = Duration::from_secs(14 * 24 * 3600);

/// The image's bytes, and whether they came from the cache: from there if
/// they're in it, else from the first of `urls` that has them.
async fn load(
    http: &reqwest::Client,
    downloads: &Semaphore,
    urls: &[String],
    cache: Option<&std::path::Path>,
) -> Result<(Vec<u8>, bool)> {
    if let Some(path) = cache
        && let Ok(meta) = tokio::fs::metadata(path).await
        && meta.len() as usize <= MAX_BYTES
    {
        let fresh = !path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("c-"))
            || meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age < AVATAR_CACHE);
        if fresh && let Ok(data) = tokio::fs::read(path).await {
            return Ok((data, true));
        }
    }
    let _permit = downloads.acquire().await?;
    let mut last = anyhow::anyhow!("no image URL");
    for url in urls {
        match download(http, url).await {
            Ok(data) => return Ok((data, false)),
            // A video too old for the big thumbnail answers 404 (with a
            // tiny gray picture): the next size is tried.
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn download(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut response = http.get(url).send().await?;
    if !response.status().is_success() {
        bail!("HTTP {}", response.status().as_u16());
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if data.len() + chunk.len() > MAX_BYTES {
            bail!("the image is too big");
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

/// Writes a cache file readable only by you, through a rename.
async fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    // A name of its own, made new: two writers never share one, and an
    // existing file (or link) is never written through.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let new = path.with_extension(format!("{}-{n}.new", std::process::id()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&new).await?;
    tokio::io::AsyncWriteExt::write_all(&mut file, data).await?;
    drop(file);
    tokio::fs::rename(&new, path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut data = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(width, height)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        data.into_inner()
    }

    #[test]
    fn channel_photos_are_cut_to_a_circle_with_see_through_corners() {
        let photo = image::RgbaImage::from_pixel(160, 100, image::Rgba([255, 0, 0, 255])).into();
        let round = circle(&photo, 40, 44);
        let alpha = |x, y| round.get_pixel(x, y)[3];
        assert_eq!(alpha(0, 2), 0, "corner");
        assert_eq!(alpha(20, 22), 255, "middle");
        assert_eq!(circle(&photo, 0, 44).dimensions(), (0, 44), "no panic");
    }

    #[test]
    fn thumbnails_fill_their_card_and_lose_black_bars() {
        // hqdefault: 480×360, the 16:9 picture between black bars.
        let mut photo = image::RgbImage::new(480, 360);
        for (_, y, p) in photo.enumerate_pixels_mut() {
            *p = if (45..315).contains(&y) {
                image::Rgb([255, 255, 255])
            } else {
                image::Rgb([0, 0, 0])
            };
        }
        let filled = fill(&photo.into(), 320, 180).to_rgb8();
        assert_eq!(filled.dimensions(), (320, 180));
        assert_eq!(filled.get_pixel(160, 2)[0], 255, "no bar at the top");
        assert_eq!(filled.get_pixel(160, 177)[0], 255, "no bar at the bottom");
    }

    #[test]
    fn images_far_bigger_than_youtube_sends_are_refused() {
        assert!(decode(&png(512, 512)).is_ok());
        assert!(decode(&png(3000, 1)).is_err());
    }

    #[test]
    fn only_jpeg_png_and_webp_are_decoded() {
        // A TIFF header: arboard compiles the decoder in, tuitube won't use it.
        let mut tiff = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(4, 4)
            .write_to(&mut tiff, image::ImageFormat::Png)
            .unwrap();
        let mut fake = b"II*\0".to_vec();
        fake.extend_from_slice(&[8, 0, 0, 0, 0, 0]);
        assert!(decode(&fake).is_err());
        assert!(decode(b"GIF89a\x01\x00\x01\x00").is_err());
        assert!(decode(&tiff.into_inner()).is_ok(), "PNG still works");
    }

    #[tokio::test]
    async fn a_decoder_panic_marks_the_image_broken_instead_of_ending_the_app() {
        let result: Result<()> = contained(|| panic!("a decoder bug"));
        assert!(result.is_err());
        assert!(!panic_is_contained(), "only while decoding");
    }

    fn env(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn ghostty_gets_kitty_images_even_when_its_answer_went_missing() {
        let cell = Some(FontSize::new(13, 30));
        let ghostty = env(&[("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-256color")]);
        let picker = choose(Picker::halfblocks(), ImageMode::Auto, ghostty, cell);
        assert_eq!(picker.protocol_type(), ProtocolType::Kitty);
        let font = picker.font_size();
        assert_eq!((font.width, font.height), (13, 30));

        // Not a kitty terminal, or no cell size to lay images out by: blocks.
        let plain = env(&[("TERM_PROGRAM", "Apple_Terminal")]);
        let picker = choose(Picker::halfblocks(), ImageMode::Auto, plain, cell);
        assert_eq!(picker.protocol_type(), ProtocolType::Halfblocks);
        let ghostty = env(&[("TERM_PROGRAM", "ghostty")]);
        let picker = choose(Picker::halfblocks(), ImageMode::Auto, ghostty, None);
        assert_eq!(picker.protocol_type(), ProtocolType::Halfblocks);

        // The settings win.
        let plain = env(&[]);
        let picker = choose(Picker::halfblocks(), ImageMode::Sixel, plain, cell);
        assert_eq!(picker.protocol_type(), ProtocolType::Sixel);
        assert_eq!(ImageMode::parse(" Kitty "), Some(ImageMode::Kitty));
        assert_eq!(ImageMode::parse("nope"), None);
    }

    #[test]
    fn urls_off_youtubes_image_hosts_are_never_fetched() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut images = Images::new(Picker::halfblocks(), tx, crate::feed::client(), None);
        let key = Key {
            subject: Subject::Thumbnail(VideoId::parse("dQw4w9WgXcQ").unwrap()),
            cols: 4,
            rows: 2,
        };
        let urls = ["https://evil.example/x.jpg".to_string()];
        assert!(images.get(&key, &urls).is_none());
        images.fetch();
        assert!(images.is_broken(&key));
    }
}
