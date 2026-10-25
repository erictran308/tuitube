//! pepper: a Ghostty build that shows video inside the terminal, straight
//! from the graphics chip (`../pepper`). There, tuitube plays the picture
//! through Pepper's player, pepper-play (inside Pepper.app, its path in
//! `PEPPER_PLAY`: libmpv drawing into IOSurfaces that Pepper shows over a
//! box of cells) instead of in mpv's window. pepper-play takes mpv's
//! options and JSON IPC, so tuitube drives it as it drives mpv.
//!
//! The two meet by a token, fresh for each playback: tuitube gives it to
//! pepper-play over the socket (never on a command line), and places the
//! box with it by an escape sequence, `ESC ] 777 ; pepper ; place ; TOKEN ;
//! COL ; ROW ; COLS ; ROWS ESC \`, which only a program writing to this tab
//! can send. Pepper shows pepper-play's frames only where that token was
//! placed, and tells it the box's size in pixels. Nothing of this happens in
//! another terminal: `port` is `None`.
//!
//! A popup over the picture: a Pepper that lists `holes` in
//! `PEPPER_FEATURES` (`takes_holes`) takes up to four boxes of cells after
//! the box's, `;COL,ROW,COLS,ROWS` each, where the video leaves the cells
//! to the terminal, so the picture plays on around what's drawn over it
//! (`holes`). An older one refuses a place with holes (and keeps the box
//! where it was), so there the picture is taken away while a popup is
//! open.

use std::ffi::OsString;

use ratatui::layout::Rect;

/// The token's random bytes (32 hex digits).
#[cfg(unix)]
const TOKEN_BYTES: usize = 16;

/// What a terminal multiplexer running tuitube sets: tmux, GNU screen,
/// zellij. Each would need every escape sequence wrapped to pass it on, and
/// screen may log them (`screen -L`).
const MULTIPLEXERS: [&str; 3] = ["TMUX", "STY", "ZELLIJ"];

/// Pepper's port, from `PEPPER_PORT`, when tuitube runs in Pepper: not
/// through a multiplexer.
pub fn port() -> Option<OsString> {
    port_from(|name| std::env::var_os(name))
}

fn port_from(var: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    if MULTIPLEXERS.iter().any(|name| var(name).is_some()) {
        return None;
    }
    var("PEPPER_PORT").filter(|port| !port.is_empty())
}

/// Whether this Pepper takes holes in a box: `PEPPER_FEATURES` lists
/// `holes` (comma separated). Not set: an older Pepper.
pub fn takes_holes() -> bool {
    holes_from(std::env::var_os("PEPPER_FEATURES"))
}

fn holes_from(features: Option<OsString>) -> bool {
    features
        .and_then(|value| value.into_string().ok())
        .is_some_and(|value| value.split(',').any(|feature| feature.trim() == "holes"))
}

/// The most holes Pepper takes in a box.
pub const MAX_HOLES: usize = 4;

/// What of `area` the boxes in `over` (the popups drawn over the picture)
/// cover, as holes for `place`: each cut to the box, those outside it left
/// out, and while there are more than Pepper takes, the two that together
/// take the least more of the picture made one (one around them all could
/// take most of it).
pub fn holes(area: Rect, over: &[Rect]) -> Vec<Rect> {
    let mut holes: Vec<Rect> = over
        .iter()
        .map(|place| place.intersection(area))
        .filter(|hole| !hole.is_empty())
        .collect();
    let cells = |r: Rect| i64::from(r.width) * i64::from(r.height);
    while holes.len() > MAX_HOLES {
        let n = holes.len();
        let (i, j) = (0..n)
            .flat_map(|i| (i + 1..n).map(move |j| (i, j)))
            .min_by_key(|&(i, j)| {
                cells(holes[i].union(holes[j])) - cells(holes[i]) - cells(holes[j])
            })
            .expect("more than one");
        holes[i] = holes[i].union(holes[j]);
        holes.swap_remove(j);
    }
    holes
}

