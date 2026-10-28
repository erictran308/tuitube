//! The splash screen, with animations on: the logo in the middle from the
//! moment tuitube has the terminal, while the rest loads behind it (asking
//! the terminal how it draws images, mpv's bundle, the first screen's
//! pictures). `main` draws it first; then `ui::draw` draws each frame under
//! it, so the pictures that frame shows are asked for, though sixel and
//! iTerm2's aren't drawn (`Images::hold`: they'd show over the splash;
//! kitty's are, and sent apart from the frame). It leaves once it's been
//! up `HOLD` and the pictures asked for are in, or it's been up `MOST`,
//! fading out over `OUT`; the animations' fade at start then brings in the
//! screen.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use tachyonfx::ColorSpace;
use unicode_width::UnicodeWidthStr;

use crate::theme::{self, Colors};

/// Up at least this long, so it's seen, however fast the rest is.
const HOLD: Duration = Duration::from_millis(700);
/// Up at most this long, whatever hasn't loaded.
const MOST: Duration = Duration::from_millis(1500);
/// How long it takes to fade out.
const OUT: Duration = Duration::from_millis(300);
/// How often it's drawn while up (the light across its words), and while
/// it fades out.
const UP_EVERY: Duration = Duration::from_millis(50);
const OUT_EVERY: Duration = Duration::from_millis(16);

const WORDS: &str = "t u i t u b e";
const LOADING: &str = "Loading your videos…";

/// The logo's sizes, in pixels two to a cell (half blocks), largest first:
/// a red box with round corners, as wide as YouTube's, a white triangle in
/// it. The largest that leaves room around it is drawn.
const LOGOS: [Logo; 2] = [
    Logo {
        width: 34,
        height: 24,
        corner: 6.0,
    },
    Logo {
        width: 22,
        height: 16,
        corner: 4.0,
    },
];

#[derive(Clone, Copy)]
struct Logo {
    width: u16,
    height: u16,
    corner: f32,
}
/// The light across "Loading": once across, how far it reaches each side,
/// as the player bar's (`ui::effects`).
const LIGHT_EVERY: f32 = 1.4;
const LIGHT_REACH: f32 = 6.0;

pub struct Splash {
    since: Instant,
    /// When it started fading out.
    leaving: Option<Instant>,
}

impl Splash {
    pub fn new(since: Instant) -> Self {
        Self {
            since,
            leaving: None,
        }
    }

    /// Moves on to `now`, given how many pictures the frame before asked
    /// for and didn't have (`None` before any frame): false once it's gone.
    pub fn advance(&mut self, now: Instant, missing: Option<usize>) -> bool {
        let up = now.saturating_duration_since(self.since);
        if self.leaving.is_none() && up >= HOLD && (missing == Some(0) || up >= MOST) {
            self.leaving = Some(now);
        }
        self.leaving
            .is_none_or(|leaving| now.saturating_duration_since(leaving) < OUT)
    }

    /// How long until the next frame.
    pub fn every(&self) -> Duration {
        if self.leaving.is_some() {
            OUT_EVERY
        } else {
            UP_EVERY
        }
    }

    /// How far it has faded out, 0 to 1.
    fn gone(&self, now: Instant) -> f32 {
        self.leaving
            .map_or(0.0, |leaving| {
                now.saturating_duration_since(leaving).as_secs_f32() / OUT.as_secs_f32()
            })
            .min(1.0)
    }
}

/// Draws the splash screen at once, in `theme`'s colors, as tuitube takes
/// the terminal (`main`, `--demo`); returns when, for `App::keep_splash`.
pub fn show(terminal: &mut ratatui::DefaultTerminal, theme: &str) -> Instant {
    let since = Instant::now();
    let colors = Colors::named(theme);
    let splash = Splash::new(since);
    // A terminal that can't be drawn to fails the first frame, as ever.
    let _ = terminal.draw(|f| {
        let area = f.area();
        draw(f.buffer_mut(), area, &colors, &splash, since);
    });
    since
}

