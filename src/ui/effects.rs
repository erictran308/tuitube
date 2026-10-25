//! Animations (`animations` in settings, on by default), drawn with
//! tachyonfx over each frame: the screen comes out of the background at
//! start, the player bar sweeps in when a video starts and a light runs
//! across "Loading" while it loads, and quicker across what the video's
//! picture says while it loads or buffers (`under`, before the popups are
//! drawn), and a popup opens out from its middle while the screen behind
//! it dims (`over`, after them, from the popups `ui::clear_popup` noted).
//!
//! Only colors change, never characters (a wide title would shift a column),
//! and never over a picture the terminal draws: kitty's are placeholder
//! characters whose color is the image's id, sixel's and iTerm2's are drawn
//! once over cells the frame then skips, and Pepper's video box is Pepper's.
//! Those cells are put back as they were drawn once the animations ran.
//! Block pictures (halfblocks) are colored cells, and fade and dim with the
//! rest. Under a popup Pepper's box dims too: an older Pepper takes its
//! picture away there (`App::video_placement`), and one that takes holes
//! shows in them the cells drawn over its picture (the controls over its
//! full screen), which dim with the rest.
//!
//! Frames come only while something moves (`Effects::moving`, `App::run`):
//! every `EVERY`, or `SHIMMER_EVERY` while only the lights across
//! "Loading" and the picture's words do, as those last as long as yt-dlp
//! and the player take. The dimming then stays as it ended, drawn
//! with each frame, until the popups close. Never in tests (`Effects.live`),
//! whose frames mustn't change with the time they took to draw.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tachyonfx::pattern::DiagonalPattern;
use tachyonfx::{ColorSpace, Effect, Interpolation, Motion, fx};

use crate::app::App;
use crate::ids::VideoId;
use crate::images::Images;
use crate::theme;

/// What the setting turns on, for the settings tab and the command line.
pub const ABOUT: &str = "Animations: the screen fades in, popups open out, the bar sweeps in";

/// How often a frame is drawn while something moves, and while only the
/// light across "Loading" does.
const EVERY: Duration = Duration::from_millis(16);
const SHIMMER_EVERY: Duration = Duration::from_millis(50);
/// At most this often over ssh (`Effects.remote`): every color of the
/// screen changes as a popup opens, about 830 KB at 60 frames a second.
const REMOTE_EVERY: Duration = Duration::from_millis(33);

/// How long each takes, in milliseconds.
const INTRO_MS: u32 = 900;
const POPUP_MS: u32 = 260;
const DIM_MS: u32 = 200;
const PLAYER_MS: u32 = 700;
/// Once across "Loading", left to right.
const SHIMMER_MS: u32 = 1400;
/// Once across what the video's picture says while it waits: a few words,
/// so quicker.
const WAITING_MS: u32 = 500;
/// How far the screen behind a popup goes toward the background.
const DIM: f32 = 0.6;
/// How soft the edge of a popup opening is, in columns.
const POPUP_EDGE: f32 = 6.0;
/// The light across "Loading": how far it reaches each side, in columns,
/// and how near the text's color gets to the brightest.
const SHIMMER_REACH: f32 = 8.0;
const SHIMMER_LIGHT: f32 = 0.85;
/// How far the light across the picture's words reaches each side.
const WAITING_REACH: f32 = 4.0;

/// What an animation is for: a new one takes the place of the one of its
/// kind. In the order they're drawn: under the popups up to `Dim`, the light
/// under the sweep, so the sweep hides what it hasn't reached yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Intro,
    Loading,
    /// The light across what the video's picture says while it waits:
    /// drawn on its own, as it's inside the picture's box, which the others
    /// leave be.
    Waiting,
    Player,
    /// The screen behind the popups, dimming, or coming back once they
    /// closed.
    Dim,
    /// The popup on top opening.
    Popup,
}

struct Running {
    kind: Kind,
    fx: Effect,
    /// Not drawn yet: it starts from its beginning, whenever the frame
    /// before was.
    fresh: bool,
    /// The dimming's: how far it starts and how far it goes.
    dims: Option<(f32, f32)>,
}

impl Running {
    /// Drawn as it ended until it's replaced: the dimming behind popups,
    /// not the brightening once they closed.
    fn held(&self) -> bool {
        self.dims.is_some_and(|(_, to)| to > 0.0)
    }
}

/// The theme's colors the animations blend with, as they are in this
/// frame: a theme picked behind a popup dims toward its own background.
#[derive(Clone, Copy, Default)]
struct Palette {
    bg: Color,
    panel: Color,
    text: Color,
}

/// A popup, as `ui::clear_popup` notes it. One that opens on top opens out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Popup {
    Keys,
    Info,
    Chapters,
    Groups,
    Question,
}

/// What a frame shows under the popups that animations start from.
#[derive(Clone, Debug, Default)]
pub struct Shown {
    /// The player bar, where it's drawn.
    pub player: Option<Rect>,
    /// The video playing.
    pub video: Option<VideoId>,
    /// The row that says "Loading …", while nothing plays yet.
    pub loading: Option<Rect>,
    /// Where the video's picture says it loads or buffers
    /// (`watch::draw_picture`).
    pub waiting: Option<Rect>,
    /// Pepper's full screen is over all of it: nothing there moves.
    pub covered: bool,
}

#[derive(Default)]
pub struct Effects {
    /// Drawn to a terminal (`for_terminal`), never in tests.
    pub live: bool,
    /// Over ssh: fewer frames, `REMOTE_EVERY`.
    pub remote: bool,
    /// The popups this frame drew, the one on top last, and where
    /// (`ui::clear_popup`).
    pub popups: RefCell<Vec<(Popup, Rect)>>,
    /// The command line's completions, where this frame drew them
    /// (`ui::command_menu`): never dimmed.
    pub menu: Cell<Option<Rect>>,
    running: Vec<Running>,
    palette: Rc<Cell<Palette>>,
    /// What the frame before showed under the popups; `None` before the
    /// first.
    seen: Option<Shown>,
    /// The popups the frame before drew.
    seen_popups: Vec<Popup>,
    /// When the frame before was drawn, and how long it's been since, while
    /// something moved (an idle wait isn't skipped through).
    last: Option<Instant>,
    elapsed: Duration,
}