/// A token for one playback: random hex digits, as Pepper takes them
/// (1–64 of `[A-Za-z0-9_-]`). `None` if the system's randomness can't be
/// read: then mpv plays in its window.
pub fn new_token() -> Option<String> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut bytes = [0u8; TOKEN_BYTES];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .ok()?;
        Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }
    #[cfg(not(unix))]
    None
}

/// Puts the picture of `token`'s playback over `area`, but for `holes`
/// (none for a Pepper that doesn't take them: `takes_holes`).
pub fn place(token: &str, area: Rect, holes: &[Rect]) -> String {
    let holes: String = holes
        .iter()
        .map(|h| format!(";{},{},{},{}", h.x, h.y, h.width, h.height))
        .collect();
    format!(
        "\x1b]777;pepper;place;{token};{};{};{};{}{holes}\x1b\\",
        area.x, area.y, area.width, area.height
    )
}

/// Takes the picture of `token`'s playback away.
pub fn remove(token: &str) -> String {
    format!("\x1b]777;pepper;remove;{token}\x1b\\")
}

/// A 16:9 box as wide as `area` allows, at its top left, no taller than
/// `max_rows`, for cells of `cell` pixels.
pub fn video_area(area: Rect, cell: (u16, u16), max_rows: u16) -> Rect {
    let (cw, ch) = (f64::from(cell.0.max(1)), f64::from(cell.1.max(1)));
    let rows_for = |cols: u16| (f64::from(cols) * cw * 9.0 / 16.0 / ch).round() as u16;
    let mut cols = area.width;
    let mut rows = rows_for(cols);
    let max_rows = max_rows.min(area.height);
    if rows > max_rows {
        rows = max_rows;
        cols = ((f64::from(rows) * ch * 16.0 / 9.0 / cw).round() as u16).min(area.width);
    }
    Rect::new(area.x, area.y, cols, rows.max(1).min(area.height))
}

/// `size` (a box `video_area` made) moved to the middle of `area`.
pub fn centered(area: Rect, size: Rect) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(size.width) / 2,
        y: area.y + area.height.saturating_sub(size.height) / 2,
        ..size
    }
}

/// The narrowest picture-in-picture box, in columns: smaller isn't worth
/// covering the cards for.
pub const PIP_MIN_COLS: u16 = 24;

/// The picture-in-picture box while browsing: a quarter of `area`'s width
/// (at least [`PIP_MIN_COLS`]), 16:9, in its bottom right corner, a cell in
/// from its edges. `None` where it wouldn't fit.
pub fn pip_area(area: Rect, cell: (u16, u16)) -> Option<Rect> {
    let inner = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let cols = (inner.width / 4).max(PIP_MIN_COLS);
    if cols > inner.width {
        return None;
    }
    let size = video_area(Rect::new(0, 0, cols, inner.height), cell, inner.height / 2);
    (size.width >= PIP_MIN_COLS && size.height >= 3).then(|| {
        Rect::new(
            inner.right() - size.width,
            inner.bottom() - size.height,
            size.width,
            size.height,
        )
    })
}

