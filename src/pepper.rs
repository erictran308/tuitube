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

use std::ffi::OsString;

use ratatui::layout::Rect;

/// The token's random bytes (32 hex digits).
const TOKEN_BYTES: usize = 16;

/// Pepper's port, from `PEPPER_PORT`, when tuitube runs in Pepper: not
/// through tmux, which would need each escape sequence wrapped to pass it on.
pub fn port() -> Option<OsString> {
    port_from(|name| std::env::var_os(name))
}

fn port_from(var: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    if var("TMUX").is_some() {
        return None;
    }
    var("PEPPER_PORT").filter(|port| !port.is_empty())
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

/// Puts the picture of `token`'s playback over `area`.
pub fn place(token: &str, area: Rect) -> String {
    format!(
        "\x1b]777;pepper;place;{token};{};{};{};{}\x1b\\",
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
            place("ab", area),
            "\x1b]777;pepper;place;ab;2;3;40;11\x1b\\"
        );
        assert_eq!(remove("ab"), "\x1b]777;pepper;remove;ab\x1b\\");
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