/// Covers `area` with the splash as it is at `now`.
pub fn draw(buf: &mut Buffer, area: Rect, c: &Colors, splash: &Splash, now: Instant) {
    let gone = splash.gone(now);
    // Colors go to the background as it fades out.
    let faded = |color: Color| ColorSpace::Rgb.lerp(&color, &c.bg, gone);
    for p in area.positions() {
        buf[p].reset();
        buf[p].set_bg(c.bg).set_fg(faded(c.text));
    }
    // The logo, then a blank row, the words, a blank row, "Loading".
    let logo = LOGOS
        .into_iter()
        .find(|logo| area.width >= logo.width + 4 && area.height >= logo.height / 2 + 8);
    let logo_rows = logo.map_or(0, |logo| logo.height / 2);
    let height = logo_rows + if logo.is_some() { 4 } else { 3 };
    if area.height < height {
        return;
    }
    let top = area.y + (area.height - height) / 2;
    if let Some(logo) = logo {
        let left = area.x + (area.width - logo.width) / 2;
        for row in 0..logo_rows {
            for col in 0..logo.width {
                let upper = logo.pixel(col, 2 * row, c).map(faded);
                let lower = logo.pixel(col, 2 * row + 1, c).map(faded);
                let cell = &mut buf[(left + col, top + row)];
                match (upper, lower) {
                    (None, None) => {}
                    (upper, lower) if upper == lower => {
                        cell.set_bg(upper.unwrap_or(c.bg));
                    }
                    (upper, lower) => {
                        cell.set_symbol("▀")
                            .set_fg(upper.unwrap_or(c.bg))
                            .set_bg(lower.unwrap_or(c.bg));
                    }
                }
            }
        }
    }
    let words_row = top + height - 3;
    let style = Style::new().fg(faded(c.text)).add_modifier(Modifier::BOLD);
    centered(buf, area, words_row, WORDS, style);
    let at = centered(
        buf,
        area,
        words_row + 2,
        LOADING,
        Style::new().fg(faded(c.subtext)),
    );
    // A light across "Loading", as the player bar's while a video loads.
    let width = LOADING.width() as f32;
    let pass = (now.saturating_duration_since(splash.since).as_secs_f32() / LIGHT_EVERY).fract();
    let center = f32::from(at.x) - LIGHT_REACH + pass * (width + 2.0 * LIGHT_REACH);
    for p in at.positions() {
        let near = 1.0 - (f32::from(p.x) - center).abs() / LIGHT_REACH;
        if near > 0.0 {
            let cell = &mut buf[p];
            cell.fg = ColorSpace::Rgb.lerp(&cell.fg, &faded(c.text), 0.85 * near * near);
        }
    }
}

/// Writes `text` in the middle of `row`, where it fits; returns where.
fn centered(buf: &mut Buffer, area: Rect, row: u16, text: &str, style: Style) -> Rect {
    let width = (text.width() as u16).min(area.width);
    let x = area.x + (area.width - width) / 2;
    buf.set_stringn(x, row, text, usize::from(width), style);
    Rect::new(x, row, width, 1)
}