/// What Pepper was last told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub token: String,
    pub area: Rect,
    pub holes: Vec<Rect>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_in_pepper_outside_tmux() {
        let env = |vars: &'static [(&str, &str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(
            port_from(env(&[("PEPPER_PORT", "io.github.erictran308.pepper.42")])),
            Some("io.github.erictran308.pepper.42".into())
        );
        assert_eq!(port_from(env(&[])), None);
        assert_eq!(port_from(env(&[("PEPPER_PORT", "")])), None);
        assert_eq!(
            port_from(env(&[
                ("PEPPER_PORT", "x.1"),
                ("TMUX", "/tmp/tmux-501/default,1,0")
            ])),
            None
        );
        assert_eq!(
            port_from(env(&[("PEPPER_PORT", "x.1"), ("STY", "123.ttys001.mac")])),
            None
        );
        assert_eq!(
            port_from(env(&[("PEPPER_PORT", "x.1"), ("ZELLIJ", "0")])),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn tokens_are_fresh_hex() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 2 * TOKEN_BYTES);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn sequences() {
        let area = Rect::new(2, 3, 40, 11);
        assert_eq!(
            place("ab", area, &[]),
            "\x1b]777;pepper;place;ab;2;3;40;11\x1b\\"
        );
        assert_eq!(
            place(
                "ab",
                area,
                &[Rect::new(10, 5, 20, 4), Rect::new(0, 0, 160, 1)]
            ),
            "\x1b]777;pepper;place;ab;2;3;40;11;10,5,20,4;0,0,160,1\x1b\\"
        );
        assert_eq!(remove("ab"), "\x1b]777;pepper;remove;ab\x1b\\");
    }

    #[test]
    fn holes_only_where_pepper_lists_them() {
        let features = |value: &str| holes_from(Some(value.into()));
        assert!(features("holes"));
        assert!(features("frames,holes"));
        assert!(!features(""));
        assert!(!features("holesome"));
        assert!(!holes_from(None), "an older Pepper");
    }

    #[test]
    fn holes_are_what_popups_cover_of_the_box() {
        let area = Rect::new(10, 5, 40, 20);
        // Cut to the box; one beside it is none.
        let popup = Rect::new(30, 10, 40, 6);
        assert_eq!(holes(area, &[popup]), [Rect::new(30, 10, 20, 6)]);
        assert_eq!(holes(area, &[Rect::new(0, 0, 10, 5)]), []);
        assert_eq!(holes(area, &[]), []);
        let many: Vec<Rect> = (0..5).map(|i| Rect::new(12 + 2 * i, 6 + i, 2, 1)).collect();
        assert_eq!(holes(area, &many[..4]), many[..4]);
        // More than Pepper takes: the two that take the least more of the
        // picture together are one, so the strip at the bottom of a full
        // screen and a popup stay as they are, not one box over most of it.
        let tab = Rect::new(0, 0, 160, 45);
        let over = [
            Rect::new(0, 41, 160, 4),
            Rect::new(50, 10, 60, 20),
            Rect::new(0, 0, 2, 1),
            Rect::new(158, 0, 2, 1),
            Rect::new(80, 35, 2, 1),
        ];
        let made = holes(tab, &over);
        assert_eq!(made.len(), MAX_HOLES, "{made:?}");
        assert!(
            made.contains(&over[0]) && made.contains(&over[1]),
            "{made:?}"
        );
        assert!(made.contains(&Rect::new(0, 0, 160, 1)), "{made:?}");
        assert!(
            over.iter()
                .all(|r| made.iter().any(|h| h.intersection(*r) == *r)),
            "every one in a hole: {made:?}"
        );
    }

    #[test]
    fn boxes_are_16_by_9() {
        // Cells 9×18 px: 80 columns are 720 px wide, so 405 px, 22.5 rows.
        let area = video_area(Rect::new(5, 2, 80, 40), (9, 18), 30);
        assert_eq!(area, Rect::new(5, 2, 80, 23));
        // Capped at 10 rows: 180 px tall, 320 px wide, 36 columns.
        assert_eq!(
            video_area(Rect::new(0, 0, 80, 40), (9, 18), 10),
            Rect::new(0, 0, 36, 10)
        );
        // Never outside the area.
        let tiny = video_area(Rect::new(0, 0, 3, 1), (9, 18), 30);
        assert!(tiny.width <= 3 && tiny.height == 1);
    }

    #[test]
    fn the_picture_in_picture_box_sits_in_the_bottom_right_corner() {
        let area = Rect::new(26, 0, 154, 40);
        let pip = pip_area(area, (9, 18)).unwrap();
        assert_eq!(pip.right(), area.right() - 1);
        assert_eq!(pip.bottom(), area.bottom() - 1);
        assert!(pip.width >= PIP_MIN_COLS && pip.width <= area.width / 4 + 1);
        // Too narrow for one.
        assert_eq!(pip_area(Rect::new(0, 0, 20, 40), (9, 18)), None);
    }
}
