//! Video thumbnails and channel photos.
//!
//! Every frame, the grid asks for the images it has on screen. After the
//! frame, [`Images::fetch`] starts whatever is missing: a download (only
//! from YouTube's image hosts, at most [`MAX_BYTES`], kept in a disk cache),
//! then decode and encode for the terminal on a blocking thread, within
//! size limits, with a panic in a decoder caught.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::Widget;
use ratatui_image::picker::cap_parser::Parser;
use ratatui_image::picker::{Capability, Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::protocol::kitty::Kitty;
use ratatui_image::{FontSize, Image, Resize};
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
    result: Result<Encoded>,
}

impl std::fmt::Debug for ImageEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageEvent")
            .field("key", &self.key)
            .finish()
    }
}

/// What the pictures kept may take in a terminal speaking kitty's protocol,
/// in bytes of pixels. Such a terminal keeps every image it's sent until
/// told to delete it, and past its quota (320 MB in Ghostty and kitty) it
/// drops the ones sent first: the first screen's thumbnails and channel
/// photos, sent once and never again, which then stay blank. Half the
/// quota: at 23 × 57 px cells a card's thumbnail takes about 2 MB, a
/// channel photo 42 KB, the watch page's picture 17 MB.
const KITTY_BUDGET: usize = 160_000_000;

/// Numbers of kitty images dropped since the terminal was last told; see
/// [`Images::send_deletions`].
type Dropped = Arc<Mutex<Vec<u32>>>;

/// An image for kitty's protocol: the number the terminal knows it by and
/// the bytes it takes there once sent. Dropping one that was sent, wherever
/// that happens, adds its number to those the terminal is told to delete.
struct KittyImage {
    id: u32,
    bytes: usize,
    dropped: Dropped,
    /// It has been drawn, and the code that sends it was taken out of the
    /// frame to be sent apart ([`Images::draw`]).
    sent: bool,
}

impl Drop for KittyImage {
    fn drop(&mut self) {
        if self.sent {
            self.dropped
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(self.id);
        }
    }
}

/// An image encoded for the terminal.
struct Encoded {
    image: Protocol,
    /// Deletes it from the terminal once it's dropped.
    kitty: Option<KittyImage>,
}

impl Encoded {
    /// What it takes in a terminal speaking kitty's protocol: nothing until
    /// it's sent.
    fn sent_bytes(&self) -> usize {
        self.kitty
            .as_ref()
            .filter(|k| k.sent)
            .map_or(0, |k| k.bytes)
    }
}

/// An encoded image kept to draw.
struct Ready {
    encoded: Encoded,
    /// The frame it was last drawn in, or asked for while it couldn't be.
    used: u64,
}

/// Where numbers for kitty's images start: at random, as the picker's own
/// are, since every program drawing in the terminal's window shares them
/// (tuitube and tuigram in panes of one tmux), and one's deletions mustn't
/// take the other's pictures.
fn kitty_id_base() -> u32 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(std::process::id()) as u32
}

/// The code that has the terminal delete image number `id` of kitty's
/// protocol. `I` deletes the image itself, not only where it's placed;
/// `q=2` asks for no answer, which would come in as keys.
fn kitty_delete(id: u32, tmux: bool) -> String {
    let (start, escape, end) = Parser::tmux_start_escape_end(tmux);
    format!("{start}{escape}_Ga=d,d=I,i={id},q=2{escape}\\{end}")
}

/// The character kitty's protocol draws a picture's cells with, ratatui-image
/// and the terminal both.
const PLACEHOLDER: char = '\u{10EEEE}';

/// The marks kitty's protocol numbers a placeholder's row and column with,
/// combining characters after it: number n is the nth of these (kitty's
/// `rowcolumn-diacritics.txt`, which ratatui-image keeps to itself).
#[rustfmt::skip]
pub(crate) const DIACRITICS: [char; 297] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}',
    '\u{346}', '\u{34A}', '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}',
    '\u{35B}', '\u{363}', '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}',
    '\u{36A}', '\u{36B}', '\u{36C}', '\u{36D}', '\u{36E}', '\u{36F}', '\u{483}', '\u{484}',
    '\u{485}', '\u{486}', '\u{487}', '\u{592}', '\u{593}', '\u{594}', '\u{595}', '\u{597}',
    '\u{598}', '\u{599}', '\u{59C}', '\u{59D}', '\u{59E}', '\u{59F}', '\u{5A0}', '\u{5A1}',
    '\u{5A8}', '\u{5A9}', '\u{5AB}', '\u{5AC}', '\u{5AF}', '\u{5C4}', '\u{610}', '\u{611}',
    '\u{612}', '\u{613}', '\u{614}', '\u{615}', '\u{616}', '\u{617}', '\u{657}', '\u{658}',
    '\u{659}', '\u{65A}', '\u{65B}', '\u{65D}', '\u{65E}', '\u{6D6}', '\u{6D7}', '\u{6D8}',
    '\u{6D9}', '\u{6DA}', '\u{6DB}', '\u{6DC}', '\u{6DF}', '\u{6E0}', '\u{6E1}', '\u{6E2}',
    '\u{6E4}', '\u{6E7}', '\u{6E8}', '\u{6EB}', '\u{6EC}', '\u{730}', '\u{732}', '\u{733}',
    '\u{735}', '\u{736}', '\u{73A}', '\u{73D}', '\u{73F}', '\u{740}', '\u{741}', '\u{743}',
    '\u{745}', '\u{747}', '\u{749}', '\u{74A}', '\u{7EB}', '\u{7EC}', '\u{7ED}', '\u{7EE}',
    '\u{7EF}', '\u{7F0}', '\u{7F1}', '\u{7F3}', '\u{816}', '\u{817}', '\u{818}', '\u{819}',
    '\u{81B}', '\u{81C}', '\u{81D}', '\u{81E}', '\u{81F}', '\u{820}', '\u{821}', '\u{822}',
    '\u{823}', '\u{825}', '\u{826}', '\u{827}', '\u{829}', '\u{82A}', '\u{82B}', '\u{82C}',
    '\u{82D}', '\u{951}', '\u{953}', '\u{954}', '\u{F82}', '\u{F83}', '\u{F86}', '\u{F87}',
    '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}',
    '\u{1A76}', '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}',
    '\u{1B6B}', '\u{1B6D}', '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}',
    '\u{1B73}', '\u{1CD0}', '\u{1CD1}', '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}',
    '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}', '\u{1DC5}', '\u{1DC6}', '\u{1DC7}',
    '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}', '\u{1DD2}', '\u{1DD3}',
    '\u{1DD4}', '\u{1DD5}', '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}', '\u{1DDA}',
    '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}', '\u{1DE0}', '\u{1DE1}',
    '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}',
    '\u{20D1}', '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}',
    '\u{20E1}', '\u{20E7}', '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}',
    '\u{2DE0}', '\u{2DE1}', '\u{2DE2}', '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}',
    '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}', '\u{2DEB}', '\u{2DEC}', '\u{2DED}',
    '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}', '\u{2DF2}', '\u{2DF3}', '\u{2DF4}',
    '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}', '\u{2DFB}',
    '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}',
    '\u{A6F0}', '\u{A6F1}', '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}',
    '\u{A8E5}', '\u{A8E6}', '\u{A8E7}', '\u{A8E8}', '\u{A8E9}', '\u{A8EA}', '\u{A8EB}',
    '\u{A8EC}', '\u{A8ED}', '\u{A8EE}', '\u{A8EF}', '\u{A8F0}', '\u{A8F1}', '\u{AAB0}',
    '\u{AAB2}', '\u{AAB3}', '\u{AAB7}', '\u{AAB8}', '\u{AABE}', '\u{AABF}', '\u{AAC1}',
    '\u{FE20}', '\u{FE21}', '\u{FE22}', '\u{FE23}', '\u{FE24}', '\u{FE25}', '\u{FE26}',
    '\u{10A0F}', '\u{10A38}', '\u{1D185}', '\u{1D186}', '\u{1D187}', '\u{1D188}',
    '\u{1D189}', '\u{1D1AA}', '\u{1D1AB}', '\u{1D1AC}', '\u{1D1AD}', '\u{1D242}',
    '\u{1D243}', '\u{1D244}',
];

