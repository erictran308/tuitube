//! The Jukebox as a list (`v`): a row per video, numbered in the order it
//! plays (the one playing marked instead), with its title, its channel and
//! its length, and no thumbnails.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::fit;
use crate::app::{App, Focus, GridShape};
use crate::theme::Colors;
use crate::video::{self, Video};

/// The widest the channel's column gets.
const CHANNEL: usize = 24;
/// Narrower than this, the rows leave the channel out.
const WITH_CHANNEL: usize = 60;
/// The length's column: "10:00:00", "UPCOMING".
const LENGTH: usize = 8;

/// The rows that fit in `area`, scrolled to keep the selected one in view.
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = usize::from(area.height).max(1);
    // One column: up and down move a row, a page is the rows shown.
    app.grid.set(GridShape { cols: 1, rows });
    if app.selected < app.scroll {
        app.scroll = app.selected;
    } else if app.selected >= app.scroll + rows {
        app.scroll = app.selected + 1 - rows;
    }
    let first = app.scroll.min(app.videos.len());
    let last = (first + rows).min(app.videos.len());
    let visible: Vec<Video> = app.videos[first..last].to_vec();
    let focused = app.focus == Focus::Grid && app.prompt.is_none();
    let digits = app.videos.len().to_string().len();
    for (i, video) in visible.iter().enumerate() {
        if video.duration.is_none() {
            app.want_length(video);
        }
        let selected = first + i == app.selected;
        let row = Rect {
            y: area.y + i as u16,
            height: 1,
            ..area
        };
        let mark = app
            .jukebox_state(&video.id)
            .map(|state| super::jukebox_mark(state, &app.icons, &app.colors));
        let line = row_line(
            &app.colors,
            video,
            Place {
                number: first + i + 1,
                digits,
                mark,
            },
            usize::from(area.width),
            selected && focused,
        );
        let line = if selected {
            line.style(Style::new().bg(app.colors.selected))
        } else {
            line
        };
        frame.render_widget(Paragraph::new(line), row);
    }
}

/// A video's place in the Jukebox: its number, as wide as the longest,
/// or the mark of the one playing.
struct Place {
    number: usize,
    digits: usize,
    mark: Option<(&'static str, &'static str, Style)>,
}

/// A video's row, `width` columns: a bar if it's selected and has the
/// keys, its place in the Jukebox, its title, channel and length.
fn row_line(c: &Colors, video: &Video, place: Place, width: usize, lit: bool) -> Line<'static> {
    let bar = if lit {
        Span::styled("▌", Style::new().fg(c.accent))
    } else {
        Span::raw(" ")
    };
    let digits = place.digits;
    let number = match place.mark {
        Some((icon, _, style)) => Span::styled(format!("{icon:>digits$}  "), style),
        None => Span::styled(
            format!("{:>digits$}  ", place.number),
            Style::new().fg(if lit { c.accent } else { c.dim }),
        ),
    };
    let (length, length_color) = if video.live {
        ("LIVE".to_string(), c.red)
    } else if let Some(d) = video.duration {
        (video::duration(d), c.subtext)
    } else if video.upcoming {
        ("UPCOMING".to_string(), c.dim)
    } else if video.short {
        ("Shorts".to_string(), c.dim)
    } else {
        (String::new(), c.dim)
    };
    let length = Span::styled(
        format!("  {length:>LENGTH$} "),
        Style::new().fg(length_color),
    );
    let channel_width = if width >= WITH_CHANNEL {
        CHANNEL.min(width / 4)
    } else {
        0
    };
    let taken = bar.width() + number.width() + length.width();
    let gap = if channel_width > 0 { 2 } else { 0 };
    let title_width = width.saturating_sub(taken + gap + channel_width);
    let title = fit(&video.title, title_width);
    let title_style = if lit {
        Style::new().fg(c.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(c.text).add_modifier(Modifier::BOLD)
    };
    let pad = title_width.saturating_sub(title.width());
    let mut spans = vec![
        bar,
        number,
        Span::styled(title, title_style),
        Span::raw(" ".repeat(pad + gap)),
    ];
    if channel_width > 0 {
        let channel = fit(&video.channel, channel_width);
        let pad = channel_width.saturating_sub(channel.width());
        spans.push(Span::styled(channel, Style::new().fg(c.subtext)));
        spans.push(Span::raw(" ".repeat(pad)));
    }
    spans.push(length);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::JukeboxState;
    use crate::store::tests::video;
    use crate::theme;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_row_fills_its_width_with_the_length_on_the_right() {
        let c = Colors::named(theme::DEFAULT);
        let mut v = video("aaaaaaaaaa1", "UC7EVSn5inapL20oPSwAwEUg", None);
        v.duration = Some(754);
        let place = |mark| Place {
            number: 3,
            digits: 2,
            mark,
        };
        for width in [0, 10, 40, 59, 60, 120] {
            let line = row_line(&c, &v, place(None), width, false);
            if width >= 40 {
                assert_eq!(line.width(), width, "{width}: {}", text(&line));
            }
            if width >= 60 {
                assert!(text(&line).contains("Channel"), "{}", text(&line));
            }
        }
        let row = text(&row_line(&c, &v, place(None), 80, true));
        assert!(row.starts_with("▌ 3  Video aaaaaaaaaa1"), "{row}");
        assert!(row.ends_with("    12:34 "), "{row}");
        assert!(!text(&row_line(&c, &v, place(None), 50, false)).contains("Channel"));
        // The one playing: its mark instead of its number, in red.
        let mark = super::super::jukebox_mark(JukeboxState::Playing, &crate::icons::PLAIN, &c);
        let line = row_line(&c, &v, place(Some(mark)), 80, false);
        assert!(
            text(&line).starts_with("  ▶  Video aaaaaaaaaa1"),
            "{}",
            text(&line)
        );
        assert_eq!(line.width(), 80);
        assert_eq!(line.spans[1].style.fg, Some(c.red));
    }
}
