//! Drawing: a top bar with the search box, the sidebar, the grid of video
//! cards, the player bar and the status line, laid out like YouTube's page.

mod grid;
mod sidebar;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, Playing, PromptKind};
use crate::video;

/// Below this width the sidebar and the grid take turns.
const NARROW: u16 = 70;

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.images.placed.clear();
    let area = frame.area();
    let c = app.colors.clone();
    frame.render_widget(Block::new().style(Style::new().bg(c.bg).fg(c.text)), area);
    let player_rows = 2 * u16::from(app.playing.is_some() || app.resolving.is_some());
    let [top, body, player, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(player_rows),
        Constraint::Length(1),
    ])
    .areas(area);

    let narrow = body.width < NARROW;
    let side_width = if narrow {
        if app.focus == Focus::Sidebar {
            body.width
        } else {
            0
        }
    } else {
        app.settings.sidebar_width().min(body.width / 2)
    };
    let [side, grid] =
        Layout::horizontal([Constraint::Length(side_width), Constraint::Fill(1)]).areas(body);
    // The search box starts where the thumbnails do: past the grid's margin
    // and a card's ring.
    top_bar(frame, app, top, grid.x + 2);
    if side.width > 0 {
        sidebar::draw(frame, app, side);
    }
    if grid.width > 0 {
        grid::draw(frame, app, grid);
    }
    if player_rows > 0 {
        player_bar(frame, app, player);
    }
    status_bar(frame, app, status);
    if app.help {
        help(frame, app, area);
    }
}

/// `text` cut to `width` columns, with `…` if it was longer.
pub fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width - 1 {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

/// The end of `text` that fits in `width` columns, found in one pass from
/// the end.
fn tail(text: &str, width: usize) -> &str {
    let mut used = 0;
    let mut start = text.len();
    for (i, ch) in text.char_indices().rev() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        start = i;
    }
    &text[start..]
}

/// The logo, the search box in the middle, and how the feeds' update goes.
/// Where the search box may start: right of the logo.
const AFTER_LOGO: u16 = 16;

fn top_bar(frame: &mut Frame, app: &App, area: Rect, content_x: u16) {
    let c = &app.colors;
    // Two columns in, like the sidebar's entries below it.
    let logo = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            " ▶ ",
            Style::new().fg(ratatui::style::Color::White).bg(c.red),
        ),
        Span::styled(
            " tuitube",
            Style::new().fg(c.text).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(logo), area);

    // Left-aligned with the videos below, or right after the logo when
    // the sidebar has the whole window.
    let right = area.x + area.width;
    let mut box_x = content_x.max(area.x + AFTER_LOGO);
    if right.saturating_sub(box_x) < 20 {
        box_x = area.x + AFTER_LOGO;
    }
    let box_width = right.saturating_sub(box_x + 2).min(70);
    let box_area = Rect {
        x: box_x.min(right),
        width: box_width,
        ..area
    };
    let inner = box_width.saturating_sub(4) as usize;
    let line = match &app.prompt {
        Some(prompt) => {
            let label = match prompt.kind {
                PromptKind::Search => "",
                PromptKind::Import => "subscriptions.csv: ",
            };
            // The end of what's typed stays in view.
            let room = inner.saturating_sub(label.width() + 1);
            let shown = tail(&prompt.text, room);
            Line::from(vec![
                Span::styled(format!(" {} ", app.icons.search), Style::new().fg(c.accent)),
                Span::styled(label, Style::new().fg(c.dim)),
                Span::styled(shown, Style::new().fg(c.text)),
                Span::styled("▏", Style::new().fg(c.accent)),
            ])
        }
        None => Line::from(vec![
            Span::styled(format!(" {} ", app.icons.search), Style::new().fg(c.dim)),
            Span::styled(
                fit("Search", inner.saturating_sub(4)),
                Style::new().fg(c.dim),
            ),
        ]),
    };
    let bg = if app.prompt.is_some() {
        c.selected
    } else {
        c.panel
    };
    frame.render_widget(Paragraph::new(line).style(Style::new().bg(bg)), box_area);
    if app.prompt.is_none() && box_width > 12 {
        let hint = Rect {
            x: box_area.x + box_width - 3,
            width: 3,
            ..box_area
        };
        frame.render_widget(
            Paragraph::new(Span::styled(" / ", Style::new().fg(c.dim).bg(c.selected))),
            hint,
        );
    }

    if let Some((done, total)) = app.refreshing {
        let text = format!("{} {done}/{total} ", app.icons.refresh);
        let w = text.width() as u16;
        if area.width > box_area.x + box_width + w {
            let right = Rect {
                x: area.x + area.width - w,
                width: w,
                ..area
            };
            frame.render_widget(
                Paragraph::new(Span::styled(text, Style::new().fg(c.dim))),
                right,
            );
        }
    }
}