impl Logo {
    /// Its pixel at `x`, `y`: the red box's, the triangle's white, or none,
    /// outside the box.
    fn pixel(self, x: u16, y: u16, c: &Colors) -> Option<Color> {
        let (x, y) = (f32::from(x) + 0.5, f32::from(y) + 0.5);
        let (width, height, corner) = (f32::from(self.width), f32::from(self.height), self.corner);
        // How far into a round corner, each way.
        let dx = (corner - x).max(x - (width - corner)).max(0.0);
        let dy = (corner - y).max(y - (height - corner)).max(0.0);
        if dx.hypot(dy) > corner {
            return None;
        }
        // Pointing right, a touch right of the middle, as it looks
        // centered: from 39% of the way across to 70%, half as tall.
        let (left, tip) = (0.39 * width, 0.70 * width);
        let (middle, half) = (height / 2.0, height / 4.0);
        let across = 1.0 - (y - middle).abs() / half;
        let white = across > 0.0 && x >= left && x <= left + (tip - left) * across;
        Some(if white { theme::WHITE } else { c.red })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> Colors {
        Colors::named(theme::DEFAULT)
    }

    fn text(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn the_logo_and_words_sit_in_the_middle() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let start = Instant::now();
        draw(&mut buf, area, &colors(), &Splash::new(start), start);
        let shown = text(&buf);
        assert!(shown.contains(WORDS) && shown.contains(LOADING), "{shown}");
        let c = colors();
        // The box's red in the middle of the logo's rows, the triangle's
        // white in it, the background around.
        let red = (0..24).any(|y| (0..80).any(|x| buf[(x, y)].bg == c.red));
        let white = (0..24).any(|y| (0..80).any(|x| buf[(x, y)].bg == theme::WHITE));
        assert!(red && white);
        assert_eq!(buf[(0, 0)].bg, c.bg);
        // As much room above the logo as under "Loading".
        let first = (0..24).find(|&y| (0..80).any(|x| buf[(x, y)].bg == c.red));
        let last = shown.lines().position(|l| l.contains(LOADING));
        let (above, under) = (first.unwrap(), 23 - last.unwrap() as u16);
        assert!(above.abs_diff(under) <= 1, "{above} above, {under} under");
    }

    #[test]
    fn a_small_window_gets_the_words_alone() {
        let area = Rect::new(0, 0, 20, 5);
        let mut buf = Buffer::empty(area);
        let start = Instant::now();
        draw(&mut buf, area, &colors(), &Splash::new(start), start);
        let shown = text(&buf);
        assert!(shown.contains(WORDS));
        assert!(!buf.content().iter().any(|cell| cell.bg == colors().red));
    }

    #[test]
    fn it_leaves_once_up_long_enough_and_the_pictures_are_in() {
        let start = Instant::now();
        let mut splash = Splash::new(start);
        assert!(splash.advance(start, Some(0)), "up at first");
        let held = start + HOLD / 2;
        assert!(splash.advance(held, Some(0)) && splash.leaving.is_none());
        let up = start + HOLD;
        assert!(splash.advance(up, None), "no frame yet: nothing known");
        assert!(splash.advance(up, Some(3)) && splash.leaving.is_none());
        assert!(splash.advance(up, Some(0)), "fading out");
        assert_eq!(splash.leaving, Some(up));
        assert_eq!(splash.every(), OUT_EVERY);
        assert!(!splash.advance(up + OUT, Some(0)), "gone");

        let mut slow = Splash::new(start);
        assert!(slow.advance(start + MOST, Some(5)));
        assert!(
            slow.leaving.is_some(),
            "the pictures still loading wait no more"
        );
    }

    #[tokio::test]
    async fn nothing_moves_under_it_and_the_fade_at_start_waits_for_it() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut app = crate::app::tests::app();
        crate::app::tests::with_feed(&mut app, 3);
        app.effects.live = true;
        app.keep_splash(Instant::now());
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(text(terminal.backend().buffer()).contains(WORDS));
        assert!(!app.effects.moving(), "nothing under it");
        app.splash.as_mut().unwrap().leaving = Some(Instant::now() - OUT);
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(app.splash.is_none(), "gone");
        assert!(!text(terminal.backend().buffer()).contains(WORDS));
        assert!(app.effects.moving(), "the fade at start, now");
    }

    #[test]
    fn fading_out_it_goes_to_the_background() {
        let area = Rect::new(0, 0, 80, 24);
        let start = Instant::now();
        let mut splash = Splash::new(start);
        splash.leaving = Some(start);
        let mut buf = Buffer::empty(area);
        draw(&mut buf, area, &colors(), &splash, start + OUT);
        let c = colors();
        assert!(
            buf.content()
                .iter()
                .all(|cell| cell.bg == c.bg && cell.fg == c.bg)
        );
    }
}