impl Effects {
    /// For drawing to a terminal (`main`, `--demo`): the animations run,
    /// with fewer frames over ssh.
    pub fn for_terminal() -> Self {
        Self {
            live: true,
            remote: ["SSH_CONNECTION", "SSH_TTY"]
                .iter()
                .any(|name| std::env::var_os(name).is_some()),
            ..Self::default()
        }
    }

    /// Something moves: the next frame is drawn in `every`. The dimming
    /// held behind a popup doesn't move.
    pub fn moving(&self) -> bool {
        self.running.iter().any(|r| r.fx.running())
    }

    /// How long until the next frame, while something moves.
    pub fn every(&self) -> Duration {
        let busy = self
            .running
            .iter()
            .any(|r| !matches!(r.kind, Kind::Loading | Kind::Waiting) && r.fx.running());
        let every = if busy { EVERY } else { SHIMMER_EVERY };
        if self.remote {
            every.max(REMOTE_EVERY)
        } else {
            every
        }
    }

    /// Starts what `shown` shows anew, after `seen`, what the frame before
    /// showed.
    fn start_under(&mut self, seen: Option<&Shown>, shown: &Shown) {
        let bg = self.palette.get().bg;
        let Some(seen) = seen else {
            // The first frame: all of it comes out of the background. Not
            // over ssh, where the first screen would come later for it:
            // every cell changes each frame, about 1 MB (760 KB at
            // `REMOTE_EVERY`), measured at 140×46.
            if !self.remote {
                self.start(Kind::Intro, intro(bg));
            }
            return;
        };
        // The picture's words, over Pepper's full screen too: they're in it.
        self.light(Kind::Waiting, shown.waiting, |_| waiting());
        if shown.covered {
            self.stop(Kind::Player);
            self.stop(Kind::Loading);
            return;
        }
        // The bar coming up, and each new video playing in it; the sweep
        // follows it as it grows (the visualizer) or the window changes.
        let new_video = shown.video.is_some() && shown.video != seen.video;
        // A bar with no room (a window too short) shows nothing to sweep.
        let room = |bar: &Rect| !bar.is_empty();
        match shown.player.filter(room) {
            Some(bar) if seen.player.filter(room).is_none() || new_video => {
                self.start(Kind::Player, player(bg).with_area(bar));
            }
            Some(bar) => self.move_to(Kind::Player, bar),
            None => self.stop(Kind::Player),
        }
        self.light(Kind::Loading, shown.loading, loading);
    }

    /// Lights `place` (with `light` when it starts) for as long as it
    /// shows: not under a popup, which would dim it and redraw the screen
    /// for it 20 times a second to no end; again once animations are turned
    /// back on.
    fn light(
        &mut self,
        kind: Kind,
        place: Option<Rect>,
        light: impl FnOnce(Rc<Cell<Palette>>) -> Effect,
    ) {
        let lit = place.filter(|place| !place.is_empty());
        match lit.filter(|_| self.seen_popups.is_empty()) {
            Some(place) if self.running.iter().any(|r| r.kind == kind) => {
                self.move_to(kind, place);
            }
            Some(place) => {
                let fx = light(self.palette.clone()).with_area(place);
                self.start(kind, fx);
            }
            None => self.stop(kind),
        }
    }

    /// Dims the screen while `popups` are open, and opens out the one on
    /// top if it wasn't open in the frame before (not one uncovered as
    /// the one over it closed).
    fn start_over(&mut self, popups: &[(Popup, Rect)]) {
        let goal = if popups.is_empty() { 0.0 } else { DIM };
        let (now, going) = self.dimming();
        if going != goal && (goal > 0.0 || now > 0.0) {
            // From where it is: popups closed before the screen was all
            // dimmed brighten it from there.
            let dimming = dim(self.palette.clone(), now, goal);
            self.start(Kind::Dim, dimming).dims = Some((now, goal));
        }
        match popups.last() {
            Some(&(top, place)) if !self.seen_popups.contains(&top) && !place.is_empty() => {
                let opening = popup(self.palette.clone()).with_area(place);
                self.start(Kind::Popup, opening);
            }
            Some(&(top, place)) if self.seen_popups.last() == Some(&top) => {
                self.move_to(Kind::Popup, place);
            }
            // Uncovered, or closed: what opened over it stops.
            _ => self.stop(Kind::Popup),
        }
    }

    /// How far the screen is dimmed now, and how far it's going.
    fn dimming(&self) -> (f32, f32) {
        let Some((r, (from, to))) = self
            .running
            .iter()
            .find_map(|r| r.dims.map(|dims| (r, dims)))
        else {
            return (0.0, 0.0);
        };
        let alpha = r.fx.timer().map_or(1.0, |t| t.alpha());
        (from + (to - from) * alpha, to)
    }

    fn start(&mut self, kind: Kind, fx: Effect) -> &mut Running {
        self.stop(kind);
        let at = self.running.partition_point(|r| r.kind < kind);
        let fx = Running {
            kind,
            fx,
            fresh: true,
            dims: None,
        };
        self.running.insert(at, fx);
        &mut self.running[at]
    }

    fn stop(&mut self, kind: Kind) {
        self.running.retain(|r| r.kind != kind);
    }

    /// What `kind` animates moved, or changed size.
    fn move_to(&mut self, kind: Kind, area: Rect) {
        if let Some(r) = self.running.iter_mut().find(|r| r.kind == kind) {
            r.fx.set_area(area);
        }
    }

    /// The time since the frame before, if something moved since then.
    fn tick(&mut self, now: Instant) {
        self.elapsed = match self.last {
            Some(last) if self.moving() => now.saturating_duration_since(last),
            _ => Duration::ZERO,
        };
        self.last = Some(now);
    }