/// Takes the code that sends a kitty picture out of `cell`, the first one a
/// picture's first drawing draws, where ratatui-image puts it before the
/// placeholder (wrapped for tmux, if need be), and leaves the placeholder.
fn take_transmit(cell: &mut ratatui::buffer::Cell) -> Option<String> {
    let symbol = cell.symbol();
    if !symbol.starts_with('\x1b') {
        return None;
    }
    let at = symbol.rfind(PLACEHOLDER)?;
    let (code, placeholder) = (symbol[..at].to_string(), symbol[at..].to_string());
    cell.set_symbol(&placeholder);
    Some(code)
}

/// ratatui-image numbers (row, column, the picture's id's high byte) only
/// the first placeholder of each row it draws in `drawn`: the terminal
/// takes each one after it as the column after the placeholder to its left.
/// Something drawn over the picture later in the frame (what the watch
/// page's picture says, a card's length, a popup) leaves the one after it
/// none to go on from, and the terminal starts the picture over there, or
/// (Ghostty, Pepper) leaves the rest of the row blank. So each placeholder
/// is given numbers of its own: its column, and its row's first's row and
/// high byte.
fn number_placeholders(buf: &mut Buffer, drawn: Rect) {
    let mut symbol = String::new();
    for y in drawn.top()..drawn.bottom() {
        // The row's first, as ratatui-image numbers it: after what sends
        // the picture, should that still be there.
        let first = buf[(drawn.x, y)].symbol();
        let Some(at) = first.rfind(PLACEHOLDER) else {
            continue;
        };
        let mut numbers = first[at..].chars().skip(1);
        let (Some(row), Some(_), Some(high), None) = (
            numbers.next(),
            numbers.next(),
            numbers.next(),
            numbers.next(),
        ) else {
            continue;
        };
        for x in drawn.left() + 1..drawn.right() {
            // ratatui-image draws no column it has no number for.
            let Some(&column) = DIACRITICS.get(usize::from(x - drawn.x)) else {
                break;
            };
            symbol.clear();
            symbol.extend([PLACEHOLDER, row, column, high]);
            buf[(x, y)].set_symbol(&symbol);
        }
    }
}

/// Writes `codes`, which show nothing, to the terminal at once. Nothing is
/// done if that fails: the terminal is gone, and tuitube quits.
fn send(out: &mut impl Write, codes: &[String]) {
    let _ = codes
        .iter()
        .try_for_each(|code| out.write_all(code.as_bytes()))
        .and_then(|()| out.flush());
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
    if reader.format() == Some(WebP) {
        check_webp(data)?;
    }
    reader.limits(limits());
    Ok(reader.decode()?)
}

/// The widest and tallest picture decoded, as in [`limits`].
const MAX_SIDE: u32 = 2048;

/// A WebP keeps its size in more than one place: the canvas (which
/// [`limits`] checks) and each frame's own header, from which the decoder
/// sizes its buffers before comparing the two. So a small file saying 1×1
/// outside and 16383×16383 inside would take hundreds of MB. Every frame
/// header is checked here first, against the canvas and [`MAX_SIDE`].
fn check_webp(data: &[u8]) -> Result<()> {
    let chunks = data.get(12..).context("a cut-off WebP picture")?;
    check_webp_chunks(chunks, MAX_SIDE, MAX_SIDE, false)
}

