//! The videos as a list (`list` in settings, or the Jukebox alone with
//! `jukebox_list`; `v` switches): two rows per video and a blank one
//! above, no thumbnails. The first has its title, under the view's name and
//! the search box, and a line as long as the window allows, with the part
//! you watched in red and the length at its end (as the player bar); the
//! second its channel, views and age, as the cards say them. The Jukebox's
//! videos are numbered in the order it plays (the one playing marked
//! instead), the numbers where the titles start elsewhere.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::fit;
use crate::app::{App, Focus, GridShape, View, now};
use crate::ids::VideoId;
use crate::theme::Colors;
use crate::video::{self, Video};

/// The rows each video takes: a blank one, to set it apart from the one
/// before (or the view's name), then its two.
const ROWS: usize = 3;
/// The line's cells: at least this, and a sixth of the row in a wider
/// window, up to the most.
const LINE_MIN: usize = 8;
const LINE_MAX: usize = 60;
/// Columns between the title and the time watched.
const GAP: usize = 2;
/// The fewest columns the title keeps: narrower than that, the line (and
/// the time watched) goes. The title and the length always stay.
const MIN_TITLE: usize = 30;

/// The videos that fit in `area`, scrolled to keep the selected one in
/// view, and the start of the next one in what's left. `area` starts a
/// column left of where the titles do, for the selected one's mark.
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = usize::from(area.height);
    // One column: up and down move a video, a page is the videos shown.
    let whole = (rows / ROWS).max(1);
    app.grid.set(GridShape {
        cols: 1,
        rows: whole,
    });
    if app.selected < app.scroll {
        app.scroll = app.selected;
    } else if app.selected >= app.scroll + whole {
        app.scroll = app.selected + 1 - whole;
    }
    let first = app.scroll.min(app.videos.len());
    let last = (first + rows.div_ceil(ROWS).max(1)).min(app.videos.len());
    let visible: Vec<Video> = app.videos[first..last].to_vec();
    let focused = app.focus == Focus::Grid && app.prompt.is_none();
    let jukebox = app.view == View::Jukebox;
    let digits = app.videos.len().to_string().len();
    let now = now();
    // From every video of the view, not just the ones shown, so the lines
    // stay put while it scrolls.
    let (mut times, mut lengths) = (0, 0);
    for v in &app.videos {
        let watched = watched(app, &v.id);
        lengths = lengths.max(length_label(v, watched).0.width());
        times = times.max(length_text(total(v, watched)).width());
    }
    for (i, video) in visible.iter().enumerate() {
        if video.duration.is_none() {
            app.want_length(video);
        }
        app.want_details(video);
        app.want_title(video);
        let selected = first + i == app.selected;
        let y = area.y + (i * ROWS) as u16;
        let area = Rect {
            y,
            height: (ROWS as u16).min(area.bottom().saturating_sub(y)),
            ..area
        };
        let row = Row {
            // Numbered only in the Jukebox, which plays in that order.
            place: jukebox.then(|| Place {
                number: first + i + 1,
                digits,
                mark: app
                    .jukebox_state(&video.id)
                    .map(|state| super::jukebox_mark(state, &app.icons, &app.colors)),
            }),
            marks: marks(app, video),
            watched: watched(app, &video.id),
            times,
            lengths,
            lit: selected && focused,
        };
        let shown = Video {
            title: app.title(video).to_string(),
            ..video.clone()
        };
        let lines = row_lines(&app.colors, &shown, row, now, usize::from(area.width));
        let lines = lines.map(|line| {
            if selected {
                line.style(Style::new().bg(app.colors.selected))
            } else {
                line
            }
        });
        let mut rows = vec![Line::default()];
        rows.extend(lines);
        frame.render_widget(Paragraph::new(rows), area);
    }
}

/// Where you stopped in a video and its length, in seconds, if you watched
/// some of it here.
fn watched(app: &App, id: &VideoId) -> Option<(f64, f64)> {
    app.progress
        .get(id)
        .copied()
        .filter(|&(_, length)| length > 0.0)
}

/// What a video's rows show besides the video.
struct Row {
    /// Its place in the Jukebox: videos are numbered only there.
    place: Option<Place>,
    /// Saved to Watch later, its place in the Jukebox.
    marks: Vec<Span<'static>>,
    /// Where you stopped in it and its length, if you watched it.
    watched: Option<(f64, f64)>,
    /// The columns of the time watched, before the line: the longest
    /// length of the view, kept on every row so the lines line up. 0 when
    /// no video of the view has a length: no line then.
    times: usize,
    /// The columns of the length, after the line: the longest of the view.
    lengths: usize,
    /// Selected, with the keys.
    lit: bool,
}

