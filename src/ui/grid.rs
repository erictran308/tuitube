//! The videos, in a box with the search box in its border: what they are,
//! then a grid of cards: thumbnail with its length, the watched part under
//! it, then the channel's photo beside the title, the channel's name, views
//! and age, and the start of the description. The Jukebox can be a list
//! instead (`list.rs`).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui_image::{FontSize, Image};
use unicode_width::UnicodeWidthStr;

use super::fit;
use crate::app::{App, Focus, GridShape, View, now};
use crate::images::{Key, Subject};
use crate::theme::Colors;
use crate::video::{self, Video};

/// The red behind LIVE and Shorts, as YouTube's own: white reads on it in
/// every theme, which a theme's pastel red doesn't promise.
const YOUTUBE_RED: Color = Color::Rgb(204, 0, 0);

/// Columns the channel photo takes beside the title, and the gap after it.
const AVATAR_COLS: u16 = 4;
const AVATAR_ROWS: u16 = 2;

/// Columns between the edge of a card's words' shade and what's on it.
const PAD: u16 = 1;

/// How the cards are laid out in an area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub cols: usize,
    pub rows: usize,
    /// A card's place, ring included.
    pub slot_width: u16,
    pub slot_height: u16,
    /// A card inside its ring.
    pub card_width: u16,
    pub thumb_rows: u16,
}