fn check_webp_chunks(mut rest: &[u8], max_w: u32, max_h: u32, in_frame: bool) -> Result<()> {
    let (mut max_w, mut max_h) = (max_w, max_h);
    let le24 = |b: &[u8]| u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16;
    for _ in 0..64 {
        if rest.len() < 8 {
            return Ok(());
        }
        let size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let payload = rest
            .get(8..8usize.saturating_add(size))
            .context("a cut-off WebP chunk")?;
        let (w, h) = match &rest[..4] {
            b"VP8X" if !in_frame => {
                let p = payload.get(..10).context("a cut-off WebP header")?;
                let (w, h) = (le24(&p[4..7]) + 1, le24(&p[7..10]) + 1);
                (max_w, max_h) = (w.min(max_w), h.min(max_h));
                (w, h)
            }
            b"VP8 " => {
                // A key frame: its tag, the start code, then 14-bit sizes.
                let p = payload.get(..10).context("a cut-off WebP frame")?;
                if p[0] & 1 != 0 || p[3..6] != [0x9d, 0x01, 0x2a] {
                    bail!("not a WebP key frame");
                }
                let size = |b: &[u8]| u32::from(u16::from_le_bytes([b[0], b[1]]) & 0x3fff);
                (size(&p[6..8]), size(&p[8..10]))
            }
            b"VP8L" => {
                let p = payload.get(..5).context("a cut-off WebP frame")?;
                if p[0] != 0x2f {
                    bail!("not a WebP lossless frame");
                }
                let bits = u32::from_le_bytes([p[1], p[2], p[3], p[4]]);
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            b"ANMF" if !in_frame => {
                let p = payload.get(..16).context("a cut-off WebP frame")?;
                let (w, h) = (le24(&p[6..9]) + 1, le24(&p[9..12]) + 1);
                if w <= max_w && h <= max_h {
                    check_webp_chunks(&payload[16..], w, h, true)?;
                }
                (w, h)
            }
            b"VP8X" | b"ANMF" => bail!("a WebP chunk where it can't be"),
            _ => (0, 0),
        };
        if w > max_w || h > max_h {
            bail!("a WebP frame bigger than its picture ({w}×{h})");
        }
        let next = 8usize.saturating_add(size).saturating_add(size & 1);
        rest = rest.get(next..).unwrap_or_default();
    }
    bail!("a WebP picture in too many pieces")
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
pub fn cell_size() -> Option<FontSize> {
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
pub fn cell_size() -> Option<FontSize> {
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
    /// Encoded images, ready to draw.
    ready: HashMap<Key, Ready>,
    frame: u64,
    building: HashMap<Key, Instant>,
    failed: HashSet<Key>,
    /// Images the last frame showed, with the URLs to try, best first.
    wanted: Vec<(Key, Vec<String>)>,
    /// Pictures already at hand (the demo's), used instead of downloads.
    preloaded: HashMap<Subject, Arc<DynamicImage>>,
    /// Nothing is downloaded: only preloaded pictures show (`--demo`).
    offline: bool,
    /// Where the last frame drew each image: for the demo's screenshot, and
    /// so the animations leave pictures be (`ui::effects`; kitty's are
    /// placeholders whose color is the image's id). Every image drawn must
    /// be placed (`place`).
    pub placed: Vec<Placement>,
    /// Sixel and iTerm2's pictures are fetched and made ready but not drawn
    /// (`hold`).
    holding: bool,
    /// How many pictures the last frame asked for that weren't ready and
    /// hadn't failed; `None` before a frame.
    missing: Option<usize>,
    /// The number the last image built for kitty's protocol was given.
    kitty_id: u32,
    /// Images dropped since the terminal was last told.
    dropped: Dropped,
    /// See [`KITTY_BUDGET`]; smaller in tests.
    kitty_budget: usize,
    /// What sends the pictures drawn for the first time this frame, taken
    /// out of it: see [`draw`](Self::draw).
    transmits: Vec<String>,
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
            holding: false,
            missing: None,
            kitty_id: kitty_id_base(),
            dropped: Dropped::default(),
            kitty_budget: KITTY_BUDGET,
            transmits: Vec::new(),
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

    /// Draws with `protocol` from now on: for a test of what a terminal with
    /// real images gets.
    #[cfg(test)]
    pub fn set_protocol(&mut self, protocol: ProtocolType) {
        self.picker.set_protocol_type(protocol);
    }

    /// How many of the pictures kept were sent to a terminal speaking
    /// kitty's protocol.
    #[cfg(test)]
    pub fn sent(&self) -> usize {
        self.ready
            .values()
            .filter(|r| r.encoded.kitty.as_ref().is_some_and(|k| k.sent))
            .count()
    }

    /// Takes what sends the pictures drawn for the first time since, as
    /// [`send_pictures`](Self::send_pictures) would.
    #[cfg(test)]
    pub fn take_transmits(&mut self) -> Vec<String> {
        std::mem::take(&mut self.transmits)
    }

    /// Builds every preloaded picture the last frame wanted, now, on this
    /// thread: for drawing a frame with them in a test.
    #[cfg(test)]
    pub fn build_wanted_now(&mut self) {
        for (key, _) in std::mem::take(&mut self.wanted) {
            if self.ready.contains_key(&key) {
                continue;
            }
            if let Some(photo) = self.preloaded.get(&key.subject).cloned() {
                let kitty = self.next_kitty();
                if let Ok(image) = encode(&self.picker, &key, &photo, kitty) {
                    self.add_ready(key, image);
                }
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

    /// Draws `key`'s picture in `area` and says so, if it's ready; else it's
    /// fetched after the frame, from the first of `urls` that has it. With
    /// `clip`, one bigger than `area` is cut to it (ratatui-image's
    /// `allow_clipping`).
    ///
    /// Kitty's protocol sends a picture with its first drawing only, as
    /// code in the first cell it draws, where anything drawn over it later
    /// in the frame (a popup, the picture-in-picture box, a badge) would wipe
    /// it out for good, as would ratatui's diff skipping that cell after a
    /// wide character. So that code is taken out of the cell, leaving its
    /// placeholder, and sent apart once the frame is on the terminal
    /// ([`send_pictures`](Self::send_pictures)). Each placeholder says
    /// where it is in the picture ([`number_placeholders`]), so the rest of
    /// a row shows after what's drawn over it.
    pub fn draw(
        &mut self,
        buf: &mut Buffer,
        key: &Key,
        urls: &[String],
        area: Rect,
        clip: bool,
    ) -> bool {
        let held = self.holding && self.paints_over();
        let Some(ready) = self.ready.get_mut(key) else {
            if !self.failed.contains(key) {
                *self.missing.get_or_insert(0) += 1;
            }
            self.wanted.push((key.clone(), urls.to_vec()));
            return false;
        };
        ready.used = self.frame;
        // Kept for when they can be drawn.
        if held {
            return false;
        }
        // ratatui-image puts what sends a kitty picture in its first cell,
        // and counts it sent even where the buffer has no such cell.
        if area.is_empty() || !buf.area.contains(area.as_position()) {
            return false;
        }
        let area = area.intersection(buf.area);
        let image = &ready.encoded.image;
        let size = image.size();
        // ratatui-image draws nothing where it doesn't fit uncut.
        if !clip && (size.width > area.width || size.height > area.height) {
            return false;
        }
        Image::new(image).allow_clipping(clip).render(area, buf);
        if matches!(image, Protocol::Kitty(_)) {
            if let Some(code) = buf.cell_mut(area.as_position()).and_then(take_transmit) {
                self.transmits.push(code);
                if let Some(kitty) = ready.encoded.kitty.as_mut() {
                    kitty.sent = true;
                }
            }
            let drawn = Rect {
                width: area.width.min(size.width),
                height: area.height.min(size.height),
                ..area
            };
            number_placeholders(buf, drawn);
        }
        true
    }

    /// While the splash screen covers the frame (`ui::splash`): pictures are
    /// fetched and made ready as usual, but those the terminal paints over
    /// what's drawn on them (sixel, iTerm2's protocol) aren't drawn, as they
    /// would show over the splash. Kitty's are drawn and sent under it, what
    /// sends them going apart from the frame ([`draw`](Self::draw)).
    pub fn hold(&mut self, holding: bool) {
        self.holding = holding;
    }

    /// A frame begins: where it draws pictures, and what it misses, are
    /// noted afresh.
    pub fn new_frame(&mut self) {
        self.placed.clear();
        self.missing = Some(0);
    }

    /// How many pictures the last frame asked for that aren't ready yet
    /// (failed ones don't count); `None` before a frame.
    pub fn missing(&self) -> Option<usize> {
        self.missing
    }

    pub fn is_broken(&self, key: &Key) -> bool {
        self.failed.contains(key)
    }

    /// Starts downloads and encodes for what the last frame wanted, and
    /// keeps what it sent within the budget.
    pub fn fetch(&mut self) {
        self.frame += 1;
        self.keep_within_budget();
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

    fn spawn_preloaded(&mut self, key: Key, photo: Arc<DynamicImage>) {
        let picker = self.picker.clone();
        let tx = self.tx.clone();
        let kitty = self.next_kitty();
        let decoding = self.decoding.clone();
        tokio::spawn(async move {
            let Ok(_permit) = decoding.acquire_owned().await else {
                return;
            };
            let build_key = key.clone();
            let result = tokio::task::spawn_blocking(move || {
                contained(|| encode(&picker, &build_key, &photo, kitty))
            })
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("the image couldn't be drawn")));
            let _ = tx.send(AppEvent::Image(ImageEvent { key, result }));
        });
    }

    fn spawn(&mut self, key: Key, urls: Vec<String>) {
        let picker = self.picker.clone();
        let tx = self.tx.clone();
        let http = self.http.clone();
        let downloads = self.downloads.clone();
        let cache = self
            .cache_dir
            .as_ref()
            .map(|dir| dir.join(cache_name(&key.subject)));
        let kitty = self.next_kitty();
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
                contained(|| build(&picker, &build_key, &bytes, kitty))
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
            Ok(image) => self.add_ready(event.key, image),
            Err(_) => {
                self.failed.insert(event.key);
            }
        }
    }

    /// Keeps `image` to draw, making room for it within [`MAX_READY`], but
    /// never from what the last frame drew or asked for: that would be asked
    /// for again at once, made again and push out another, frame after
    /// frame. Those may go past the cap. One made again of a picture still
    /// in use is dropped instead, unsent, rather than the one on screen.
    fn add_ready(&mut self, key: Key, image: Encoded) {
        if let Some(kept) = self.ready.get(&key) {
            if kept.used >= self.last_frame() {
                return;
            }
            // One it replaces counts no more.
            self.ready.remove(&key);
        }
        if self.ready.len() >= MAX_READY
            && let Some(oldest) = self.first_to_go(|_, _| Some(0))
        {
            self.ready.remove(&oldest);
        }
        let ready = Ready {
            encoded: image,
            used: self.frame,
        };
        self.ready.insert(key, ready);
    }

    /// Keeps what the pictures sent take in a terminal speaking kitty's
    /// protocol within [`KITTY_BUDGET`]. Only drawing sends one, so it's done
    /// after each frame, dropping sent pictures that frame didn't draw (those
    /// on screen may go past it): large ones first (over a sixteenth of the
    /// budget, 10 MB: the watch page's picture, not a card's thumbnail), as
    /// they free the most and the watch page shows one at a time; channel
    /// photos last, as they take little and each is drawn on many cards.
    fn keep_within_budget(&mut self) {
        let large = self.kitty_budget / 16;
        let mut taken: usize = self.ready.values().map(|r| r.encoded.sent_bytes()).sum();
        while taken > self.kitty_budget
            && let Some(key) = self.first_to_go(|key, ready| {
                let bytes = ready.encoded.sent_bytes();
                (bytes > 0).then_some(match key.subject {
                    _ if bytes > large => 0,
                    Subject::Thumbnail(_) => 1,
                    Subject::Avatar(_) => 2,
                })
            })
            && let Some(gone) = self.ready.remove(&key)
        {
            taken -= gone.encoded.sent_bytes();
        }
    }

    /// Of the images kept that the last frame neither drew nor asked for,
    /// and that `rank` ranks, the one to drop first: the lowest rank, then
    /// the one drawn longest ago.
    fn first_to_go(&self, rank: impl Fn(&Key, &Ready) -> Option<u8>) -> Option<Key> {
        let last_frame = self.last_frame();
        self.ready
            .iter()
            .filter(|(_, ready)| ready.used < last_frame)
            .filter_map(|(key, ready)| Some(((rank(key, ready)?, ready.used), key)))
            .min_by_key(|&(order, _)| order)
            .map(|(_, key)| key.clone())
    }

    /// The frame drawn last: `fetch`, after each frame, counts the next one.
    fn last_frame(&self) -> u64 {
        self.frame.saturating_sub(1)
    }

    /// Drops every encoded image, as tuitube quits: a terminal speaking
    /// kitty's protocol would keep them after it's gone.
    pub fn drop_all(&mut self) {
        self.ready.clear();
    }

    /// Has a terminal speaking kitty's protocol delete the images dropped
    /// since it was last told, freeing what they took there; see
    /// [`KITTY_BUDGET`]. Written to the terminal (`out`) before each frame,
    /// ahead of any image the frame sends.
    pub fn send_deletions(&mut self, out: &mut impl Write) {
        if let Some(code) = self.deletions() {
            send(out, &[code]);
        }
    }

    /// Sends the pictures the frame just drawn drew for the first time
    /// ([`draw`](Self::draw)), once it's on the terminal: its placeholders
    /// show each picture as soon as it's there, as sending one also places
    /// it where they say.
    pub fn send_pictures(&mut self, out: &mut impl Write) {
        let codes = std::mem::take(&mut self.transmits);
        if !codes.is_empty() {
            send(out, &codes);
        }
    }

    /// The code [`send_deletions`](Self::send_deletions) sends.
    fn deletions(&mut self) -> Option<String> {
        let ids = std::mem::take(&mut *self.dropped.lock().unwrap_or_else(PoisonError::into_inner));
        if ids.is_empty() {
            return None;
        }
        let tmux = self.picker.tmux_detected();
        Some(ids.into_iter().map(|id| kitty_delete(id, tmux)).collect())
    }

    /// A number for the next image built with kitty's protocol, and where
    /// it goes once that image is dropped.
    fn next_kitty(&mut self) -> Option<(u32, Dropped)> {
        (self.picker.protocol_type() == ProtocolType::Kitty).then(|| {
            // Never 0, which kitty's protocol takes as no number.
            self.kitty_id = self.kitty_id.wrapping_add(1).max(1);
            (self.kitty_id, self.dropped.clone())
        })
    }

    /// Forgets failures, so a refresh tries those images again.
    pub fn retry_failed(&mut self) {
        self.failed.clear();
    }

    /// Makes the picture mpv shows in Now Playing for `id`, and says where
    /// it is with `AppEvent::Cover` for playback `play`. It's the thumbnail,
    /// cached or downloaded (from the first of `urls` that has it) and
    /// decoded as a card's is, then cut square and written as a JPEG of
    /// tuitube's own: mpv and macOS read that file, never YouTube's bytes or
    /// a URL (mpv would fetch one itself).
    pub fn cover(&self, id: &VideoId, mut urls: Vec<String>, play: u64) {
        let Some(dir) = self.cache_dir.clone() else {
            return;
        };
        urls.retain(|url| image_url_allowed(url));
        if self.offline || urls.is_empty() {
            return;
        }
        let cache = dir.join(cache_name(&Subject::Thumbnail(id.clone())));
        let path = dir.join(cover_name(id));
        let (http, tx) = (self.http.clone(), self.tx.clone());
        let (downloads, decoding) = (self.downloads.clone(), self.decoding.clone());
        tokio::spawn(async move {
            let Ok((data, from_cache)) = load(&http, &downloads, &urls, Some(&cache)).await else {
                return;
            };
            let Ok(_permit) = decoding.acquire_owned().await else {
                return;
            };
            let data = Arc::new(data);
            let bytes = data.clone();
            let jpeg = tokio::task::spawn_blocking(move || contained(|| cover_jpeg(&bytes)))
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("the image couldn't be decoded")));
            // Kept for the card as a card's own download is.
            match (&jpeg, from_cache) {
                (Ok(_), false) => {
                    let _ = write_private(&cache, &data).await;
                }
                (Err(_), true) => {
                    let _ = tokio::fs::remove_file(&cache).await;
                }
                _ => {}
            }
            if let Ok(jpeg) = jpeg
                && write_private(&path, &jpeg).await.is_ok()
            {
                let _ = tx.send(AppEvent::Cover { play, path });
            }
        });
    }
}

/// The side of the Now Playing picture, in pixels: notch apps and Control
/// Center draw it square, at most a few hundred points across.
const COVER_SIDE: u32 = 600;

/// How long a Now Playing picture is kept: mpv reads it when it's given.
const COVER_FOR: Duration = Duration::from_secs(24 * 3600);

/// The Now Playing picture: `data` decoded, cut square from its middle,
/// encoded as a JPEG.
fn cover_jpeg(data: &[u8]) -> Result<Vec<u8>> {
    let square = fill(&decode(data)?, COVER_SIDE, COVER_SIDE).to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&square)?;
    Ok(jpeg)
}

/// The Now Playing picture's file: named by the checked id only.
fn cover_name(id: &VideoId) -> String {
    format!("p-{id}.jpg")
}

/// Decodes `data` and encodes it for the terminal at `key`'s size: a
/// thumbnail cropped to fill, a channel photo cut to a circle.
fn build(
    picker: &Picker,
    key: &Key,
    data: &[u8],
    kitty: Option<(u32, Dropped)>,
) -> Result<Encoded> {
    encode(picker, key, &decode(data)?, kitty)
}

/// Encodes a decoded picture for the terminal at `key`'s size.
fn encode(
    picker: &Picker,
    key: &Key,
    photo: &DynamicImage,
    kitty: Option<(u32, Dropped)>,
) -> Result<Encoded> {
    let font = picker.font_size();
    let (width, height) = (
        u32::from(key.cols) * u32::from(font.width),
        u32::from(key.rows) * u32::from(font.height),
    );
    let image = match key.subject {
        Subject::Thumbnail(_) => fill(photo, width, height),
        Subject::Avatar(_) => circle(photo, width, height).into(),
    };
    protocol(picker, image, Size::new(key.cols, key.rows), kitty)
}

/// How pictures fit their cells.
const RESIZE: Resize = Resize::Fit(None);

/// `image` as the terminal is sent it in `size` cells. With kitty's
/// protocol, one made to fill them exactly, as `encode` makes them all, is
/// sent as the image numbered by `kitty`, picked here rather than by the
/// picker, so it can be deleted from the terminal once it's dropped; the
/// picker's `new_protocol` takes such a picture as it is, too. Any other is
/// the picker's own, and isn't deleted.
fn protocol(
    picker: &Picker,
    image: DynamicImage,
    size: Size,
    kitty: Option<(u32, Dropped)>,
) -> Result<Encoded> {
    let font = picker.font_size();
    let fills = (image.width(), image.height())
        == (
            u32::from(size.width) * u32::from(font.width),
            u32::from(size.height) * u32::from(font.height),
        );
    let Some((id, dropped)) = kitty.filter(|_| fills) else {
        let image = picker.new_protocol(image, size, RESIZE)?;
        return Ok(Encoded { image, kitty: None });
    };
    let bytes = image.width() as usize * image.height() as usize * 4;
    let compress = picker
        .capabilities()
        .contains(&Capability::KittyCompression);
    let image = Kitty::new(image, size, id, picker.tmux_detected(), compress)?;
    let kitty = KittyImage {
        id,
        bytes,
        dropped,
        sent: false,
    };
    Ok(Encoded {
        image: Protocol::Kitty(image),
        kitty: Some(kitty),
    })
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

/// Deletes cached pictures fetched more than `CACHE_FOR` ago (Now Playing's
/// after `COVER_FOR`), leftovers of interrupted writes, and the 480×360
/// thumbnails the first version cached (`v-…`), so the cache doesn't grow
/// without end.
fn tidy_cache(dir: &std::path::Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let kept_for = if name.starts_with("p-") {
            COVER_FOR
        } else {
            CACHE_FOR
        };
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_none_or(|age| age > kept_for);
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
    fn now_playings_picture_is_a_square_jpeg_of_tuitubes_own() {
        let jpeg = cover_jpeg(&png(1280, 720)).unwrap();
        let picture = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
        assert_eq!(
            (picture.width(), picture.height()),
            (COVER_SIDE, COVER_SIDE)
        );
        assert!(cover_jpeg(b"<svg/>").is_err(), "only what decodes");
        assert!(cover_jpeg(&png(3000, 1)).is_err(), "within the limits");
        let id = VideoId::parse("dQw4w9WgXcQ").unwrap();
        assert_eq!(cover_name(&id), "p-dQw4w9WgXcQ.jpg");
    }

    #[test]
    fn now_playings_pictures_are_kept_a_day_thumbnails_a_month() {
        let dir = std::env::temp_dir().join(format!("tuitube-covers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::config::private_dir(&dir).unwrap();
        let day_ago = std::time::SystemTime::now() - Duration::from_secs(25 * 3600);
        for name in ["p-old.jpg", "p-new.jpg", "t-old"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        for name in ["p-old.jpg", "t-old"] {
            let file = std::fs::File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap();
            file.set_modified(day_ago).unwrap();
        }
        tidy_cache(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["p-new.jpg", "t-old"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_now_playings_picture_is_made_from_the_thumbnail() {
        let dir = std::env::temp_dir().join(format!("tuitube-live-cover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::config::private_dir(&dir).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let images = Images::new(
            Picker::halfblocks(),
            tx,
            crate::feed::client(),
            Some(dir.clone()),
        );
        let id = VideoId::parse("jNQXAC9IVRw").unwrap();
        images.cover(&id, vec![id.large_thumbnail_url(), id.thumbnail_url()], 7);
        let event = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("in time")
            .expect("an answer");
        let AppEvent::Cover { play, path } = event else {
            panic!("not a cover");
        };
        assert_eq!((play, path.clone()), (7, dir.join("p-jNQXAC9IVRw.jpg")));
        let picture = image::open(&path).unwrap();
        assert_eq!(
            (picture.width(), picture.height()),
            (COVER_SIDE, COVER_SIDE)
        );
        assert!(dir.join("t-jNQXAC9IVRw").exists(), "kept for the card too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn images_far_bigger_than_youtube_sends_are_refused() {
        assert!(decode(&png(512, 512)).is_ok());
        assert!(decode(&png(3000, 1)).is_err());
    }

    /// A RIFF/WEBP file made of these chunks.
    fn webp(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (fourcc, payload) in chunks {
            body.extend_from_slice(*fourcc);
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend(body);
        file
    }

    /// A lossy key frame's first bytes, saying it's `w`×`h`.
    fn vp8(w: u16, h: u16) -> Vec<u8> {
        let mut p = vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a];
        p.extend_from_slice(&w.to_le_bytes());
        p.extend_from_slice(&h.to_le_bytes());
        p.extend_from_slice(&[0; 8]);
        p
    }

    fn vp8x(w: u32, h: u32) -> Vec<u8> {
        let mut p = vec![0; 4];
        p.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        p.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        p
    }

    #[test]
    fn a_webp_whose_frame_is_bigger_than_it_says_is_refused_before_decoding() {
        // 1×1 outside, 16383×16383 inside: about 384 MB if decoded.
        let sneaky = webp(&[(b"VP8X", vp8x(1, 1)), (b"VP8 ", vp8(16383, 16383))]);
        assert!(sneaky.len() < 64);
        let error = decode(&sneaky).unwrap_err().to_string();
        assert!(error.contains("bigger than its picture"), "{error}");
        let huge = webp(&[(b"VP8 ", vp8(3000, 3000))]);
        assert!(decode(&huge).is_err());
        let mut lossless = vec![0x2f];
        lossless.extend_from_slice(&((4095u32) | (4095 << 14)).to_le_bytes());
        assert!(check_webp(&webp(&[(b"VP8L", lossless)])).is_err());
        let mut frame = vec![0; 6];
        frame.extend_from_slice(&vp8x(64, 64)[4..]);
        frame.extend_from_slice(&[0; 4]);
        frame.extend(webp(&[(b"VP8 ", vp8(2000, 2000))])[12..].iter());
        let animated = webp(&[(b"VP8X", vp8x(64, 64)), (b"ANMF", frame)]);
        assert!(
            check_webp(&animated).is_err(),
            "a frame inside an animation"
        );
        // Sizes that fit are let through to the decoder.
        assert!(
            check_webp(&webp(&[
                (b"VP8X", vp8x(1280, 720)),
                (b"VP8 ", vp8(1280, 720))
            ]))
            .is_ok()
        );
        let mut real = Vec::new();
        DynamicImage::new_rgba8(4, 3)
            .write_to(
                &mut std::io::Cursor::new(&mut real),
                image::ImageFormat::WebP,
            )
            .unwrap();
        assert_eq!(
            decode(&real).unwrap().width(),
            4,
            "a real WebP still decodes"
        );
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
        assert!(!images.draw(&mut blank(), &key, &urls, Rect::new(0, 0, 4, 2), false));
        images.fetch();
        assert!(images.is_broken(&key));
    }

    /// Images for a terminal that draws only colored blocks, with cells of
    /// 10 × 20 px.
    fn blocks() -> Images {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Images::new(Picker::halfblocks(), tx, crate::feed::client(), None)
    }

    /// Images for a terminal speaking kitty's protocol, its numbers from 1.
    fn kitty() -> Images {
        let mut images = blocks();
        images.set_protocol(ProtocolType::Kitty);
        images.kitty_id = 0;
        images
    }

    /// Video `n`'s thumbnail, `cols` × `rows` cells.
    fn thumbnail(n: usize, cols: u16, rows: u16) -> Key {
        let id = VideoId::parse(&format!("video{n:06}")).unwrap();
        Key {
            subject: Subject::Thumbnail(id),
            cols,
            rows,
        }
    }

    /// A card's thumbnail: 4 × 2 cells, 6400 bytes of pixels.
    fn card(n: usize) -> Key {
        thumbnail(n, 4, 2)
    }

    /// Channel `n`'s photo, as big as a card's thumbnail.
    fn channel_photo(n: usize) -> Key {
        let id = ChannelId::parse(&format!("UC{n:022}")).unwrap();
        Key {
            subject: Subject::Avatar(id),
            cols: 4,
            rows: 2,
        }
    }

    /// `key`'s picture, encoded as on a background thread, then kept.
    fn keep(images: &mut Images, key: &Key) {
        let kitty = images.next_kitty();
        let photo = DynamicImage::new_rgba8(16, 9);
        let image = encode(&images.picker, key, &photo, kitty).unwrap();
        images.on_built(ImageEvent {
            key: key.clone(),
            result: Ok(image),
        });
    }

    /// A blank screen of 20 × 10 cells.
    fn blank() -> Buffer {
        Buffer::empty(Rect::new(0, 0, 20, 10))
    }

    /// Draws `key`'s picture on `screen` with its first cell at `(x, y)`.
    fn draw_at(images: &mut Images, screen: &mut Buffer, key: &Key, x: u16, y: u16) -> bool {
        let area = Rect::new(x, y, key.cols, key.rows);
        images.draw(screen, key, &[], area, false)
    }

    /// Draws `key`'s picture at the top left of a screen of its own, and
    /// gives the screen if it was.
    fn drawn_alone(images: &mut Images, key: &Key) -> Option<Buffer> {
        let mut screen = blank();
        draw_at(images, &mut screen, key, 0, 0).then_some(screen)
    }

    /// Some cell of `screen` holds code for the terminal.
    fn holds_code(screen: &Buffer) -> bool {
        screen.content().iter().any(|c| c.symbol().contains('\x1b'))
    }

    /// `image` drawn in 6 × 3 cells, by ratatui-image.
    fn rendered(image: &Protocol) -> Buffer {
        let area = Rect::new(0, 0, 6, 3);
        let mut buf = Buffer::empty(area);
        Image::new(image).render(area, &mut buf);
        buf
    }

    fn delete(id: u32) -> String {
        format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
    }

    #[test]
    fn a_kitty_picture_is_made_as_the_pickers_own_but_for_its_number() {
        let picker = kitty().picker;
        // Made to fill its cells of 10 × 20 px, as `encode` makes them all.
        for (cols, rows) in [(6, 3), (4, 2), (1, 1)] {
            let size = Size::new(cols, rows);
            let (w, h) = (u32::from(cols) * 10, u32::from(rows) * 20);
            let image: DynamicImage =
                RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 7, 255])).into();
            let theirs = rendered(&picker.new_protocol(image.clone(), size, RESIZE).unwrap());
            // The number the picker picked, from the code that sends it.
            let id = theirs[(0, 0)]
                .symbol()
                .split("i=")
                .nth(1)
                .and_then(|rest| rest.split(',').next())
                .and_then(|id| id.parse().ok())
                .unwrap();
            let ours = protocol(&picker, image, size, Some((id, Dropped::default()))).unwrap();
            assert!(ours.kitty.is_some());
            assert_eq!(rendered(&ours.image), theirs, "{cols}×{rows}");
        }
        // Any other is the picker's own, never deleted: `new_protocol` would
        // enlarge this one to its cells.
        let odd = DynamicImage::new_rgba8(55, 55);
        let ours = protocol(&picker, odd, Size::new(6, 3), Some((1, Dropped::default()))).unwrap();
        assert!(ours.kitty.is_none());
    }

    #[test]
    fn every_cell_of_a_kitty_picture_says_where_it_is_in_it() {
        let mut images = kitty();
        // A number with a high byte, which the third mark carries.
        images.kitty_id = 0x0500_0000;
        let key = thumbnail(1, 6, 3);
        keep(&mut images, &key);
        let mut screen = blank();
        assert!(draw_at(&mut images, &mut screen, &key, 2, 1));
        let says = |screen: &Buffer, column: u16, row: u16| {
            let cell = &screen[(2 + column, 1 + row)];
            let numbers = [row, column, 5].map(|n| DIACRITICS[usize::from(n)]);
            assert_eq!(
                cell.symbol(),
                String::from_iter([PLACEHOLDER].into_iter().chain(numbers)),
                "({column}, {row})"
            );
            assert_eq!(cell.fg, screen[(2, 1)].fg, "the picture's color");
        };
        for (column, row) in (0..6).flat_map(|column| (0..3).map(move |row| (column, row))) {
            says(&screen, column, row);
        }

        // Cut: the cells drawn, and nothing past them.
        let mut screen = blank();
        assert!(images.draw(&mut screen, &key, &[], Rect::new(2, 1, 4, 2), true));
        for (column, row) in (0..4).flat_map(|column| (0..2).map(move |row| (column, row))) {
            says(&screen, column, row);
        }
        assert_eq!(screen[(6, 1)], blank()[(6, 1)]);
        assert_eq!(screen[(2, 3)], blank()[(2, 3)]);

        // One that doesn't fit uncut, past the screen's edge, isn't drawn.
        let mut screen = blank();
        assert!(!images.draw(&mut screen, &key, &[], Rect::new(16, 8, 6, 3), false));
        assert_eq!(screen, blank());
    }

    /// The numbers are ratatui-image's: the first placeholder of each row
    /// it draws has that row's, for every row it can number. It numbers
    /// columns and the id's high byte from the same table.
    #[test]
    fn the_numbers_are_ratatui_images() {
        let rows = DIACRITICS.len() as u16;
        let picture = DynamicImage::new_rgba8(1, 1);
        let image = Kitty::new(picture, Size::new(1, rows), 1, false, false).unwrap();
        let area = Rect::new(0, 0, 1, rows);
        let mut screen = Buffer::empty(area);
        Image::new(&Protocol::Kitty(image)).render(area, &mut screen);
        for (row, number) in DIACRITICS.iter().enumerate() {
            // The first holds what sends the picture before it.
            let symbol = screen[(0, row as u16)].symbol();
            let placeholder = &symbol[symbol.rfind(PLACEHOLDER).unwrap()..];
            assert_eq!(placeholder.chars().nth(1), Some(*number), "row {row}");
        }
    }

    #[test]
    fn a_kitty_terminal_is_told_to_delete_the_pictures_dropped_by_the_number_they_were_sent_as() {
        let mut images = kitty();
        keep(&mut images, &card(1));
        drawn_alone(&mut images, &card(1)).expect("drawn");
        assert!(images.take_transmits()[0].contains("i=1,a=T"), "sent as 1");
        assert_eq!(images.deletions(), None, "nothing dropped yet");

        // Made again once it's off screen: the one it replaces goes.
        images.frame = 2;
        keep(&mut images, &card(1));
        assert_eq!(images.deletions(), Some(delete(1)));
        assert_eq!(images.deletions(), None, "told once");

        // All those sent as tuitube quits, written to the terminal: one
        // never drawn was never sent.
        drawn_alone(&mut images, &card(1)).expect("drawn");
        keep(&mut images, &card(2));
        images.drop_all();
        let mut out = Vec::new();
        images.send_deletions(&mut out);
        assert_eq!(String::from_utf8(out).unwrap(), delete(2));
    }

    #[test]
    fn a_picture_on_screen_is_kept_rather_than_one_made_again_of_it() {
        let mut images = kitty();
        keep(&mut images, &card(1));
        drawn_alone(&mut images, &card(1)).expect("drawn");
        assert_eq!(images.take_transmits().len(), 1);
        images.fetch();
        // Made again meanwhile: dropped unsent, and the one sent stays.
        keep(&mut images, &card(1));
        assert_eq!(images.deletions(), None);
        let again = drawn_alone(&mut images, &card(1)).expect("drawn");
        assert!(images.take_transmits().is_empty(), "sent already");
        assert_eq!(
            again[(0, 0)].fg,
            ratatui::style::Color::Rgb(0, 0, 1),
            "as number 1"
        );
    }

    #[test]
    fn pictures_sent_to_a_kitty_terminal_stay_within_its_budget_but_not_by_dropping_those_on_screen()
     {
        let mut images = kitty();
        images.kitty_budget = 2 * 6400;
        for n in 1..=3 {
            keep(&mut images, &card(n));
        }
        // Not sent yet, they take nothing there.
        images.fetch();
        assert_eq!(images.ready.len(), 3);
        // A frame draws all three: on screen, they stay.
        let mut screen = blank();
        for n in 1..=3 {
            assert!(draw_at(&mut images, &mut screen, &card(n), 5 * n as u16, 0));
        }
        images.fetch();
        assert_eq!(images.ready.len(), 3);
        assert_eq!(images.deletions(), None);
        // The next draws the second and third: the first goes.
        for n in 2..=3 {
            assert!(draw_at(&mut images, &mut screen, &card(n), 5 * n as u16, 0));
        }
        images.fetch();
        assert!(!images.ready.contains_key(&card(1)), "drawn longest ago");
        assert_eq!(images.ready.len(), 2);
        assert_eq!(images.deletions(), Some(delete(1)));
    }

    #[test]
    fn the_watch_pages_picture_goes_first_and_channel_photos_last() {
        let mut images = kitty();
        // Sixteen cards' thumbnails: a picture bigger than one is large.
        images.kitty_budget = 16 * 6400;
        // As big as nine cards' thumbnails.
        let page = thumbnail(100, 12, 6);
        // A frame that draws `keys`, each made just before.
        let shown = |images: &mut Images, keys: &[Key]| {
            for key in keys {
                keep(images, key);
                assert!(drawn_alone(images, key).is_some());
            }
            images.fetch();
        };
        shown(&mut images, &[channel_photo(1), card(1)]);
        shown(&mut images, std::slice::from_ref(&page));
        shown(&mut images, &(2..=5).map(card).collect::<Vec<_>>());
        assert_eq!(images.deletions(), None, "all within the budget");
        shown(&mut images, &[card(6), card(7)]);
        assert!(
            !images.ready.contains_key(&page),
            "though not drawn longest ago"
        );
        assert!(images.ready.contains_key(&card(1)));
        assert_eq!(images.deletions(), Some(delete(3)));

        // Then the cards drawn longest ago, before a channel photo drawn as
        // long ago.
        shown(&mut images, &(8..=16).map(card).collect::<Vec<_>>());
        assert!(!images.ready.contains_key(&card(1)));
        assert!(images.ready.contains_key(&channel_photo(1)));
        assert_eq!(images.deletions(), Some(delete(2)));
    }

    #[test]
    fn a_kitty_pictures_first_drawing_leaves_its_placeholder_and_sends_it_apart_once() {
        let mut images = kitty();
        keep(&mut images, &card(1));
        let screen = drawn_alone(&mut images, &card(1)).expect("drawn");
        // Its first cell: the placeholder, its row, column and number.
        let first = screen[(0, 0)].symbol();
        assert!(first.starts_with(PLACEHOLDER), "{first:?}");
        assert_eq!(first.chars().count(), 4, "{first:?}");
        assert!(!holds_code(&screen));
        let mut out = Vec::new();
        images.send_pictures(&mut out);
        let sent = String::from_utf8(out).unwrap();
        assert!(sent.starts_with("\x1b_Gq=2,i=1,a=T"), "{sent:.40}");
        assert!(!sent.contains(PLACEHOLDER));

        // The next frame sends nothing.
        images.fetch();
        drawn_alone(&mut images, &card(1)).expect("drawn");
        let mut out = Vec::new();
        images.send_pictures(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_picture_whose_first_cell_is_covered_in_its_first_frame_is_sent_all_the_same() {
        use ratatui::widgets::Clear;
        let mut images = kitty();
        keep(&mut images, &card(1));
        let mut screen = blank();
        assert!(draw_at(&mut images, &mut screen, &card(1), 4, 4));
        // A popup, or the picture-in-picture box, over its first cell.
        Clear.render(Rect::new(2, 3, 12, 6), &mut screen);
        assert_eq!(images.take_transmits().len(), 1);
        assert_eq!(images.sent(), 1);
        // Drawn again, it isn't sent again; dropped, it's deleted.
        images.fetch();
        assert!(draw_at(&mut images, &mut blank(), &card(1), 4, 4));
        assert!(images.take_transmits().is_empty());
        images.drop_all();
        assert_eq!(images.deletions(), Some(delete(1)));
    }

    #[test]
    fn inside_tmux_the_code_that_deletes_a_picture_goes_through_it() {
        assert_eq!(
            kitty_delete(1, true),
            "\x1bPtmux;\x1b\x1b_Ga=d,d=I,i=1,q=2\x1b\x1b\\\x1b\\"
        );
        assert_eq!(kitty_delete(1, false), delete(1));
    }

    #[test]
    fn terminals_that_keep_no_pictures_are_told_nothing() {
        let mut images = blocks();
        keep(&mut images, &card(1));
        drawn_alone(&mut images, &card(1)).expect("drawn");
        assert!(images.take_transmits().is_empty());
        images.drop_all();
        let mut out = Vec::new();
        images.send_deletions(&mut out);
        assert!(out.is_empty());
    }
}