    /// Draws the animations of the kinds `here` takes, then puts back the
    /// cells in `keep` as they were. Each first sees the colors its cells
    /// show (`settle`).
    fn draw(
        &mut self,
        here: impl Fn(Kind) -> bool,
        buf: &mut Buffer,
        keep: impl FnOnce() -> Vec<Rect>,
    ) {
        if !self.running.iter().any(|r| here(r.kind)) {
            return;
        }
        let screen = buf.area;
        // Only what an animation here would touch is kept.
        let touched: Vec<Rect> = self
            .running
            .iter()
            .filter(|r| here(r.kind))
            .map(|r| r.fx.area().unwrap_or(screen))
            .collect();
        // Only colors are animated, so only colors are kept (a kitty
        // picture's first cell can hold the whole picture, as text).
        let kept: Vec<(Rect, Vec<(Color, Color)>)> = keep()
            .into_iter()
            .map(|r| r.intersection(screen))
            .filter(|r| touched.iter().any(|t| t.intersects(*r)))
            .map(|r| (r, r.positions().map(|p| (buf[p].fg, buf[p].bg)).collect()))
            .collect();
        let palette = self.palette.get();
        for r in self.running.iter_mut().filter(|r| here(r.kind)) {
            // Within the window, should it have shrunk.
            if let Some(area) = r.fx.area() {
                r.fx.set_area(area.intersection(screen));
            }
            settle(buf, r.fx.area().unwrap_or(screen), palette);
            let elapsed = if r.fresh {
                Duration::ZERO
            } else {
                self.elapsed
            };
            r.fresh = false;
            r.fx.process(elapsed, buf, screen);
        }
        self.running
            .retain(|r| !here(r.kind) || r.held() || r.fx.running());
        for (area, colors) in kept {
            for (p, (fg, bg)) in area.positions().zip(colors) {
                buf[p].fg = fg;
                buf[p].bg = bg;
            }
        }
    }
}

/// Where the frame drew pictures.
fn pictures(images: &Images) -> Vec<Rect> {
    // Blocks are colored cells like any other: they fade and dim with the
    // rest.
    if !images.draws_photos() {
        return Vec::new();
    }
    images.placed.iter().map(|p| p.shown).collect()
}

/// Before the popups are drawn: starts what this frame shows anew, then
/// draws the animations under the popups, leaving the pictures be.
pub fn under(app: &mut App, buf: &mut Buffer, shown: Shown, now: Instant) {
    let on = app.effects.live && app.settings.animations;
    let c = &app.colors;
    let effects = &mut app.effects;
    effects.palette.set(Palette {
        bg: c.bg,
        panel: c.panel,
        text: c.text,
    });
    effects.tick(now);
    let seen = effects.seen.take();
    if on {
        effects.start_under(seen.as_ref(), &shown);
    } else {
        effects.running.clear();
    }
    effects.seen = Some(shown);
    effects.draw(
        |kind| kind < Kind::Dim && kind != Kind::Waiting,
        buf,
        || {
            let mut keep = pictures(&app.images);
            keep.extend(app.video_box.get());
            keep
        },
    );
    // The picture's words are inside what that keeps: lit after.
    effects.draw(|kind| kind == Kind::Waiting, buf, Vec::new);
}

/// After the popups are drawn: the screen behind them dims, leaving the
/// pictures, the popups, the command line's completions and `status` (the
/// status bar, where tuitube says what a popup's keys did) be, and one that
/// opened on top opens out. Pepper's box is left be once nothing is open
/// over it (its picture whole over it again); under a popup it dims with
/// the rest, though `App::picture_shown` may keep the picture there: an
/// older Pepper has taken it away, and one that takes holes shows the
/// cells in them.
pub fn over(app: &mut App, buf: &mut Buffer, status: Option<Rect>) {
    let on = app.effects.live && app.settings.animations;
    let picture_back = !app.popup_open();
    let popups = std::mem::take(app.effects.popups.get_mut());
    let menu = app.effects.menu.take();
    let effects = &mut app.effects;
    if on {
        effects.start_over(&popups);
    }
    effects.seen_popups = popups.iter().map(|&(popup, _)| popup).collect();
    effects.draw(
        |kind| kind == Kind::Dim,
        buf,
        || {
            let mut keep = pictures(&app.images);
            keep.extend(popups.iter().map(|&(_, place)| place));
            keep.extend(menu);
            keep.extend(status);
            if picture_back {
                keep.extend(app.video_box.get());
            }
            keep
        },
    );
    effects.draw(|kind| kind == Kind::Popup, buf, Vec::new);
}

/// Gives the cells in `area` left to the terminal (`Reset`, as `Clear` leaves
/// them) the theme's colors before an animation blends them: tachyonfx
/// would take them for black. Every other color tuitube draws is exact
/// (`theme::WHITE`, `theme::BLACK`), so none is the palette's guess.
fn settle(buf: &mut Buffer, area: Rect, palette: Palette) {
    for p in area.intersection(buf.area).positions() {
        let cell = &mut buf[p];
        if cell.fg == Color::Reset {
            cell.fg = palette.text;
        }
        if cell.bg == Color::Reset {
            cell.bg = palette.bg;
        }
    }
}

/// The whole screen comes out of the background, from its top left corner
/// (a fade from a color runs backwards, so its pattern starts at the other).
fn intro(bg: Color) -> Effect {
    fx::fade_from(bg, bg, (INTRO_MS, Interpolation::QuadOut))
        .with_pattern(DiagonalPattern::bottom_right_to_top_left().with_transition_width(12.0))
        .with_color_space(ColorSpace::Rgb)
}