/// What's playing, on two rows: the title (and what plays next, when
/// there's room), then how far along it is and the keys that control it.
fn player_bar(frame: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;
    frame.render_widget(Block::new().style(Style::new().bg(c.panel)), area);
    let width = area.width as usize;
    let (title, progress) = if let Some(playing) = &app.playing {
        (
            title_row(app, playing, width),
            progress_row(app, playing, width),
        )
    } else if let Some(resolving) = &app.resolving {
        let loading = Line::from(vec![
            Span::styled(
                format!(" {} ", app.icons.loading),
                Style::new().fg(c.accent),
            ),
            Span::styled(
                fit(
                    &format!("Loading “{}”…", resolving.video.title),
                    width.saturating_sub(4),
                ),
                Style::new().fg(c.subtext),
            ),
        ]);
        (loading, Line::default())
    } else {
        (Line::default(), Line::default())
    };
    let [top, bottom] = Layout::vertical([Constraint::Length(1); 2]).areas(area);
    frame.render_widget(Paragraph::new(title), top);
    frame.render_widget(Paragraph::new(progress), bottom);
}

/// "Next: …" shows only with this many columns to spare beside the title.
const MIN_NEXT: usize = 24;

/// Playing or paused, sound only or not, the title and channel, and on the
/// right what autoplay plays next, if the title leaves room for it.
fn title_row(app: &App, playing: &Playing, width: usize) -> Line<'static> {
    let c = &app.colors;
    let state = if playing.paused {
        app.icons.pause
    } else {
        app.icons.play
    };
    let state = format!(" {state} ");
    let sound = if playing.audio_only {
        format!("{} ", app.icons.sound)
    } else {
        String::new()
    };
    let title = &playing.video.title;
    let channel = if playing.video.channel.is_empty() {
        String::new()
    } else {
        format!("  ·  {}", playing.video.channel)
    };
    // One column kept free at the right edge.
    let room = width.saturating_sub(state.width() + sound.width() + 1);
    let spare = room.saturating_sub(title.width() + channel.width());
    let next = match app.up_next.front() {
        Some(video) if app.settings.autoplay && spare >= MIN_NEXT => {
            fit(&format!("Next: {}", video.title), spare - 2)
        }
        _ => String::new(),
    };
    let room = room.saturating_sub(next.width());
    let title = fit(title, room);
    let channel = fit(&channel, room.saturating_sub(title.width()));
    let pad = room.saturating_sub(title.width() + channel.width());
    Line::from(vec![
        Span::styled(state, Style::new().fg(c.red).add_modifier(Modifier::BOLD)),
        Span::styled(sound, Style::new().fg(c.text)),
        Span::styled(title, Style::new().fg(c.text).add_modifier(Modifier::BOLD)),
        Span::styled(channel, Style::new().fg(c.subtext)),
        Span::raw(" ".repeat(pad)),
        Span::styled(next, Style::new().fg(c.dim)),
    ])
}

/// Where it is, a bar across with a dot where it is, its length, then the
/// keys, each with its icon (autoplay's lit while it's on). The keys go
/// first when the window is too narrow for a useful bar.
fn progress_row(app: &App, playing: &Playing, width: usize) -> Line<'static> {
    let c = &app.colors;
    let icons = &app.icons;
    let controllable = playing.controllable();
    // Under the title, past the state icon.
    let at = if controllable || playing.duration.is_some() {
        format!("   {} ", video::duration(playing.position as u32))
    } else {
        "   ".into()
    };
    let length = playing
        .duration
        .map_or(String::new(), |l| format!(" {}", video::duration(l as u32)));

    let key = Style::new().fg(c.dim);
    let mut keys: Vec<(String, Style)> = Vec::new();
    if controllable {
        let toggle = if playing.paused {
            icons.play
        } else {
            icons.pause
        };
        keys.push((format!("{} ,", icons.back), key));
        keys.push((format!("{toggle} Space"), key));
        keys.push((format!("{} .", icons.ahead), key));
    }
    if !app.up_next.is_empty() {
        keys.push((format!("{} N", icons.next), key));
    }
    keys.push((format!("{} X", icons.stop), key));
    let autoplay = if app.settings.autoplay {
        Style::new().fg(c.accent)
    } else {
        key
    };
    keys.push((format!("{} A", icons.autoplay), autoplay));
    let mut key_spans = vec![Span::raw(" ")];
    for (text, style) in keys {
        key_spans.push(Span::raw("  "));
        key_spans.push(Span::styled(text, style));
    }
    key_spans.push(Span::raw(" "));
    let keys_width: usize = key_spans.iter().map(Span::width).sum();

    let mut bar = width.saturating_sub(at.width() + length.width() + keys_width);
    if bar < 10 {
        key_spans.clear();
        bar = width.saturating_sub(at.width() + length.width() + 1);
    }
    let mut spans = vec![Span::styled(at, Style::new().fg(c.subtext))];
    match playing.duration.filter(|l| *l > 0.0) {
        Some(l) if bar > 0 => {
            let ratio = (playing.position / l).clamp(0.0, 1.0);
            let done = (ratio * (bar - 1) as f64).round() as usize;
            spans.push(Span::styled("━".repeat(done), Style::new().fg(c.red)));
            spans.push(Span::styled("●", Style::new().fg(c.red)));
            spans.push(Span::styled(
                "─".repeat(bar - 1 - done),
                Style::new().fg(c.border),
            ));
        }
        // A live stream: no length to measure against.
        _ => spans.push(Span::styled("─".repeat(bar), Style::new().fg(c.border))),
    }
    spans.push(Span::styled(length, Style::new().fg(c.subtext)));
    spans.extend(key_spans);
    Line::from(spans)
}

