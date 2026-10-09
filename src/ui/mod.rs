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

use crate::app::{App, Focus, PromptKind};
use crate::video;

/// Below this width the sidebar and the grid take turns.
const NARROW: u16 = 70;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let c = app.colors.clone();
    frame.render_widget(Block::new().style(Style::new().bg(c.bg).fg(c.text)), area);
    let player_rows = u16::from(app.playing.is_some() || app.resolving.is_some());
    let [top, body, player, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(player_rows),
        Constraint::Length(1),
    ])
    .areas(area);
    top_bar(frame, app, top);

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

/// The logo, the search box in the middle, and how the feeds' update goes.
fn top_bar(frame: &mut Frame, app: &App, area: Rect) {
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

    let box_width = (area.width / 2)
        .clamp(20, 70)
        .min(area.width.saturating_sub(12));
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_width)) / 2,
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
            let shown: String = {
                let text = &prompt.text;
                let mut start = 0;
                while text[start..].width() > room {
                    start += text[start..].chars().next().map_or(1, char::len_utf8);
                }
                text[start..].to_string()
            };
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

/// What's playing, how far along, and the keys that control it.
fn player_bar(frame: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;
    frame.render_widget(Block::new().style(Style::new().bg(c.panel)), area);
    let width = area.width as usize;
    let line = if let Some(playing) = &app.playing {
        let icon = format!(
            " {} ",
            if playing.paused {
                app.icons.pause
            } else {
                app.icons.play
            }
        );
        let time = match playing.duration {
            Some(length) => format!(
                " {} / {} ",
                video::duration(playing.position as u32),
                video::duration(length as u32)
            ),
            None if playing.controllable() => {
                format!(" {} ", video::duration(playing.position as u32))
            }
            None => String::new(),
        };
        let keys = if playing.controllable() {
            "  Space pause  , . seek  X stop "
        } else {
            "  X stop "
        };
        let bar_width = (width / 5).clamp(0, 30);
        let filled = playing.duration.filter(|l| *l > 0.0).map_or(0, |l| {
            ((playing.position / l).clamp(0.0, 1.0) * bar_width as f64) as usize
        });
        let fixed = icon.width() + time.width() + bar_width + keys.width() + 2;
        let title = fit(
            &format!("{} · {}", playing.video.title, playing.video.channel),
            width.saturating_sub(fixed),
        );
        let pad = width.saturating_sub(fixed + title.width() - 2);
        let mode = if playing.audio_only { "♪" } else { "" };
        Line::from(vec![
            Span::styled(icon, Style::new().fg(c.red).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{mode}{title}"), Style::new().fg(c.text)),
            Span::raw(" ".repeat(pad.saturating_sub(mode.width()))),
            Span::styled(time, Style::new().fg(c.subtext)),
            Span::styled("━".repeat(filled), Style::new().fg(c.red)),
            Span::styled("─".repeat(bar_width - filled), Style::new().fg(c.border)),
            Span::styled(keys, Style::new().fg(c.dim)),
        ])
    } else if let Some((_, video)) = &app.resolving {
        Line::from(vec![
            Span::styled(
                format!(" {} ", app.icons.loading),
                Style::new().fg(c.accent),
            ),
            Span::styled(
                fit(
                    &format!("Loading “{}”…", video.title),
                    width.saturating_sub(4),
                ),
                Style::new().fg(c.subtext),
            ),
        ])
    } else {
        Line::default()
    };
    frame.render_widget(Paragraph::new(line), area);
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
                    " Enter play · a listen · w watch later · c channel · S subscribe · / search · ? help"
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

const HELP: [(&str, &str); 19] = [
    ("←↓↑→  h j k l", "move"),
    ("gg  G  Home  End", "first, last"),
    ("PgUp PgDn  Ctrl-u Ctrl-d", "a page, half a page"),
    ("Tab", "sidebar ↔ videos"),
    ("Enter", "play"),
    ("a", "listen (sound only)"),
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
    }
}