/// A popup comes out of its own background from its middle out, in an
/// oval as wide as its box, its edge soft (`POPUP_EDGE` columns).
fn popup(palette: Rc<Cell<Palette>>) -> Effect {
    fx::effect_fn(
        (),
        (POPUP_MS, Interpolation::QuadOut),
        move |_, context, cells| {
            let panel = palette.get().panel;
            let area = context.area;
            let middle_x = f32::from(area.x) + f32::from(area.width) / 2.0;
            let middle_y = f32::from(area.y) + f32::from(area.height) / 2.0;
            // A row is about two columns tall.
            let far = (f32::from(area.width) / 2.0).hypot(f32::from(area.height));
            let edge = context.alpha() * (far + POPUP_EDGE);
            for (p, cell) in cells {
                let x = f32::from(p.x) + 0.5 - middle_x;
                let y = 2.0 * (f32::from(p.y) + 0.5 - middle_y);
                let hidden = 1.0 - ((edge - x.hypot(y)) / POPUP_EDGE).clamp(0.0, 1.0);
                if hidden > 0.0 {
                    cell.fg = ColorSpace::Rgb.lerp(&cell.fg, &panel, hidden);
                    cell.bg = ColorSpace::Rgb.lerp(&cell.bg, &panel, hidden);
                }
            }
        },
    )
}

/// Every color behind the popups goes from `from` to `to` of the way to the
/// background, so they stand out on any theme.
fn dim(palette: Rc<Cell<Palette>>, from: f32, to: f32) -> Effect {
    fx::effect_fn(
        (),
        (DIM_MS, Interpolation::QuadOut),
        move |_, context, cells| {
            let bg = palette.get().bg;
            let amount = from + (to - from) * context.alpha();
            for (_, cell) in cells {
                cell.fg = ColorSpace::Rgb.lerp(&cell.fg, &bg, amount);
                cell.bg = ColorSpace::Rgb.lerp(&cell.bg, &bg, amount);
            }
        },
    )
}

/// The player bar sweeps in from the left.
fn player(bg: Color) -> Effect {
    fx::sweep_in(
        Motion::LeftToRight,
        30,
        0,
        bg,
        (PLAYER_MS, Interpolation::QuadOut),
    )
    // Straight from one color to the other, as every animation here: the
    // default goes round the color wheel (red through purple to blue).
    .with_color_space(ColorSpace::Rgb)
}

/// A light runs across the row, left to right, over and over: each cell's
/// text as bright as the theme's text while it passes.
fn loading(palette: Rc<Cell<Palette>>) -> Effect {
    shimmer(SHIMMER_MS, SHIMMER_REACH, move || palette.get().text)
}

/// The same across what the video's picture says while it waits, quicker,
/// as bright as white.
fn waiting() -> Effect {
    shimmer(WAITING_MS, WAITING_REACH, || theme::WHITE)
}