/// A message, or the keys for where you are; your subscription count on
/// the right.
fn status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;
    let width = area.width as usize;
    let right = format!(" {} subscriptions ", app.subscriptions.len());
    let room = width.saturating_sub(right.width());
    let left = match (&app.status, &app.prompt) {
        (_, Some(prompt)) => {
            let keys = match prompt.kind {
                PromptKind::Search => " Enter search · Esc cancel · Ctrl-u clear",
                PromptKind::Import => {
                    " Paste or drop the path to subscriptions.csv from Google Takeout · Enter import · Esc cancel"
                }
            };
            Span::styled(fit(keys, room), Style::new().fg(c.dim))
        }
        (Some(status), None) => {
            let color = if status.error { c.red } else { c.ok };
            Span::styled(
                fit(&format!(" {}", status.text), room),
                Style::new().fg(color),
            )
        }
        (None, None) => {
            let keys = match app.focus {
                Focus::Grid => {
                    " Enter play · a listen · m mix · w watch later · c channel · S subscribe · / search · ? help"
                }
                Focus::Sidebar => {
                    " ↑↓ move · Enter open · Tab videos · / search · I import · ? help"
                }
            };
            Span::styled(fit(keys, room), Style::new().fg(c.dim))
        }
    };
    let pad = room.saturating_sub(left.content.width());
    let line = Line::from(vec![
        left,
        Span::raw(" ".repeat(pad)),
        Span::styled(right, Style::new().fg(c.dim)),
    ]);
    frame.render_widget(Paragraph::new(line).style(Style::new().bg(c.bg)), area);
}

const HELP: [(&str, &str); 21] = [
    ("←↓↑→  h j k l", "move"),
    ("gg  G  Home  End", "first, last"),
    ("PgUp PgDn  Ctrl-u Ctrl-d", "a page, half a page"),
    ("Tab", "sidebar ↔ videos"),
    ("Enter", "play"),
    ("a", "listen (sound only)"),
    ("m", "YouTube's Mix of the video (music only)"),
    ("N  /  A", "next video  /  autoplay on or off"),
    ("w", "watch later (again to remove)"),
    ("x", "remove from Watch later or History"),
    ("c", "the video's channel"),
    ("S  /  u", "subscribe or unsubscribe  /  undo"),
    ("/  s", "search YouTube"),
    ("Backspace  Ctrl-o", "back"),
    ("o  /  y", "open in the browser  /  copy the link"),
    ("R", "refresh"),
    ("I", "import subscriptions from Google Takeout"),
    ("T", "next theme"),
    ("Space", "pause or play"),
    (",  .  /  <  >", "back or ahead 10 s  /  1 min;  X stops"),
    ("q", "quit"),
];