/// A video's place in the Jukebox: its number, as wide as the longest,
/// or the mark of the one playing.
struct Place {
    number: usize,
    digits: usize,
    mark: Option<(&'static str, &'static str, Style)>,
}

/// What a card says after its numbers: saved to Watch later, and, outside
/// the Jukebox, its place in it.
fn marks(app: &App, video: &Video) -> Vec<Span<'static>> {
    let style = Style::new().fg(app.colors.accent);
    let mut marks = Vec::new();
    if app.watch_later.contains(&video.id) {
        marks.push(Span::styled(format!(" {}", app.icons.watch_later), style));
    }
    if app.view != View::Jukebox
        && let Some(place) = app.jukebox.iter().position(|v| v.id == video.id)
    {
        marks.push(Span::styled(
            format!(" {}{}", app.icons.jukebox, place + 1),
            style,
        ));
    }
    marks
}

/// A video's two rows, `width` columns, each with an edge if it's selected
/// and has the keys. The first: its place in the Jukebox, its title and
/// marks, the time watched, the line and the length (the line only while
/// the title keeps [`MIN_TITLE`] columns, from the width and the view
/// alone, so the lines line up). The second, under the title: its channel,
/// views and age.
fn row_lines(c: &Colors, video: &Video, row: Row, now: i64, width: usize) -> [Line<'static>; 2] {
    let edge = if row.lit {
        Span::styled("▌", Style::new().fg(c.accent))
    } else {
        Span::raw(" ")
    };
    let number = match row.place {
        None => Span::raw(" "),
        Some(Place {
            mark: Some((icon, _, style)),
            digits,
            ..
        }) => Span::styled(format!(" {icon:>digits$}  "), style),
        Some(Place { number, digits, .. }) => Span::styled(
            format!(" {number:>digits$}  "),
            Style::new().fg(if row.lit { c.accent } else { c.dim }),
        ),
    };
    // The second row starts under the title.
    let lead = edge.width() + number.width();
    let info = info_line(video, now, width.saturating_sub(lead + 1));

    // The length always shows, and a column at the right end, as at the
    // left. The line while the title keeps its room, longer in a wider
    // window, from what the title can spare.
    let mut spare = width.saturating_sub(lead + GAP + row.lengths + 1 + MIN_TITLE);
    let line_min = row.times + 1 + LINE_MIN + 1;
    let with_line = row.times > 0 && line_min <= spare;
    let cells = if with_line {
        spare -= line_min;
        LINE_MIN + spare.min((width / 6).clamp(LINE_MIN, LINE_MAX) - LINE_MIN)
    } else {
        0
    };
    let (length, known) = length_label(video, row.watched);
    let length_color = if video.live {
        c.red
    } else if known {
        c.subtext
    } else {
        c.dim
    };
    let mut time = vec![Span::raw(" ".repeat(GAP))];
    if with_line {
        time.extend(line_spans(c, row.watched, known, row.times, cells));
    }
    let lengths = row.lengths;
    time.push(Span::styled(
        format!("{:<lengths$} ", fit(&length, lengths)),
        Style::new().fg(length_color),
    ));

    let taken = lead + time.iter().map(Span::width).sum::<usize>();
    let title_width = width.saturating_sub(taken);
    // The marks only beside a title that keeps most of its room.
    let marks_width: usize = row.marks.iter().map(Span::width).sum();
    let marks = if marks_width * 2 <= title_width {
        row.marks
    } else {
        Vec::new()
    };
    let marks_width: usize = marks.iter().map(Span::width).sum();
    let title = fit(&video.title, title_width - marks_width);
    let title_style = if row.lit {
        Style::new().fg(c.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(c.text).add_modifier(Modifier::BOLD)
    };
    let pad = title_width.saturating_sub(title.width() + marks_width);
    let mut first = vec![edge.clone(), number, Span::styled(title, title_style)];
    first.extend(marks);
    first.push(Span::raw(" ".repeat(pad)));
    first.extend(time);
    let pad = width.saturating_sub(lead + info.width());
    let second = vec![
        edge,
        Span::raw(" ".repeat(lead - 1)),
        Span::styled(info, Style::new().fg(c.subtext)),
        Span::raw(" ".repeat(pad)),
    ];
    [Line::from(first), Line::from(second)]
}

/// The channel, views and age, as the cards say them, in `room` columns:
/// the views left out first, then the age, then the channel cut.
fn info_line(video: &Video, now: i64, room: usize) -> String {
    let channel = Some(video.channel.clone()).filter(|name| !name.is_empty());
    let views = video.views.map(video::views);
    let age = video.published.map(|at| video::age(at, now));
    let say = |parts: &[&Option<String>]| {
        parts
            .iter()
            .filter_map(|part| part.as_deref())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    [
        say(&[&channel, &views, &age]),
        say(&[&channel, &age]),
        say(&[&channel]),
    ]
    .into_iter()
    .find(|text| text.width() <= room)
    .unwrap_or_else(|| fit(&say(&[&channel]), room))
}

/// A video's length: YouTube's, or else History's, for a video watched.
fn total(video: &Video, watched: Option<(f64, f64)>) -> Option<u32> {
    video.duration.or(watched.map(|(_, length)| length as u32))
}

/// A length as the line's end says it: "12:34", or nothing.
fn length_text(duration: Option<u32>) -> String {
    duration.map(video::duration).unwrap_or_default()
}

/// What the line's end says: the video's length, or why it has none; and
/// whether it's a length, for the line to measure against.
fn length_label(video: &Video, watched: Option<(f64, f64)>) -> (String, bool) {
    if video.live {
        ("LIVE".into(), false)
    } else if let Some(d) = total(video, watched) {
        (video::duration(d), true)
    } else if video.upcoming {
        ("UPCOMING".into(), false)
    } else if video.short {
        ("Shorts".into(), false)
    } else {
        (String::new(), false)
    }
}

/// The time you stopped at (0:00 for a video not played), in `times`
/// columns, then the line, `cells` long and red as far as you watched, as
/// the player bar: at least a cell of it once you watched a second. Both
/// blank for a video with no length.
fn line_spans(
    c: &Colors,
    watched: Option<(f64, f64)>,
    known: bool,
    times: usize,
    cells: usize,
) -> Vec<Span<'static>> {
    if !known {
        return vec![Span::raw(" ".repeat(times + 1 + cells + 1))];
    }
    let at = watched.map_or(0.0, |(at, length)| at.clamp(0.0, length));
    let mut spans = vec![Span::styled(
        format!("{:>times$} ", fit(&video::duration(at as u32), times)),
        Style::new().fg(c.subtext),
    )];
    let done = watched.map_or(0, |(at, length)| {
        let ratio = (at / length).clamp(0.0, 1.0);
        ((ratio * cells as f64).round() as usize)
            .max(usize::from(at >= 1.0))
            .min(cells)
    });
    spans.push(Span::styled("━".repeat(done), Style::new().fg(c.red)));
    // As thick as the red, in gray.
    spans.push(Span::styled(
        "━".repeat(cells - done),
        Style::new().fg(c.border),
    ));
    spans.push(Span::raw(" "));
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::JukeboxState;
    use crate::store::tests::video;
    use crate::theme;

    const NOW: i64 = 1_800_000_000;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The column `needle` starts at in `row`.
    fn column(row: &str, needle: &str) -> usize {
        row[..row
            .find(needle)
            .unwrap_or_else(|| panic!("{needle}: {row}"))]
            .width()
    }

    /// The line's cells in `line`: red, then gray.
    fn cells(line: &Line) -> (usize, usize) {
        let c = Colors::named(theme::DEFAULT);
        let count = |color| {
            line.spans
                .iter()
                .filter(|s| s.content.starts_with('━') && s.style.fg == Some(color))
                .map(Span::width)
                .sum()
        };
        (count(c.red), count(c.border))
    }

    fn known() -> Video {
        let mut v = video(
            "aaaaaaaaaa1",
            "UC7EVSn5inapL20oPSwAwEUg",
            Some(NOW - 3 * 86_400),
        );
        v.duration = Some(754);
        v.views = Some(1_250);
        v
    }

    /// A row not played, outside the Jukebox, in a view whose longest
    /// length is "12:34".
    fn plain() -> Row {
        Row {
            place: None,
            marks: Vec::new(),
            watched: None,
            times: 5,
            lengths: 5,
            lit: false,
        }
    }

    /// The row of a video watched to `at`.
    fn watched(at: f64) -> Row {
        Row {
            watched: Some((at, 754.0)),
            ..plain()
        }
    }

    fn numbered(mark: Option<(&'static str, &'static str, Style)>, lit: bool) -> Row {
        Row {
            place: Some(Place {
                number: 3,
                digits: 2,
                mark,
            }),
            lit,
            ..plain()
        }
    }

    /// A video's rows, `width` columns.
    fn lines(video: &Video, row: Row, width: usize) -> [Line<'static>; 2] {
        row_lines(&Colors::named(theme::DEFAULT), video, row, NOW, width)
    }

    /// The same, as text.
    fn rows(video: &Video, row: Row, width: usize) -> [String; 2] {
        lines(video, row, width).map(|line| text(&line))
    }

    #[test]
    fn a_video_takes_two_rows_its_title_and_line_then_what_youtube_says() {
        let c = Colors::named(theme::DEFAULT);
        let v = known();
        for width in [0, 10, 40, 59, 60, 120] {
            let both = lines(&v, numbered(None, false), width);
            if width >= 40 {
                for line in &both {
                    assert_eq!(line.width(), width, "{width}: {}", text(line));
                }
                assert!(text(&both[0]).contains("12:34"), "{}", text(&both[0]));
            }
        }
        let [first, second] = rows(&v, numbered(None, true), 80);
        // The number where other views' titles start, the title after it.
        assert!(first.starts_with("▌  3  Video aaaaaaaaaa1"), "{first}");
        assert!(first.contains(" 0:00 ━"), "not played: {first}");
        assert!(first.ends_with("━ 12:34 "), "{first}");
        assert_eq!(
            second.trim_end(),
            "▌     Channel · 1.2K views · 3 days ago",
            "under the title"
        );
        // Narrow: no line, and the length stays.
        let [narrow, _] = rows(&v, numbered(None, false), 50);
        assert!(!narrow.contains('━'), "{narrow}");
        assert!(narrow.ends_with("  12:34 "), "{narrow}");
        // The one playing: its mark instead of its number, in red.
        let mark = super::super::jukebox_mark(JukeboxState::Playing, &crate::icons::PLAIN, &c);
        let both = lines(&v, numbered(Some(mark), false), 80);
        assert!(
            text(&both[0]).starts_with("   ▶  Video aaaaaaaaaa1"),
            "{}",
            text(&both[0])
        );
        assert_eq!(both[0].spans[1].style.fg, Some(c.red));
        assert!(text(&both[1]).starts_with("      Channel ·"));
        // A video YouTube said nothing about: no length, no line.
        let bare = video("aaaaaaaaaa2", "UC7EVSn5inapL20oPSwAwEUg", None);
        let both = lines(&bare, plain(), 120);
        assert_eq!(cells(&both[0]), (0, 0));
        assert!(!text(&both[0]).contains("0:00"), "{}", text(&both[0]));
        assert_eq!(text(&both[1]).trim_end(), "  Channel");
        assert_eq!((both[0].width(), both[1].width()), (120, 120));
    }

    #[test]
    fn the_second_row_drops_the_views_then_the_age_then_cuts_the_channel() {
        let v = known();
        let info = |room| info_line(&v, NOW, room);
        assert_eq!(info(80), "Channel · 1.2K views · 3 days ago");
        assert_eq!(info(33), "Channel · 1.2K views · 3 days ago");
        assert_eq!(info(32), "Channel · 3 days ago");
        assert_eq!(info(20), "Channel · 3 days ago");
        assert_eq!(info(19), "Channel");
        assert_eq!(info(5), "Chan…");
        assert_eq!(info(0), "");
    }

    #[test]
    fn the_line_shows_on_every_row_red_as_far_as_you_watched() {
        let c = Colors::named(theme::DEFAULT);
        let v = known();
        let [half, _] = lines(&v, watched(377.0), 160);
        let row = text(&half);
        assert_eq!(half.width(), 160);
        assert!(row.contains("6:17 ━"), "{row}");
        assert!(row.ends_with("━ 12:34 "), "{row}");
        assert!(!row.contains('●'), "no dot: {row}");
        assert!(!row.contains('─'), "the gray as thick as the red: {row}");
        let (red, rest) = cells(&half);
        assert!(red.abs_diff(rest) <= 1, "half: {row}");
        assert!(
            half.spans
                .iter()
                .any(|s| s.content.starts_with('━') && s.style.fg == Some(c.red)),
            "red, as the player bar"
        );
        // Barely started: still a cell of red.
        let [barely, _] = lines(&v, watched(7.0), 160);
        assert_eq!(cells(&barely).0, 1, "{}", text(&barely));
        assert!(text(&barely).contains("0:07 ━━"), "{}", text(&barely));

        // Not played: 0:00 and the line all gray, from the same column.
        let [other_line, _] = lines(&v, plain(), 160);
        let other = text(&other_line);
        assert_eq!(cells(&other_line), (0, red + rest), "as long: {other}");
        assert!(other.contains(" 0:00 ━"), "{other}");
        assert_eq!(column(&other, "━"), column(&row, "━"), "{other}\n{row}");
        // In a view where no video has a length, no line: the title takes
        // its room.
        let mut unknown = known();
        unknown.duration = None;
        unknown.title =
            "A rather long title that goes on and on, past where the line would be".into();
        let no_lengths = Row {
            times: 0,
            lengths: 0,
            ..plain()
        };
        let [none, _] = rows(&unknown, no_lengths, 100);
        assert!(!none.contains('━') && !none.contains("0:00"), "{none}");
        assert!(none.contains("past where the line would be"), "{none}");

        // Longer in a wider window.
        let [wider, _] = lines(&v, watched(377.0), 260);
        let (red_wide, rest_wide) = cells(&wider);
        assert!(red_wide + rest_wide > red + rest, "{}", text(&wider));
        assert!(red_wide + rest_wide <= LINE_MAX);

        // With a video of an hour or more in the view, its times take 7
        // columns on every row, so the lines still line up.
        let hour_view = |at: f64, length: f64| Row {
            watched: Some((at, length)),
            times: 7,
            lengths: 7,
            ..plain()
        };
        let mut hour = known();
        hour.duration = Some(7_200);
        let [long, _] = rows(&hour, hour_view(3_723.0, 7_200.0), 160);
        let [short, _] = rows(&v, hour_view(377.0, 754.0), 160);
        assert_eq!(column(&long, "━"), column(&short, "━"), "{long}\n{short}");
        assert!(long.contains("1:02:03 ━"), "{long}");
        assert!(long.ends_with("━ 2:00:00 "), "{long}");
        assert!(short.contains("   6:17 ━"), "{short}");
        assert!(short.ends_with("━ 12:34   "), "{short}");

        // Live: no length to measure against.
        let mut live = known();
        live.duration = None;
        live.live = true;
        let [line, _] = lines(&live, plain(), 160);
        let row = text(&line);
        assert!(row.ends_with("  LIVE  "), "as wide as 12:34: {row}");
        assert_eq!(cells(&line), (0, 0), "{row}");
        assert!(!row.contains("0:00"), "{row}");
    }

    #[test]
    fn the_line_goes_before_the_title_gets_short() {
        let v = known();
        let mut last = true;
        for width in (20..=200).rev() {
            let [first, _] = lines(&v, watched(100.0), width);
            let row = text(&first);
            let shown = row.contains('━');
            assert!(last || !shown, "{width}: it doesn't come back narrower");
            last = shown;
            // The title and the length stay, the title cut if it must be.
            assert!(row.starts_with("  Video"), "{width}: {row}");
            assert!(row.ends_with("12:34 "), "{width}: {row}");
            if shown {
                // No marks: the title, then its padding.
                let room = first.spans[2].width() + first.spans[3].width();
                assert!(room >= MIN_TITLE, "{width}: {row}");
            }
        }
        // The edge and space, the title, the time, the shortest line, the
        // length and the column at the end.
        let least = 2 + MIN_TITLE + GAP + 5 + 1 + LINE_MIN + 1 + 5 + 1;
        assert!(rows(&v, watched(100.0), least)[0].contains('━'));
        assert!(!rows(&v, watched(100.0), least - 1)[0].contains('━'));
    }

    #[test]
    fn marks_follow_the_title_while_it_has_room() {
        let v = known();
        let marked = || Row {
            marks: vec![Span::raw(" ◷"), Span::raw(" ≡2")],
            ..plain()
        };
        let [line, _] = lines(&v, marked(), 100);
        assert!(
            text(&line).contains("Video aaaaaaaaaa1 ◷ ≡2 "),
            "{}",
            text(&line)
        );
        assert_eq!(line.width(), 100);
        let [line, _] = lines(&v, marked(), 18);
        assert!(!text(&line).contains('◷'), "{}", text(&line));
    }
}