/// As many cards of at least `min_width` columns as fit side by side; the
/// thumbnail 16:9 in pixels, from the terminal's cell size.
pub fn geometry(area: Rect, min_width: u16, font: FontSize, descriptions: bool) -> Geometry {
    let cols = (area.width / (min_width + 2)).max(1);
    let slot_width = area.width / cols;
    let card_width = slot_width.saturating_sub(2).max(1);
    let (fw, fh) = (f32::from(font.width.max(1)), f32::from(font.height.max(1)));
    let thumb_rows = (f32::from(card_width) * fw * 9.0 / 16.0 / fh)
        .round()
        .clamp(3.0, 40.0) as u16;
    // Ring, thumbnail, the watched bar, title (2), channel, numbers,
    // description, a blank row closing the words' shade, ring.
    let slot_height = 1 + thumb_rows + 1 + 4 + u16::from(descriptions) + 1 + 1;
    let rows = (area.height / slot_height).max(1) as usize;
    Geometry {
        cols: cols as usize,
        rows,
        slot_width,
        slot_height,
        card_width,
        thumb_rows,
    }
}

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let c = app.colors.clone();
    // The search box is in the border: it's lit while you type in it.
    let line = super::border(app.focus == Focus::Grid || app.prompt.is_some(), &c);
    // How the feeds' update goes, where the header doesn't say it.
    let refresh = match app.refreshing {
        Some((done, total)) if !matches!(app.view, View::Home | View::Shorts) => {
            Line::from(Span::styled(
                format!(" {} {done}/{total} ", app.icons.refresh),
                Style::new().fg(c.dim),
            ))
        }
        _ => Line::default(),
    };
    let search = super::search_box(app, area.width, refresh.width() as u16, line);
    let block = super::bordered()
        .border_style(line)
        .title(search)
        .title(refresh.right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let header = Rect {
        height: inner.height.min(1),
        ..inner
    };
    // A column in from the border, so the selected card's ring doesn't
    // touch it.
    let cards = Rect {
        x: inner.x + 1,
        y: inner.y + 1,
        width: inner.width.saturating_sub(2),
        height: inner.height.saturating_sub(1),
    };
    draw_header(frame, app, header);
    if app.videos.is_empty() {
        empty(frame, app, cards);
        return;
    }
    if app.view == View::Jukebox && app.settings.jukebox_list {
        return super::list::draw(frame, app, cards);
    }
    let g = geometry(
        cards,
        app.settings.card_width(),
        app.images.font_size(),
        app.settings.descriptions,
    );
    app.grid.set(GridShape {
        cols: g.cols,
        rows: g.rows,
    });
    let row = app.selected / g.cols;
    if row < app.scroll {
        app.scroll = row;
    } else if row >= app.scroll + g.rows {
        app.scroll = row + 1 - g.rows;
    }
    // The rows that fit, and the start of the next one in what's left, as
    // YouTube's page shows the top of the next row.
    let shown_rows = usize::from(cards.height)
        .div_ceil(usize::from(g.slot_height))
        .max(1);
    let first = app.scroll * g.cols;
    let last = (first + shown_rows * g.cols).min(app.videos.len());
    let visible: Vec<Video> = app.videos[first..last].to_vec();
    for (i, video) in visible.iter().enumerate() {
        let slot = Rect {
            x: cards.x + (i % g.cols) as u16 * g.slot_width,
            y: cards.y + (i / g.cols) as u16 * g.slot_height,
            width: g.slot_width,
            height: g.slot_height,
        };
        let selected = first + i == app.selected;
        card(frame, app, &c, video, slot, cards, &g, selected);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;
    let count = match app.videos.len() {
        0 => String::new(),
        1 => "  1 video".into(),
        n => format!("  {n} videos"),
    };
    let mut spans = vec![
        Span::styled(
            // Two columns in: over the thumbnails' left edge, and in line
            // with the search box.
            format!("  {}", fit(&app.view.title(), area.width as usize / 2)),
            Style::new().fg(c.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(count, Style::new().fg(c.dim)),
    ];
    // How long the Jukebox plays, once every length is known.
    if app.view == View::Jukebox
        && let Some(total) = app
            .videos
            .iter()
            .map(|v| v.duration)
            .sum::<Option<u32>>()
            .filter(|&total| total > 0)
    {
        spans.push(Span::styled(
            format!("  ·  {}", video::duration(total)),
            Style::new().fg(c.dim),
        ));
    }
    if let Some(loading) = &app.loading {
        spans.push(Span::styled(
            format!("  {loading}"),
            Style::new().fg(c.accent),
        ));
    }
    // When Home and Shorts were last brought up to date, so it's never a
    // guess how fresh they are.
    if matches!(app.view, View::Home | View::Shorts) {
        let freshness = match (app.refreshing, app.feeds_updated) {
            (Some((done, total)), _) => Some((format!("  ·  updating {done}/{total}"), c.accent)),
            (None, Some(at)) => Some((format!("  ·  updated {}", video::age(at, now())), c.dim)),
            (None, None) => None,
        };
        if let Some((text, color)) = freshness {
            spans.push(Span::styled(text, Style::new().fg(color)));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// What a view says when it has no videos.
fn empty(frame: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;
    let lines: Vec<(&str, Style)> = match &app.view {
        _ if app.loading.is_some() => vec![("Loading…", Style::new().fg(c.subtext))],
        View::Home if app.subscriptions.is_empty() => vec![
            (
                "Welcome to tuitube",
                Style::new().fg(c.text).add_modifier(Modifier::BOLD),
            ),
            ("", Style::new()),
            (
                "Bring your YouTube subscriptions over. No account or login needed:",
                Style::new().fg(c.subtext),
            ),
            ("", Style::new()),
            (
                "1. Open takeout.google.com and deselect everything but “YouTube and YouTube Music”",
                Style::new().fg(c.text),
            ),
            (
                "2. Under “All YouTube data included”, keep only “subscriptions”",
                Style::new().fg(c.text),
            ),
            ("3. Export, download and unzip it", Style::new().fg(c.text)),
            (
                "4. Press I here and paste the path to subscriptions.csv",
                Style::new().fg(c.text),
            ),
            ("", Style::new()),
            (
                "Or press / to search, and S on a video to subscribe to its channel.",
                Style::new().fg(c.dim),
            ),
            (
                "Google Takeout exports stay on your computer: tuitube never logs in.",
                Style::new().fg(c.dim),
            ),
        ],
        View::Home | View::Shorts if app.refreshing.is_some() => {
            vec![(
                "Fetching your subscriptions' newest videos…",
                Style::new().fg(c.subtext),
            )]
        }
        View::Home => vec![(
            "No videos yet. R fetches your subscriptions' newest ones.",
            Style::new().fg(c.subtext),
        )],
        View::Shorts => vec![(
            "No Shorts from your subscriptions lately.",
            Style::new().fg(c.subtext),
        )],
        View::Search(q) if q.is_empty() => {
            vec![("Press / to search YouTube.", Style::new().fg(c.subtext))]
        }
        View::Search(_) => vec![("Nothing found.", Style::new().fg(c.subtext))],
        View::WatchLater => vec![(
            "Nothing saved yet. Press w on a video to watch it later.",
            Style::new().fg(c.subtext),
        )],
        View::History if !app.settings.history => {
            vec![(
                "History is off (history = false in settings.toml).",
                Style::new().fg(c.subtext),
            )]
        }
        View::History => vec![(
            "Videos you play here show up here. They're kept only on this computer.",
            Style::new().fg(c.subtext),
        )],
        View::Channel(id, _) if app.is_gone(id) => vec![(
            "YouTube removed this channel. x on it in the sidebar unsubscribes.",
            Style::new().fg(c.subtext),
        )],
        View::Channel(..) => vec![(
            "No videos. Press R to fetch the channel's newest.",
            Style::new().fg(c.subtext),
        )],
        View::Mix(..) => vec![("No Mix for this video.", Style::new().fg(c.subtext))],
        View::Jukebox => vec![
            ("The Jukebox is empty.", Style::new().fg(c.subtext)),
            ("", Style::new()),
            (
                "Press e on a video to put it in, E to put it first.",
                Style::new().fg(c.dim),
            ),
            (
                "It plays through in order, each video leaving it as it plays.",
                Style::new().fg(c.dim),
            ),
        ],
    };
    let width = lines.iter().map(|(l, _)| l.width()).max().unwrap_or(0) as u16;
    let height = lines.len() as u16;
    let block = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 3,
        width: width.min(area.width),
        height: height.min(area.height),
    };
    let text: Vec<Line> = lines
        .into_iter()
        .map(|(l, s)| Line::from(Span::styled(fit(l, area.width as usize), s)))
        .collect();
    frame.render_widget(Paragraph::new(text), block);
}

/// A color for a channel's initial, the same every time for one name.
fn initial_color(name: &str, c: &Colors) -> Color {
    let palette = [c.red, c.warning, c.ok, c.accent, c.live, c.subtext];
    let hash = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    palette[hash as usize % palette.len()]
}

/// Whether all of `r` is within `bounds`.
fn inside(r: Rect, bounds: Rect) -> bool {
    r.intersection(bounds) == r
}

/// A card in `slot`, of which only what's within `bounds` is drawn: the
/// last row may be cut off by the window's bottom.
#[allow(clippy::too_many_arguments)]
fn card(
    frame: &mut Frame,
    app: &mut App,
    c: &Colors,
    video: &Video,
    slot: Rect,
    bounds: Rect,
    g: &Geometry,
    selected: bool,
) {
    let focused = app.focus == Focus::Grid && app.prompt.is_none();
    if selected && inside(slot, bounds) {
        let ring = if focused { c.accent } else { c.border };
        frame.render_widget(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(ring)),
            slot,
        );
    }
    let inner = Rect {
        x: slot.x + 1,
        y: slot.y + 1,
        width: g.card_width,
        height: slot.height.saturating_sub(2),
    };

    // The thumbnail, or a placeholder until it's ready.
    let thumb = Rect {
        height: g.thumb_rows,
        ..inner
    };
    let shown = thumb.intersection(bounds);
    if shown.is_empty() {
        return;
    }
    let whole = shown == thumb;
    // Kitty's and block images can be cut; sixel and iTerm2 ones can't.
    let cuttable = !app.images.paints_over();
    let covered = app.popup_open() && app.images.paints_over();
    let key = Key {
        subject: Subject::Thumbnail(video.id.clone()),
        cols: thumb.width,
        rows: thumb.height,
    };
    let drawn = !covered
        && (whole || cuttable)
        && match app.images.get(&key, &video.thumbnail_urls()) {
            Some(image) if whole => {
                frame.render_widget(Image::new(image), thumb);
                true
            }
            Some(image) => {
                frame.render_widget(Image::new(image).allow_clipping(true), shown);
                true
            }
            None => false,
        };
    if drawn {
        app.images.place(&key.subject, thumb, shown);
    }
    if !drawn {
        frame.render_widget(Block::new().style(Style::new().bg(c.selected)), shown);
        let mark = if app.images.is_broken(&key) {
            "✕"
        } else {
            app.icons.play
        };
        let middle = Rect {
            x: thumb.x + thumb.width / 2,
            y: thumb.y + thumb.height / 2,
            width: 1,
            height: 1,
        };
        if inside(middle, bounds) {
            frame.render_widget(
                Paragraph::new(Span::styled(mark, Style::new().fg(c.dim))),
                middle,
            );
        }
    }

    // The length, over the thumbnail's corner, as YouTube shows it; under
    // it for terminals that would paint the image over it.
    if video.duration.is_none() {
        app.want_length(video);
    }
    let badge = if video.live {
        Some((" LIVE ".to_string(), YOUTUBE_RED))
    } else if let Some(d) = video.duration {
        Some((format!(" {} ", video::duration(d)), Color::Rgb(15, 15, 15)))
    } else if video.upcoming {
        Some((" UPCOMING ".to_string(), Color::Rgb(15, 15, 15)))
    } else if video.short {
        Some((" Shorts ".to_string(), YOUTUBE_RED))
    } else {
        None
    };
    let badge_over = !app.images.paints_over() || !drawn;
    if let Some((text, bg)) = &badge
        && badge_over
    {
        let w = text.width() as u16;
        let at = Rect {
            x: (thumb.x + thumb.width).saturating_sub(w + 1),
            y: thumb.y + thumb.height - 1,
            width: w,
            height: 1,
        };
        if w + 2 <= thumb.width && inside(at, bounds) {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    text.clone(),
                    Style::new().fg(Color::White).bg(*bg),
                )),
                at,
            );
        }
    }

    // Under the thumbnail, the card's words on a shade of their own, so
    // they stand apart from the background, as YouTube's cards do.
    let words = Rect {
        y: thumb.y + thumb.height,
        height: (inner.y + inner.height).saturating_sub(thumb.y + thumb.height),
        ..inner
    }
    .intersection(bounds);
    if !words.is_empty() {
        frame.render_widget(Block::new().style(Style::new().bg(c.selected)), words);
    }

    // The watched part, red, right under the thumbnail, along the top of
    // the shade.
    let bar = Rect {
        y: thumb.y + thumb.height,
        height: 1,
        ..inner
    };
    if let Some(&(at, length)) = app.progress.get(&video.id)
        && length > 0.0
        && inside(bar, bounds)
    {
        let width = bar.width as usize;
        let watched = ((at / length).clamp(0.0, 1.0) * width as f64).round() as usize;
        let line = Line::from(vec![
            Span::styled("▀".repeat(watched), Style::new().fg(c.red)),
            Span::styled("▀".repeat(width - watched), Style::new().fg(c.border)),
        ]);
        frame.render_widget(Paragraph::new(line), bar);
    }

    // The channel's photo beside the title.
    let text_top = bar.y + 1;
    let show_avatar = inner.width >= 20;
    let photo = Rect {
        x: inner.x + PAD,
        y: text_top,
        width: AVATAR_COLS,
        height: AVATAR_ROWS,
    };
    if show_avatar && inside(photo, bounds) {
        avatar(frame, app, c, video, photo, covered);
    }
    let text_x = if show_avatar {
        inner.x + PAD + AVATAR_COLS + 1
    } else {
        inner.x + PAD
    };
    let text_width = (inner.x + inner.width).saturating_sub(text_x + PAD);
    let w = text_width as usize;

    let title_style = if selected && focused {
        Style::new().fg(c.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(c.text).add_modifier(Modifier::BOLD)
    };
    let mut lines: Vec<Line> = title_lines(&video.title, w)
        .into_iter()
        .map(|l| Line::from(Span::styled(l, title_style)))
        .collect();
    lines.push(Line::from(Span::styled(
        fit(&video.channel, w),
        Style::new().fg(c.subtext),
    )));

    let mut facts: Vec<String> = Vec::new();
    if let Some(views) = video.views {
        facts.push(video::views(views));
    }
    if let Some(published) = video.published {
        facts.push(video::age(published, now()));
    }
    if !badge_over && let Some((text, _)) = &badge {
        facts.push(text.trim().to_string());
    }
    // Playing from the Jukebox: said first, in red.
    let mut meta = Vec::new();
    if app.view == View::Jukebox
        && let Some(state) = app.jukebox_state(&video.id)
    {
        let (icon, word, style) = super::jukebox_mark(state, &app.icons, c);
        meta.push(Span::styled(fit(&format!("{icon} {word} · "), w), style));
    }
    let room = w.saturating_sub(meta.iter().map(Span::width).sum());
    meta.push(Span::styled(
        fit(&facts.join(" · "), room),
        Style::new().fg(c.subtext),
    ));
    if app.watch_later.contains(&video.id) {
        meta.push(Span::styled(
            format!(" {}", app.icons.watch_later),
            Style::new().fg(c.accent),
        ));
    }
    // Its place in the Jukebox, outside it.
    if app.view != View::Jukebox
        && let Some(place) = app.jukebox.iter().position(|v| v.id == video.id)
    {
        meta.push(Span::styled(
            format!(" {}{}", app.icons.jukebox, place + 1),
            Style::new().fg(c.accent),
        ));
    }
    lines.push(Line::from(meta));
    if app.settings.descriptions && !video.description.is_empty() {
        lines.push(Line::from(Span::styled(
            fit(&video.description, w),
            Style::new().fg(c.dim),
        )));
    }
    let text_area = Rect {
        x: text_x,
        y: text_top,
        width: text_width,
        height: (inner.y + inner.height).saturating_sub(text_top),
    }
    .intersection(bounds);
    if !text_area.is_empty() {
        frame.render_widget(Paragraph::new(lines), text_area);
    }
}

/// The title on at most two lines, the second cut with `…` if needed.
fn title_lines(title: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let wrapped = textwrap::wrap(title, width);
    let mut lines: Vec<String> = wrapped.iter().take(2).map(|l| l.to_string()).collect();
    if wrapped.len() > 2
        && let Some(last) = lines.last_mut()
    {
        let rest: Vec<&str> = wrapped[1..].iter().map(|l| l.as_ref()).collect();
        *last = fit(&format!("{}…", rest.join(" ")), width);
        if !last.ends_with('…') {
            *last = fit(&format!("{last}…"), width);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines.resize(2, String::new());
    lines
}

fn avatar(frame: &mut Frame, app: &mut App, c: &Colors, video: &Video, area: Rect, covered: bool) {
    // Photos are only looked up where they can be drawn.
    let url = video
        .channel_id
        .as_ref()
        .filter(|_| app.images.draws_photos())
        .and_then(|id| app.avatar_url(id));
    if let (Some(id), Some(url)) = (&video.channel_id, url)
        && !covered
    {
        let key = Key {
            subject: Subject::Avatar(id.clone()),
            cols: area.width,
            rows: area.height,
        };
        if let Some(image) = app.images.get(&key, std::slice::from_ref(&url)) {
            frame.render_widget(Image::new(image), area);
            app.images.place(&key.subject, area, area);
            return;
        }
    }
    // The channel's initial on a color of its own, until (or instead of)
    // its photo.
    let initial: String = video
        .channel
        .chars()
        .find(|ch| ch.is_alphanumeric())
        .map(|ch| ch.to_uppercase().collect())
        .unwrap_or_else(|| "•".into());
    let bg = initial_color(&video.channel, c);
    let square = Rect {
        x: area.x,
        width: AVATAR_COLS - 1,
        ..area
    };
    frame.render_widget(Block::new().style(Style::new().bg(bg)), square);
    let mark = Rect {
        x: square.x + 1,
        width: 1,
        height: 1,
        ..square
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            initial,
            Style::new().fg(c.bg).add_modifier(Modifier::BOLD),
        )),
        mark,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_are_as_many_as_fit_and_thumbnails_are_16_by_9() {
        let font = FontSize {
            width: 8,
            height: 16,
        };
        let g = geometry(Rect::new(0, 0, 150, 50), 34, font, true);
        assert_eq!(g.cols, 4);
        assert_eq!(g.card_width, 35);
        // 35 columns of 8 px = 280 px wide, so 157.5 px tall: 10 rows of 16.
        assert_eq!(g.thumb_rows, 10);
        assert_eq!(g.slot_height, 19);
        assert_eq!(g.rows, 2);
        let tiny = geometry(Rect::new(0, 0, 10, 5), 34, font, false);
        assert_eq!((tiny.cols, tiny.rows), (1, 1));
    }

    #[test]
    fn long_titles_take_two_lines_and_end_with_an_ellipsis() {
        let lines = title_lines("one two three four five six seven", 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "one two");
        assert!(lines[1].ends_with('…'), "{lines:?}");
        assert!(lines[1].width() <= 10);
        assert_eq!(title_lines("short", 10), ["short", ""]);
    }
}