/// A light runs across the row, left to right, once every `ms`, over and
/// over, reaching `reach` columns each side: each cell's text as bright as
/// `light` while it passes.
fn shimmer(ms: u32, reach: f32, light: impl Fn() -> Color + 'static) -> Effect {
    let pass = fx::effect_fn(
        (),
        (ms, Interpolation::SineInOut),
        move |_, context, cells| {
            let light = light();
            let area = context.area;
            // From out of sight on the left to out of sight on the right.
            let across = f32::from(area.width) + 2.0 * reach;
            let at = f32::from(area.x) - reach + context.alpha() * across;
            for (p, cell) in cells {
                let near = 1.0 - (f32::from(p.x) - at).abs() / reach;
                if near > 0.0 {
                    let amount = SHIMMER_LIGHT * near * near;
                    cell.fg = ColorSpace::Rgb.lerp(&cell.fg, &light, amount);
                }
            }
        },
    );
    fx::repeating(pass)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::app;
    use crate::app::{HelpMenu, HelpTab};
    use crate::images::Subject;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui_image::picker::ProtocolType;
    use unicode_width::UnicodeWidthStr;

    /// A screen of text in the theme's colors, with a kitty picture's
    /// placeholders in `picture`, their color its id.
    fn drawn(app: &App, picture: Rect) -> Buffer {
        let c = &app.colors;
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        for p in buf.area.positions() {
            buf[p].set_symbol("x").set_fg(c.subtext).set_bg(c.panel);
        }
        for p in picture.positions() {
            buf[p].set_symbol("\u{10EEEE}").set_fg(Color::Rgb(0, 0, 7));
        }
        buf
    }

    /// One frame at `at`, as `ui::draw` draws it: the animations under the
    /// popups, then `popups` (the one on top last) drawn as they are, then
    /// the animations over them.
    fn frame(
        app: &mut App,
        shown: Shown,
        popups: &[(Popup, Rect)],
        at: Instant,
        picture: Rect,
    ) -> Buffer {
        framed(app, shown, popups, at, picture, None)
    }

    /// `frame`, with the status bar saying something in `message`.
    fn framed(
        app: &mut App,
        shown: Shown,
        popups: &[(Popup, Rect)],
        at: Instant,
        picture: Rect,
        message: Option<Rect>,
    ) -> Buffer {
        let mut buf = drawn(app, picture);
        let as_drawn = buf.clone();
        app.images.placed.clear();
        let id = VideoId::parse("dQw4w9WgXcQ").unwrap();
        app.images.place(&Subject::Thumbnail(id), picture, picture);
        // As tuitube draws them: only while they're open.
        app.help = (!popups.is_empty()).then(|| HelpMenu::new(HelpTab::Keys));
        under(app, &mut buf, shown, at);
        for &(popup, place) in popups {
            for p in place.positions() {
                buf[p] = as_drawn[p].clone();
            }
            app.effects.popups.borrow_mut().push((popup, place));
        }
        over(app, &mut buf, message);
        buf
    }

    /// Drawn to a terminal with real images (kitty's: placeholders).
    fn live() -> App {
        let mut app = app();
        app.effects.live = true;
        app.images.set_protocol(ProtocolType::Kitty);
        app
    }

    /// Live, and its first frame's animation over by `start + LATER`.
    fn settled(start: Instant) -> App {
        let mut app = live();
        frame(&mut app, Shown::default(), NONE, start, PICTURE);
        frame(&mut app, Shown::default(), NONE, start + LATER, PICTURE);
        assert!(!app.effects.moving());
        app
    }

    fn dimmed(app: &App) -> Color {
        ColorSpace::Rgb.lerp(&app.colors.subtext, &app.colors.bg, DIM)
    }

    const PICTURE: Rect = Rect::new(2, 6, 6, 3);
    const NONE: &[(Popup, Rect)] = &[];
    const KEYS: (Popup, Rect) = (Popup::Keys, Rect::new(10, 2, 20, 6));
    const QUESTION: (Popup, Rect) = (Popup::Question, Rect::new(14, 3, 12, 4));
    const LATER: Duration = Duration::from_secs(2);
    const SOON: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn the_screen_comes_out_of_the_background_but_pictures_stay() {
        let mut app = live();
        let start = Instant::now();
        let first = frame(&mut app, Shown::default(), NONE, start, PICTURE);
        assert_eq!(first[(39, 0)].fg, app.colors.bg, "hidden at first");
        assert!(app.effects.moving());
        let picture = drawn(&app, PICTURE);
        for p in PICTURE.positions() {
            assert_eq!(first[p], picture[p], "a picture's cells as drawn");
        }
        let end = frame(&mut app, Shown::default(), NONE, start + EVERY, PICTURE);
        assert_eq!(end[(39, 9)].fg, app.colors.bg, "the far corner comes last");
        let end = frame(&mut app, Shown::default(), NONE, start + LATER, PICTURE);
        assert_eq!(end, drawn(&app, PICTURE), "all as drawn once it's over");
        assert!(!app.effects.moving(), "no more frames");
    }

    #[tokio::test]
    async fn a_popup_dims_the_screen_behind_it_until_it_closes() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        let opening = frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        assert!(app.effects.moving());
        assert_eq!(opening[(20, 5)].fg, app.colors.panel, "it opens out");
        let shown = frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        let c = &app.colors;
        assert_eq!(shown[(0, 0)].fg, dimmed(&app), "dimmed behind it");
        assert_eq!(
            shown[(39, 9)].bg,
            ColorSpace::Rgb.lerp(&c.panel, &c.bg, DIM)
        );
        assert_eq!(shown[(12, 3)].fg, c.subtext, "the popup as drawn");
        assert_eq!(shown[(2, 6)], drawn(&app, PICTURE)[(2, 6)], "and pictures");
        assert!(!app.effects.moving(), "held as it ended, with no frames");

        let closed = frame(&mut app, Shown::default(), NONE, t + LATER, PICTURE);
        assert_eq!(
            closed[(0, 0)].fg,
            dimmed(&app),
            "comes back from where it was"
        );
        assert!(app.effects.moving());
        let closed = frame(&mut app, Shown::default(), NONE, t + 2 * LATER, PICTURE);
        assert_eq!(closed, drawn(&app, PICTURE));
        assert!(!app.effects.moving());
    }

    #[tokio::test]
    async fn a_popup_over_another_opens_out_and_the_one_it_uncovers_stays() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        let asked = frame(
            &mut app,
            Shown::default(),
            &[KEYS, QUESTION],
            t + LATER,
            PICTURE,
        );
        assert!(app.effects.moving(), "the question opens out");
        assert_eq!(asked[(20, 5)].fg, app.colors.panel);
        assert_eq!(
            asked[(11, 3)].fg,
            app.colors.subtext,
            "over the popup under it"
        );
        frame(
            &mut app,
            Shown::default(),
            &[KEYS, QUESTION],
            t + 2 * LATER,
            PICTURE,
        );
        assert!(!app.effects.moving());

        let answered = frame(&mut app, Shown::default(), &[KEYS], t + 2 * LATER, PICTURE);
        assert!(
            !app.effects.moving(),
            "the popup uncovered doesn't open again"
        );
        assert_eq!(answered[(20, 5)].fg, app.colors.subtext);
        assert_eq!(answered[(0, 0)].fg, dimmed(&app), "still dimmed behind");
    }

    #[tokio::test]
    async fn popups_closed_while_it_dims_brighten_it_from_where_it_got() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        let part = frame(&mut app, Shown::default(), &[KEYS], t + SOON, PICTURE)[(0, 0)].fg;
        assert_ne!(part, app.colors.subtext);
        assert_ne!(part, dimmed(&app), "not all the way yet");
        let closed = frame(&mut app, Shown::default(), NONE, t + SOON, PICTURE);
        assert_eq!(closed[(0, 0)].fg, part, "no darker than it got");
        let again = frame(&mut app, Shown::default(), &[KEYS], t + SOON, PICTURE);
        assert_eq!(
            again[(0, 0)].fg,
            part,
            "opened again, it dims on from there"
        );
    }

    #[tokio::test]
    async fn a_popup_opens_out_where_it_is_now() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        // The window changed size: the popup is somewhere else.
        let moved = (Popup::Keys, Rect::new(20, 4, 16, 5));
        let shown = frame(&mut app, Shown::default(), &[moved], t + EVERY, PICTURE);
        assert_ne!(
            shown[(11, 3)].fg,
            app.colors.panel,
            "nothing left where it was"
        );
    }

    #[tokio::test]
    async fn peppers_box_dims_under_a_popup_as_its_picture_goes() {
        let start = Instant::now();
        let mut app = settled(start);
        app.video_box.set(Some(Rect::new(30, 7, 10, 3)));
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        let shown = frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        assert_eq!(shown[(35, 8)].fg, dimmed(&app));
    }

    /// Where Pepper takes holes the picture plays on under a popup, but what
    /// shows in its holes (the controls over its full screen) dims with
    /// the rest.
    #[tokio::test]
    async fn peppers_box_dims_under_a_popup_where_it_takes_holes_too() {
        let start = Instant::now();
        let mut app = settled(start);
        app.pepper_holes = true;
        app.video_box.set(Some(Rect::new(30, 7, 10, 3)));
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        let shown = frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        assert_eq!(shown[(35, 8)].fg, dimmed(&app));
    }

    #[tokio::test]
    async fn a_light_runs_across_loading_until_the_video_plays() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        let bar = Rect::new(0, 7, 40, 3);
        let row = Rect::new(1, 8, 38, 1);
        let loading = Shown {
            player: Some(bar),
            loading: Some(row),
            ..Shown::default()
        };
        frame(&mut app, loading.clone(), NONE, t, Rect::default());
        assert_eq!(app.effects.every(), EVERY, "while the bar sweeps in");
        // Halfway through a pass, the light is in the middle of the row.
        let at = t + Duration::from_millis(u64::from(SHIMMER_MS / 2));
        let shown = frame(&mut app, loading.clone(), NONE, at, Rect::default());
        let (subtext, bg) = (app.colors.subtext, app.colors.bg);
        assert_ne!(shown[(20, 8)].fg, subtext, "lit in the middle");
        assert_eq!(shown[(2, 8)].fg, subtext, "and only there");
        assert_eq!(
            app.effects.every(),
            SHIMMER_EVERY,
            "fewer frames for the light"
        );
        // It goes on for as long as it loads.
        frame(&mut app, loading, NONE, at + 10 * LATER, Rect::default());
        assert!(app.effects.moving());

        let id = VideoId::parse("dQw4w9WgXcQ").unwrap();
        let playing = Shown {
            player: Some(bar),
            video: Some(id.clone()),
            ..Shown::default()
        };
        let swept = frame(
            &mut app,
            playing.clone(),
            NONE,
            at + 10 * LATER,
            Rect::default(),
        );
        assert_eq!(swept[(39, 8)].fg, bg, "the bar sweeps in again as it plays");
        frame(
            &mut app,
            playing.clone(),
            NONE,
            at + 11 * LATER,
            Rect::default(),
        );
        assert!(!app.effects.moving(), "no light once it plays");
        frame(&mut app, playing, NONE, at + 12 * LATER, Rect::default());
        assert!(!app.effects.moving(), "the same video sweeps in once");
    }

    #[tokio::test]
    async fn the_sweep_follows_the_player_bar_as_it_grows() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        let id = VideoId::parse("dQw4w9WgXcQ").unwrap();
        let playing = |bar: Rect| Shown {
            player: Some(bar),
            video: Some(id.clone()),
            ..Shown::default()
        };
        frame(&mut app, playing(Rect::new(0, 7, 40, 3)), NONE, t, PICTURE);
        // The visualizer's rows come above it.
        let grown = frame(&mut app, playing(Rect::new(0, 4, 40, 6)), NONE, t, PICTURE);
        assert_eq!(
            grown[(39, 4)].fg,
            app.colors.bg,
            "its new top sweeps in too"
        );
    }

    #[tokio::test]
    async fn nothing_moves_in_tests_or_with_animations_off() {
        let mut app = app();
        let start = Instant::now();
        let first = frame(&mut app, Shown::default(), &[KEYS], start, PICTURE);
        assert_eq!(first, drawn(&app, PICTURE), "tests draw what's there");
        assert!(!app.effects.moving());

        let mut app = live();
        app.settings.animations = false;
        let first = frame(&mut app, Shown::default(), &[KEYS], start, PICTURE);
        assert_eq!(first, drawn(&app, PICTURE));
        assert!(!app.effects.moving());

        let mut app = live();
        frame(&mut app, Shown::default(), &[KEYS], start, PICTURE);
        assert!(app.effects.moving());
        app.settings.animations = false;
        let now = frame(&mut app, Shown::default(), &[KEYS], start + EVERY, PICTURE);
        assert_eq!(now, drawn(&app, PICTURE), "turned off, it stops at once");
    }

    #[tokio::test]
    async fn loading_lights_up_again_once_animations_are_back_on() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        let loading = Shown {
            player: Some(Rect::new(0, 7, 40, 3)),
            loading: Some(Rect::new(1, 8, 38, 1)),
            ..Shown::default()
        };
        frame(&mut app, loading.clone(), NONE, t, PICTURE);
        app.settings.animations = false;
        frame(&mut app, loading.clone(), NONE, t + EVERY, PICTURE);
        assert!(!app.effects.moving());
        app.settings.animations = true;
        frame(&mut app, loading, NONE, t + 2 * EVERY, PICTURE);
        assert!(app.effects.moving(), "still loading: lit again");
    }

    #[tokio::test]
    async fn the_status_bar_stays_readable_over_the_dimming() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        let said = Some(Rect::new(0, 9, 40, 1));
        framed(&mut app, Shown::default(), &[KEYS], t, PICTURE, said);
        let shown = framed(
            &mut app,
            Shown::default(),
            &[KEYS],
            t + LATER,
            PICTURE,
            said,
        );
        assert_eq!(shown[(30, 9)].fg, app.colors.subtext, "as drawn");
        assert_eq!(shown[(30, 8)].fg, dimmed(&app), "the rest dimmed");
    }

    #[tokio::test]
    async fn the_ui_draws_only_exact_colors_so_none_snaps_as_it_blends() {
        let exact = |color: Color| matches!(color, Color::Rgb(..) | Color::Reset);
        let mut app = app();
        crate::app::tests::with_feed(&mut app, 3);
        // A length badge on its card.
        app.videos[1].duration = Some(754);
        crate::app::tests::listening(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mut check = |app: &mut App| {
            terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
            for cell in terminal.backend().buffer().content() {
                assert!(exact(cell.fg) && exact(cell.bg), "{cell:?}");
            }
        };
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        check(&mut app);
        app.on_key(key('i'));
        check(&mut app);
        app.info_popup = None;
        app.help = Some(HelpMenu::new(HelpTab::Settings));
        app.confirm = Some(crate::app::Confirm::ClearJukebox);
        check(&mut app);
        (app.help, app.confirm) = (None, None);
        app.chapter_menu = Some(Default::default());
        check(&mut app);
        app.chapter_menu = None;
        app.on_key(key('F'));
        assert!(app.group_menu.is_some());
        check(&mut app);
        app.group_menu = None;
        // The command line, with its completions.
        app.on_key(key(':'));
        app.on_key(key('s'));
        assert!(app.command.is_some());
        check(&mut app);
        app.command = None;
        app.settings.list = true;
        check(&mut app);
        app.settings.list = false;
        app.on_key(key('p'));
        assert!(app.watch.is_some(), "the watch page");
        check(&mut app);
    }

    #[test]
    fn tachyonfxs_own_effects_blend_straight_as_the_others_do() {
        let bg = Color::Rgb(30, 30, 46);
        assert_eq!(intro(bg).color_space(), ColorSpace::Rgb);
        assert_eq!(player(bg).color_space(), ColorSpace::Rgb);
    }

    #[test]
    fn cells_left_to_the_terminal_blend_from_the_themes_colors() {
        let palette = Palette {
            bg: Color::Rgb(30, 30, 46),
            panel: Color::Rgb(49, 50, 68),
            text: Color::Rgb(205, 214, 244),
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        settle(&mut buf, Rect::new(0, 0, 1, 1), palette);
        let colors = |x: u16| (buf[(x, 0)].fg, buf[(x, 0)].bg);
        assert_eq!(colors(0), (palette.text, palette.bg));
        assert_eq!(colors(1), (Color::Reset, Color::Reset), "outside, as drawn");
    }

    #[tokio::test]
    async fn peppers_box_dims_while_the_command_line_keeps_its_picture_away() {
        let start = Instant::now();
        let mut app = settled(start);
        app.video_box.set(Some(Rect::new(30, 7, 10, 3)));
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        app.command = Some(Default::default());
        let closed = frame(&mut app, Shown::default(), NONE, t + LATER, PICTURE);
        assert_eq!(closed[(35, 8)].fg, dimmed(&app), "with the rest");
    }

    #[tokio::test]
    async fn a_theme_picked_behind_a_popup_dims_toward_its_own_background() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        app.colors = crate::theme::Colors::named("latte");
        let shown = frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        assert_eq!(shown[(0, 0)].fg, dimmed(&app));
    }

    #[tokio::test]
    async fn a_question_answered_while_it_opens_leaves_the_popup_under_it_be() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        frame(
            &mut app,
            Shown::default(),
            &[KEYS, QUESTION],
            t + LATER,
            PICTURE,
        );
        let answered = frame(
            &mut app,
            Shown::default(),
            &[KEYS],
            t + LATER + SOON,
            PICTURE,
        );
        assert!(!app.effects.moving(), "nothing opens out");
        assert_eq!(
            answered[(11, 3)].fg,
            app.colors.subtext,
            "the popup as drawn"
        );
    }

    #[tokio::test]
    async fn over_ssh_the_first_screen_comes_at_once() {
        let mut app = live();
        app.effects.remote = true;
        let first = frame(&mut app, Shown::default(), NONE, Instant::now(), PICTURE);
        assert_eq!(first, drawn(&app, PICTURE), "no fading in");
        assert!(!app.effects.moving());
    }

    #[tokio::test]
    async fn fewer_frames_over_ssh() {
        let start = Instant::now();
        let mut app = live();
        frame(&mut app, Shown::default(), NONE, start, PICTURE);
        assert_eq!(app.effects.every(), EVERY);
        app.effects.remote = true;
        assert_eq!(app.effects.every(), REMOTE_EVERY);
        let loading = Shown {
            player: Some(Rect::new(0, 7, 40, 3)),
            loading: Some(Rect::new(1, 8, 38, 1)),
            ..Shown::default()
        };
        frame(&mut app, loading.clone(), NONE, start + LATER, PICTURE);
        frame(&mut app, loading, NONE, start + 2 * LATER, PICTURE);
        assert_eq!(app.effects.every(), SHIMMER_EVERY, "the light, as few");
    }

    #[tokio::test]
    async fn peppers_box_is_left_be_once_the_popups_close() {
        let start = Instant::now();
        let mut app = settled(start);
        app.video_box.set(Some(Rect::new(30, 7, 10, 3)));
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        let closed = frame(&mut app, Shown::default(), NONE, t + LATER, PICTURE);
        assert_eq!(closed[(0, 0)].fg, dimmed(&app), "the rest brightens");
        assert_eq!(
            closed[(35, 8)].fg,
            app.colors.subtext,
            "Pepper's picture is back"
        );
    }

    #[tokio::test]
    async fn no_light_and_no_frames_for_a_loading_row_with_no_room() {
        let start = Instant::now();
        let mut app = settled(start);
        let squeezed = Shown {
            loading: Some(Rect::new(1, 8, 0, 1)),
            ..Shown::default()
        };
        frame(&mut app, squeezed, NONE, start + LATER, PICTURE);
        assert!(!app.effects.moving());
    }

    #[tokio::test]
    async fn nothing_opens_or_sweeps_in_a_box_with_no_room() {
        let start = Instant::now();
        let mut app = settled(start);
        let id = VideoId::parse("dQw4w9WgXcQ").unwrap();
        let squeezed = Shown {
            player: Some(Rect::new(0, 9, 40, 0)),
            video: Some(id),
            ..Shown::default()
        };
        frame(&mut app, squeezed, NONE, start + LATER, PICTURE);
        assert!(!app.effects.moving(), "no sweep");
        let none = (Popup::Keys, Rect::new(20, 5, 0, 0));
        frame(&mut app, Shown::default(), &[none], start + LATER, PICTURE);
        let opening = app.effects.running.iter().any(|r| r.kind == Kind::Popup);
        assert!(!opening, "no popup opening out");
    }

    #[tokio::test]
    async fn the_command_lines_completions_are_never_dimmed() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        // `:` right as the popup closes, while the screen brightens.
        app.effects.menu.set(Some(Rect::new(0, 8, 12, 1)));
        let closed = frame(&mut app, Shown::default(), NONE, t + LATER, PICTURE);
        assert_eq!(closed[(0, 0)].fg, dimmed(&app));
        assert_eq!(closed[(10, 8)].fg, app.colors.subtext, "as drawn");
    }

    #[tokio::test]
    async fn blocks_dim_with_the_rest_as_they_are_only_cells() {
        let start = Instant::now();
        let mut app = settled(start);
        app.images.set_protocol(ProtocolType::Halfblocks);
        let t = start + LATER;
        frame(&mut app, Shown::default(), &[KEYS], t, PICTURE);
        let shown = frame(&mut app, Shown::default(), &[KEYS], t + LATER, PICTURE);
        let picture = ColorSpace::Rgb.lerp(&Color::Rgb(0, 0, 7), &app.colors.bg, DIM);
        assert_eq!(shown[(2, 6)].fg, picture);
    }

    #[tokio::test]
    async fn nothing_moves_under_peppers_full_screen() {
        let start = Instant::now();
        let mut app = settled(start);
        let covered = Shown {
            player: Some(Rect::new(0, 7, 40, 3)),
            loading: Some(Rect::new(1, 8, 38, 1)),
            covered: true,
            ..Shown::default()
        };
        frame(&mut app, covered, NONE, start + LATER, PICTURE);
        assert!(!app.effects.moving(), "no frames for what can't be seen");
        let shown = Shown {
            player: Some(Rect::new(0, 7, 40, 3)),
            loading: Some(Rect::new(1, 8, 38, 1)),
            ..Shown::default()
        };
        frame(&mut app, shown, NONE, start + LATER, PICTURE);
        assert!(
            app.effects.moving(),
            "back from full screen, still loading: lit"
        );
    }

    #[tokio::test]
    async fn every_popup_is_noted_as_it_is_drawn() {
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let mut app = app();
        crate::app::tests::with_feed(&mut app, 3);
        crate::app::tests::listening(&mut app);
        // Each opens by its key; a key goes to the popup on top, so the
        // first is put aside while the second opens.
        app.on_key(key('i'));
        let info = app.info_popup.take();
        assert!(info.is_some());
        app.on_key(key('F'));
        assert!(app.group_menu.is_some());
        app.info_popup = info;
        app.chapter_menu = Some(Default::default());
        app.help = Some(HelpMenu::new(HelpTab::Keys));
        app.confirm = Some(crate::app::Confirm::ClearJukebox);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let noted = [
            Popup::Keys,
            Popup::Info,
            Popup::Chapters,
            Popup::Groups,
            Popup::Question,
        ];
        assert_eq!(app.effects.seen_popups, noted, "the one on top last");
    }

    #[tokio::test]
    async fn the_light_waits_under_a_popup_and_goes_on_once_it_closes() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        let loading = Shown {
            loading: Some(Rect::new(1, 8, 38, 1)),
            ..Shown::default()
        };
        frame(&mut app, loading.clone(), NONE, t, PICTURE);
        assert!(app.effects.moving());
        frame(&mut app, loading.clone(), &[KEYS], t + EVERY, PICTURE);
        frame(&mut app, loading.clone(), &[KEYS], t + LATER, PICTURE);
        assert!(!app.effects.moving(), "no frames for it behind the popup");
        frame(&mut app, loading.clone(), NONE, t + LATER, PICTURE);
        frame(&mut app, loading, NONE, t + 2 * LATER, PICTURE);
        assert!(app.effects.moving(), "lit again");
    }

    #[tokio::test]
    async fn the_light_runs_across_the_words_saying_loading_not_the_whole_bar() {
        let mut app = app();
        crate::app::tests::with_feed(&mut app, 3);
        let first = app.videos[0].clone();
        crate::app::tests::start_loading(&mut app, first);
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let row = app.effects.seen.as_ref().and_then(|s| s.loading);
        let title = app.title(&app.videos[0]).to_string();
        let words = format!(" {} Loading “{title}”…", app.icons.loading);
        assert_eq!(row.map(|r| usize::from(r.width)), Some(words.width()));
    }

    /// What the video's picture says while it waits is inside the box the
    /// animations leave be (Pepper's, or the thumbnail's): the light runs
    /// across it all the same, and nothing else in the box changes.
    #[tokio::test]
    async fn a_quicker_light_runs_across_what_the_picture_says_while_it_waits() {
        let start = Instant::now();
        let mut app = settled(start);
        let t = start + LATER;
        let video_box = Rect::new(30, 6, 10, 4);
        app.video_box.set(Some(video_box));
        let words = Rect::new(31, 8, 8, 1);
        let waiting = Shown {
            waiting: Some(words),
            ..Shown::default()
        };
        frame(&mut app, waiting.clone(), NONE, t, PICTURE);
        assert!(app.effects.moving());
        assert_eq!(app.effects.every(), SHIMMER_EVERY);
        // Halfway through a pass: in the middle of the words.
        let half = Duration::from_millis(u64::from(WAITING_MS / 2));
        let lit = frame(&mut app, waiting.clone(), NONE, t + half, PICTURE);
        let as_drawn = drawn(&app, PICTURE);
        assert_ne!(lit[(35, 8)].fg, as_drawn[(35, 8)].fg, "lit");
        assert_eq!(
            lit[(30, 6)],
            as_drawn[(30, 6)],
            "the rest of the box as drawn"
        );
        // Over Pepper's full screen too, which it's in.
        let covered = Shown {
            covered: true,
            ..waiting
        };
        frame(&mut app, covered, NONE, t + LATER, PICTURE);
        assert!(app.effects.moving());
        // Gone: dark again.
        frame(&mut app, Shown::default(), NONE, t + 2 * LATER, PICTURE);
        assert!(!app.effects.moving());
    }

    #[tokio::test]
    async fn the_light_runs_across_the_words_the_picture_says() {
        let mut app = app();
        crate::app::tests::with_feed(&mut app, 3);
        let first = app.videos[0].clone();
        crate::app::tests::start_loading(&mut app, first);
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let place = app.effects.seen.as_ref().and_then(|s| s.waiting);
        let words = format!(" {} Loading… ", app.icons.loading);
        assert_eq!(place.map(|r| usize::from(r.width)), Some(words.width()));
    }
}