fn help(frame: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;
    let key_width = HELP.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!(" {key:<key_width$}  "), Style::new().fg(c.accent)),
                Span::styled(*what, Style::new().fg(c.text)),
            ])
        })
        .collect();
    let width = (key_width + 44).min(area.width as usize) as u16;
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(c.accent))
        .title(Span::styled(
            " Keys ",
            Style::new().fg(c.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(" any key closes ", Style::new().fg(c.dim))).right_aligned(),
        )
        .style(Style::new().bg(c.panel));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{app, with_feed};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn the_prompt_shows_the_end_of_what_was_typed() {
        assert_eq!(tail("hello world", 5), "world");
        assert_eq!(tail("日本語テキスト", 5), "スト", "two columns each");
        assert_eq!(tail("abc", 10), "abc");
        assert_eq!(tail("abc", 0), "");
    }

    #[test]
    fn text_is_cut_to_fit_with_an_ellipsis() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello world", 6), "hello…");
        assert_eq!(fit("日本語テキスト", 5), "日本…");
        assert_eq!(fit("abc", 0), "");
    }

    #[tokio::test]
    async fn an_empty_home_explains_how_to_import_subscriptions() {
        let mut app = app();
        app.status = None;
        let shown = screen(&mut app, 120, 40);
        assert!(shown.contains("tuitube"));
        assert!(shown.contains("Google Takeout"), "{shown}");
        assert!(shown.contains("Home"));
        assert!(shown.contains("Watch later"));
    }

    #[tokio::test]
    async fn the_feed_shows_as_a_grid_of_cards() {
        let mut app = app();
        app.status = None;
        with_feed(&mut app, 8);
        let shown = screen(&mut app, 140, 45);
        assert!(shown.contains("Video vid00000007"), "{shown}");
        assert!(shown.contains("BekBrace"));
        let shape = app.grid.get();
        assert!(shape.cols >= 2, "{shape:?}");
        // The selected card has a ring.
        assert!(shown.contains('╭'));
    }

    #[tokio::test]
    async fn the_next_row_shows_cut_off_at_any_window_height() {
        let mut app = app();
        with_feed(&mut app, 20);
        for height in 8..70 {
            let shown = screen(&mut app, 140, height);
            assert!(shown.contains("tuitube"), "{height}");
        }
        // 45 rows hold two rows of cards and the top of a third, whose
        // titles are cut off.
        let shown = screen(&mut app, 140, 45);
        assert_eq!(app.grid.get().rows, 2);
        assert!(shown.contains("Video vid00000014"), "the second row");
        assert!(
            !shown.contains("Video vid00000013"),
            "the third row's text is cut off"
        );
    }

    #[tokio::test]
    async fn the_header_and_search_box_line_up_with_the_thumbnails() {
        let mut app = app();
        with_feed(&mut app, 6);
        let mut terminal = Terminal::new(TestBackend::new(140, 45)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let content = app.settings.sidebar_width() + 2;
        // The first card is selected: its ring is a column left of its
        // thumbnail.
        assert_eq!(buffer[(content - 1, 3)].symbol(), "╭");
        assert_eq!(buffer[(content, 2)].symbol(), "H", "the header's Home");
        assert_eq!(buffer[(content - 1, 2)].symbol(), " ");
        // The search box's background starts there too.
        assert_eq!(buffer[(content, 0)].bg, app.colors.panel);
        assert_ne!(buffer[(content - 1, 0)].bg, app.colors.panel);
    }

    #[tokio::test]
    async fn a_narrow_window_shows_the_sidebar_or_the_grid() {
        let mut app = app();
        with_feed(&mut app, 3);
        let shown = screen(&mut app, 50, 30);
        assert!(!shown.contains("Watch later"), "the grid has the window");
        app.focus = Focus::Sidebar;
        let shown = screen(&mut app, 50, 30);
        assert!(shown.contains("Watch later"));
    }

    #[tokio::test]
    async fn the_help_lists_the_keys() {
        let mut app = app();
        app.help = true;
        let shown = screen(&mut app, 120, 40);
        assert!(shown.contains("listen (sound only)"));
        assert!(shown.contains("autoplay on or off"));
    }

    #[tokio::test]
    async fn the_player_bar_has_the_title_then_the_progress_and_keys() {
        let mut app = app();
        app.icons = crate::icons::PLAIN;
        with_feed(&mut app, 3);
        app.status = None;
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, false);
        let shown = screen(&mut app, 140, 40);
        let lines: Vec<&str> = shown.lines().collect();
        let title = lines
            .iter()
            .position(|l| l.contains("▶ Video vid00000002  ·  Channel"))
            .expect(&shown);
        let progress = lines[title + 1];
        assert!(progress.contains("0:10 ━"), "{progress}");
        assert!(progress.contains("● 1:40") || progress.contains("─ 1:40"));
        assert!(
            progress.contains("« ,  ⏸ Space  » .  ■ X  ⟳ A"),
            "{progress}"
        );
        assert!(!shown.contains("Next:"));

        app.up_next = app.videos[1..].iter().cloned().collect();
        let shown = screen(&mut app, 140, 40);
        assert!(shown.contains("Next: Video vid00000001"), "{shown}");
        assert!(shown.contains("» .  ⏭ N  ■ X"));
        app.settings.autoplay = false;
        assert!(
            !screen(&mut app, 140, 40).contains("Next:"),
            "autoplay won't play it"
        );
        for width in 1..140 {
            screen(&mut app, width, 40);
        }
        for height in 1..12 {
            screen(&mut app, 140, height);
        }
    }
}
