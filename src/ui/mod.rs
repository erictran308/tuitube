//! Drawing: a top bar with the search box, the sidebar, the grid of video
//! cards, the player bar and the status line, laid out like YouTube's page.

mod grid;
mod sidebar;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, HelpMenu, HelpTab, Playing, PromptKind};
use crate::settings::Settings;
use crate::theme::{self, Colors};
use crate::{sponsorblock, video};

/// Below this width the sidebar and the grid take turns.
const NARROW: u16 = 70;

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.images.placed.clear();
    let area = frame.area();
    let c = app.colors.clone();
    frame.render_widget(Block::new().style(Style::new().bg(c.bg).fg(c.text)), area);
    // The player's block: what's playing in its border, where it is inside.
    let player_rows = 3 * u16::from(app.playing.is_some() || app.resolving.is_some());
    let [body, player, status] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(player_rows),
        Constraint::Length(1),
    ])
    .areas(area);

    // The search box is in the videos' border, so they show while it's open.
    let narrow = body.width < NARROW;
    let side_width = if narrow {
        if app.focus == Focus::Sidebar && app.prompt.is_none() {
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
    if app.help.is_some() {
        help(frame, app, area);
    }
}

/// A box with a round border all round, as tuigram's panes.
fn bordered<'a>() -> Block<'a> {
    Block::bordered().border_type(BorderType::Rounded)
}

/// The border of what has the keys is in the accent color.
fn border(focused: bool, c: &Colors) -> Style {
    Style::new().fg(if focused { c.accent } else { c.border })
}

/// Key hints with the keys in backticks, like "`Enter` play · `a` listen",
/// as tuigram writes them: the keys stand out in the accent color, and
/// what they do is dim.
fn hint_spans(hints: &str, c: &Colors) -> Vec<Span<'static>> {
    let key = Style::new().fg(c.accent).add_modifier(Modifier::BOLD);
    let text = Style::new().fg(c.dim);
    hints
        .split('`')
        .enumerate()
        .filter(|(_, part)| !part.is_empty())
        .map(|(i, part)| Span::styled(part.to_string(), if i % 2 == 1 { key } else { text }))
        .collect()
}

/// As many of `hints` (separated by " · ") as fit in `width`, whole, as
/// `hint_spans`; cut with `…` if not even the first fits.
fn hints_fitting(hints: &str, width: usize, c: &Colors) -> Vec<Span<'static>> {
    let shown = |items: &[&str]| items.join(" · ").replace('`', "").width();
    let mut items: Vec<&str> = hints.split(" · ").collect();
    while items.len() > 1 && shown(&items) > width {
        items.pop();
    }
    if shown(&items) > width {
        let plain = items.join(" · ").replace('`', "");
        return vec![Span::styled(fit(&plain, width), Style::new().fg(c.dim))];
    }
    hint_spans(&items.join(" · "), c)
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

/// Narrower than this, the search box shows only its icon and a word.
const MIN_SEARCH: usize = 12;

/// The search box, as the title of the videos' block (`width` columns
/// wide, `right` of them taken by its title on the right): a field from
/// the thumbnails' left edge, with what's typed or a hint and `/` at its
/// end. `line` is the border's style, which leads into the field.
pub(super) fn search_box(app: &App, width: u16, right: u16, line: Style) -> Line<'static> {
    let c = &app.colors;
    let lead = "─ ";
    // The corners, the lead, a space after the field and one of border
    // before the title on the right.
    let taken = 2 + lead.width() + 1 + if right > 0 { usize::from(right) + 1 } else { 0 };
    let box_width = usize::from(width).saturating_sub(taken).min(70);
    let bg = if app.prompt.is_some() {
        c.selected
    } else {
        c.panel
    };
    let inner = box_width.saturating_sub(4);
    let mut spans = match &app.prompt {
        Some(prompt) => {
            let label = match prompt.kind {
                PromptKind::Search => "",
                PromptKind::Import => "subscriptions.csv: ",
            };
            // The end of what's typed stays in view.
            let room = inner.saturating_sub(label.width() + 1);
            let shown = tail(&prompt.text, room).to_string();
            vec![
                Span::styled(format!(" {} ", app.icons.search), Style::new().fg(c.accent)),
                Span::styled(label, Style::new().fg(c.dim)),
                Span::styled(shown, Style::new().fg(c.text)),
                Span::styled("▏", Style::new().fg(c.accent)),
            ]
        }
        None => vec![
            Span::styled(format!(" {} ", app.icons.search), Style::new().fg(c.dim)),
            Span::styled(
                fit("Search", inner.saturating_sub(4)),
                Style::new().fg(c.dim),
            ),
        ],
    };
    let hint = app.prompt.is_none() && box_width > MIN_SEARCH;
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = box_width.saturating_sub(used + if hint { 3 } else { 0 });
    spans.push(Span::raw(" ".repeat(pad)));
    let mut spans: Vec<Span> = spans
        .into_iter()
        .map(|s| s.patch_style(Style::new().bg(bg)))
        .collect();
    if hint {
        spans.push(Span::styled(" / ", Style::new().fg(c.dim).bg(c.selected)));
    }
    spans.insert(0, Span::styled(lead, line));
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// What's playing, in a box: the title in its border (and on the right
/// what plays next, when there's room), and inside how far along it is and
/// the keys that control it.
fn player_bar(frame: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;
    let width = area.width.saturating_sub(2) as usize;
    let mut block = bordered().border_style(border(false, c));
    let row = if let Some(playing) = &app.playing {
        let (title, right) = title_row(app, playing, width);
        block = block.title(title).title(right.right_aligned());
        progress_row(app, playing, width)
    } else if let Some(resolving) = &app.resolving {
        Line::from(vec![
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
        ])
    } else {
        Line::default()
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(row), inner);
}

/// "Next: …" shows only with this many columns to spare beside the title.
const MIN_NEXT: usize = 24;

/// For the player's border, `width` columns inside its corners: playing or
/// paused, sound only or not, the title and channel; and for its right, a
/// SponsorBlock skip just made, in yellow, or else what autoplay plays
/// next, if the title leaves room for it.
fn title_row(app: &App, playing: &Playing, width: usize) -> (Line<'static>, Line<'static>) {
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
    // A space after the channel, before the border goes on.
    let room = width.saturating_sub(state.width() + sound.width() + 1);
    let skip = playing.shown_skip().map_or(String::new(), |skip| {
        let seconds = if skip.seconds < 60.0 {
            format!("{:.0} s", skip.seconds)
        } else {
            video::duration(skip.seconds as u32)
        };
        let what = sponsorblock::shown(skip.category);
        fit(&format!(" [SKIP] {what} · {seconds} "), room)
    });
    let room = room.saturating_sub(skip.width());
    let spare = room.saturating_sub(title.width() + channel.width());
    // At least two columns of border between the title and "Next: …".
    let next = match app.up_next.front() {
        Some(video) if skip.is_empty() && app.settings.autoplay && spare >= MIN_NEXT => {
            fit(&format!(" Next: {} ", video.title), spare - 2)
        }
        _ => String::new(),
    };
    let room = room.saturating_sub(next.width());
    let title = fit(title, room);
    let channel = fit(&channel, room.saturating_sub(title.width()));
    let left = Line::from(vec![
        Span::styled(state, Style::new().fg(c.red).add_modifier(Modifier::BOLD)),
        Span::styled(sound, Style::new().fg(c.text)),
        Span::styled(title, Style::new().fg(c.text).add_modifier(Modifier::BOLD)),
        Span::styled(channel, Style::new().fg(c.subtext)),
        Span::raw(" "),
    ]);
    let right = Line::from(vec![
        Span::styled(next, Style::new().fg(c.dim)),
        Span::styled(
            skip,
            Style::new().fg(c.warning).add_modifier(Modifier::BOLD),
        ),
    ]);
    (left, right)
}

/// Where it is, a bar across with a dot where it is, its length, then the
/// keys, each after its icon and in the accent color, as in the status bar
/// (autoplay's icon lit while it's on). The keys go first when the window
/// is too narrow for a useful bar.
fn progress_row(app: &App, playing: &Playing, width: usize) -> Line<'static> {
    let c = &app.colors;
    let icons = &app.icons;
    let controllable = playing.controllable();
    // Under the state icon in the border above.
    let at = if controllable || playing.duration.is_some() {
        format!(" {} ", video::duration(playing.position as u32))
    } else {
        " ".into()
    };
    let length = playing
        .duration
        .map_or(String::new(), |l| format!(" {}", video::duration(l as u32)));

    let dim = Style::new().fg(c.dim);
    let mut keys: Vec<(&str, &str, Style)> = Vec::new();
    if controllable {
        let toggle = if playing.paused {
            icons.play
        } else {
            icons.pause
        };
        keys.push((icons.back, ",", dim));
        keys.push((toggle, "Space", dim));
        keys.push((icons.ahead, ".", dim));
    }
    if !app.up_next.is_empty() {
        keys.push((icons.next, "N", dim));
    }
    keys.push((icons.stop, "X", dim));
    let autoplay = if app.settings.autoplay {
        Style::new().fg(c.accent)
    } else {
        dim
    };
    keys.push((icons.autoplay, "A", autoplay));
    let key = Style::new().fg(c.accent).add_modifier(Modifier::BOLD);
    let mut key_spans = vec![Span::raw(" ")];
    for (icon, name, style) in keys {
        key_spans.push(Span::raw("  "));
        key_spans.push(Span::styled(format!("{icon} "), style));
        key_spans.push(Span::styled(name, key));
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
                PromptKind::Search => " `Enter` search · `Esc` cancel · `Ctrl-u` clear",
                PromptKind::Import => {
                    " Paste or drop the path to subscriptions.csv from Google Takeout · `Enter` import · `Esc` cancel"
                }
            };
            hints_fitting(keys, room, c)
        }
        (Some(status), None) => {
            let color = if status.error { c.red } else { c.ok };
            vec![Span::styled(
                fit(&format!(" {}", status.text), room),
                Style::new().fg(color),
            )]
        }
        (None, None) => {
            let keys = match app.focus {
                Focus::Grid => {
                    " `Enter` play · `a` listen · `m` mix · `w` watch later · `c` channel · `S` subscribe · `/` search · `?` help"
                }
                Focus::Sidebar => {
                    " `↑↓` move · `Enter` open · `Tab` videos · `/` search · `I` import · `?` help"
                }
            };
            hints_fitting(keys, room, c)
        }
    };
    let used: usize = left.iter().map(Span::width).sum();
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(room.saturating_sub(used))));
    spans.push(Span::styled(right, Style::new().fg(c.dim)));
    let line = Line::from(spans);
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
    ("Space", "pause or play"),
    (",  .  /  <  >", "back or ahead 10 s  /  1 min;  X stops"),
    ("?  /  O", "these keys  /  settings: theme, Shorts…"),
    ("q", "quit"),
];

/// The `?` popup, centered: a tab with every key, and one with the
/// settings. Both are as big as the bigger one, so the tabs don't move.
fn help(frame: &mut Frame, app: &mut App, area: Rect) {
    let c = app.colors.clone();
    let Some(menu) = app.help.as_mut() else {
        return;
    };
    let keys = key_lines(&c);
    let (settings, cursor) = settings_lines(&app.settings, menu.selected, &c);
    let widest = |lines: &[Line]| lines.iter().map(Line::width).max().unwrap_or(0);
    let width = (widest(&keys).max(widest(&settings)) + 3).min(area.width as usize) as u16;
    let height = (keys.len().max(settings.len()) as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let tab = |label: &'static str, active: bool| {
        if active {
            Span::styled(
                label,
                Style::new()
                    .fg(c.bg)
                    .bg(c.accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(label, Style::new().fg(c.dim))
        }
    };
    let tabs = Line::from(vec![
        Span::raw(" "),
        tab(" Keys ", menu.tab == HelpTab::Keys),
        Span::raw(" "),
        tab(" Settings ", menu.tab == HelpTab::Settings),
        Span::raw(" "),
    ]);
    let hint = match menu.tab {
        HelpTab::Keys => " `j k` scroll · `Tab` settings · `Esc` close ",
        HelpTab::Settings if menu.selected >= HelpMenu::THEMES => {
            " `Enter` use · `Tab` keys · `Esc` close "
        }
        HelpTab::Settings => " `Enter` on/off · `Tab` keys · `Esc` close ",
    };
    frame.render_widget(Clear, popup);
    let block = bordered()
        .border_style(border(true, &c))
        .title(tabs)
        .title_bottom(Line::from(hint_spans(hint, &c)).right_aligned())
        .style(Style::new().bg(c.panel));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = usize::from(inner.height);
    let (lines, scroll) = match menu.tab {
        HelpTab::Keys => (keys, &mut menu.scroll),
        HelpTab::Settings => {
            // Scrolled as little as keeps the cursor in view, with the line
            // above it: the heading, for the first row of a group. On the
            // last theme, the end shows.
            let scroll = &mut menu.settings_scroll;
            *scroll = (*scroll).min(cursor.saturating_sub(1));
            let cursor_end = if menu.selected == HelpMenu::LAST {
                settings.len() - 1
            } else {
                cursor
            };
            if cursor_end >= *scroll + rows {
                *scroll = cursor_end + 1 - rows;
            }
            (settings, scroll)
        }
    };
    let max = lines.len().saturating_sub(rows);
    *scroll = (*scroll).min(max);
    frame.render_widget(Paragraph::new(lines).scroll((*scroll as u16, 0)), inner);
    if max > 0 {
        let mut state = ScrollbarState::new(max).position(*scroll);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .style(Style::new().fg(c.dim)),
            popup.inner(Margin::new(0, 1)),
            &mut state,
        );
    }
}

/// The keys tab: every key, and what it does.
fn key_lines(c: &Colors) -> Vec<Line<'static>> {
    let key_width = HELP.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    HELP.iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!(" {key:<key_width$}  "), Style::new().fg(c.accent)),
                Span::styled(*what, Style::new().fg(c.text)),
            ])
        })
        .collect()
}

/// The settings tab, and the line of the row `selected`.
fn settings_lines(settings: &Settings, selected: usize, c: &Colors) -> (Vec<Line<'static>>, usize) {
    let mut lines = Vec::new();
    let mut cursor = 0;
    let mut add = |lines: &mut Vec<Line<'static>>, row: usize, mark: &'static str, label: &str| {
        let chosen = row == selected;
        let bar = if chosen {
            Span::styled("▌", Style::new().fg(c.accent))
        } else {
            Span::raw(" ")
        };
        let line = Line::from(vec![
            bar,
            Span::styled(mark, Style::new().fg(c.accent)),
            Span::styled(label.to_string(), Style::new().fg(c.text)),
        ]);
        if chosen {
            cursor = lines.len();
            lines.push(line.style(Style::new().bg(c.selected)));
        } else {
            lines.push(line);
        }
    };
    let heading = |text: &'static str| {
        Line::from(Span::styled(
            text,
            Style::new().fg(c.dim).add_modifier(Modifier::BOLD),
        ))
    };
    let check = |on: bool| if on { " [✓] " } else { " [ ] " };

    lines.push(heading(" Videos"));
    add(
        &mut lines,
        HelpMenu::SHORTS,
        check(settings.shorts),
        "Show Shorts",
    );
    add(
        &mut lines,
        HelpMenu::LIVE,
        check(settings.live),
        "Show live streams and premieres",
    );
    add(
        &mut lines,
        HelpMenu::DESCRIPTIONS,
        check(settings.descriptions),
        "Show the start of each description",
    );
    lines.push(Line::default());
    lines.push(heading(" Playing"));
    add(
        &mut lines,
        HelpMenu::AUTOPLAY,
        check(settings.autoplay),
        "Autoplay: the next video when one ends",
    );
    add(
        &mut lines,
        HelpMenu::SPONSORBLOCK,
        check(settings.sponsorblock),
        "Skip sponsors with SponsorBlock (asks sponsor.ajay.app)",
    );
    lines.push(Line::default());
    lines.push(heading(" Privacy"));
    add(
        &mut lines,
        HelpMenu::HISTORY,
        check(settings.history),
        "Keep a history of what you watch, here only",
    );
    lines.push(Line::default());
    lines.push(heading(" Theme"));
    for (i, (id, name)) in theme::names().iter().enumerate() {
        // The dot marks the theme in use.
        let mark = if *id == settings.theme {
            " ● "
        } else {
            " ○ "
        };
        add(&mut lines, HelpMenu::THEMES + i, mark, name);
    }
    (lines, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Prompt;
    use crate::app::tests::{app, sponsor_at, with_feed};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn drawn(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let buffer = drawn(app, width, height);
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
        let buffer = drawn(&mut app, 140, 45);
        let side = app.settings.sidebar_width();
        // The videos' box starts where the sidebar's ends; its border, a
        // column in, then the cards.
        assert_eq!(buffer[(side, 0)].symbol(), "╭");
        let content = side + 3;
        // The first card is selected: its ring is a column left of its
        // thumbnail.
        assert_eq!(buffer[(content - 1, 2)].symbol(), "╭");
        assert_eq!(buffer[(content, 1)].symbol(), "H", "the header's Home");
        assert_eq!(buffer[(content - 1, 1)].symbol(), " ");
        // The search box's background starts there too, in the border.
        assert_eq!(buffer[(content, 0)].bg, app.colors.panel);
        assert_ne!(buffer[(content - 1, 0)].bg, app.colors.panel);
        assert_eq!(buffer[(side + 1, 0)].symbol(), "─");
    }

    #[tokio::test]
    async fn the_sidebar_and_the_videos_are_boxes_lit_where_the_keys_go() {
        let mut app = app();
        app.status = None;
        with_feed(&mut app, 6);
        let shown = screen(&mut app, 140, 45);
        let lines: Vec<&str> = shown.lines().collect();
        assert!(lines[0].starts_with("╭  ▶  tuitube ─"), "{shown}");
        assert!(lines[1].starts_with("│"), "Home under the logo: {shown}");
        assert!(lines[1].contains("Home"));
        let rule = lines
            .iter()
            .find(|l| l.contains("Subscriptions ("))
            .expect(&shown);
        let side = app.settings.sidebar_width() as usize;
        let rule: String = rule.chars().take(side).collect();
        let heading = format!("├─ Subscriptions ({}) ─", app.subscriptions.len());
        assert!(rule.starts_with(&heading), "{rule}");
        assert!(rule.ends_with("─┤"), "{rule}");

        let side = side as u16;
        let buffer = drawn(&mut app, 140, 45);
        assert_eq!(app.focus, Focus::Grid);
        assert_eq!(buffer[(side, 10)].fg, app.colors.accent, "the videos'");
        assert_eq!(buffer[(0, 10)].fg, app.colors.border);
        app.focus = Focus::Sidebar;
        let buffer = drawn(&mut app, 140, 45);
        assert_eq!(buffer[(side, 10)].fg, app.colors.border);
        assert_eq!(buffer[(0, 10)].fg, app.colors.accent, "the sidebar's");
        // Typing in the search box, in the videos' border, lights it.
        app.prompt = Some(Prompt {
            kind: PromptKind::Search,
            text: "rust".into(),
        });
        let buffer = drawn(&mut app, 140, 45);
        assert_eq!(buffer[(side, 10)].fg, app.colors.accent);
        assert_eq!(buffer[(0, 10)].fg, app.colors.border);
        assert!(
            screen(&mut app, 140, 45)
                .lines()
                .next()
                .unwrap()
                .contains("rust▏")
        );
    }

    #[tokio::test]
    async fn a_narrow_window_shows_the_search_box_while_it_is_open() {
        let mut app = app();
        with_feed(&mut app, 3);
        app.focus = Focus::Sidebar;
        assert!(screen(&mut app, 50, 30).contains("Watch later"));
        app.prompt = Some(Prompt {
            kind: PromptKind::Search,
            text: "lofi".into(),
        });
        let shown = screen(&mut app, 50, 30);
        assert!(!shown.contains("Watch later"), "the videos have the window");
        assert!(shown.contains("lofi▏"), "{shown}");
    }

    #[test]
    fn key_hints_show_their_keys_in_the_accent_color_and_fit_whole() {
        let c = Colors::named(theme::DEFAULT);
        let spans = hint_spans(" `Enter` play · `a` listen", &c);
        let texts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, [" ", "Enter", " play · ", "a", " listen"]);
        assert_eq!(spans[1].style.fg, Some(c.accent));
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].style.fg, Some(c.dim));

        let hints = " `Enter` play · `a` listen · `m` mix";
        let text = |spans: Vec<Span>| {
            spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert_eq!(
            text(hints_fitting(hints, 40, &c)),
            " Enter play · a listen · m mix"
        );
        assert_eq!(text(hints_fitting(hints, 22, &c)), " Enter play · a listen");
        assert_eq!(text(hints_fitting(hints, 21, &c)), " Enter play");
        assert_eq!(text(hints_fitting(hints, 6, &c)), " Ente…");
    }

    #[tokio::test]
    async fn the_status_bar_shows_its_keys_in_the_accent_color() {
        let mut app = app();
        app.status = None;
        with_feed(&mut app, 3);
        let buffer = drawn(&mut app, 140, 40);
        let shown = screen(&mut app, 140, 40);
        let status = shown.lines().last().unwrap();
        assert!(status.starts_with(" Enter play · a listen"), "{status}");
        let enter = buffer[(1, 39)].clone();
        assert_eq!(enter.fg, app.colors.accent);
        assert!(enter.modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(7, 39)].fg, app.colors.dim, "play");
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
    async fn the_help_lists_the_keys_then_the_settings() {
        let mut app = app();
        app.help = Some(HelpMenu::new(HelpTab::Keys));
        let shown = screen(&mut app, 120, 40);
        assert!(shown.contains("listen (sound only)"));
        assert!(shown.contains("autoplay on or off"));
        assert!(shown.contains(" Keys ") && shown.contains(" Settings "));
        assert!(!shown.contains("Show Shorts"));

        app.help = Some(HelpMenu::new(HelpTab::Settings));
        app.settings.sponsorblock = true;
        app.settings.live = false;
        let shown = screen(&mut app, 120, 40);
        assert!(!shown.contains("listen (sound only)"));
        let line = |text: &str| {
            shown
                .lines()
                .find(|l| l.contains(text))
                .unwrap_or_else(|| panic!("{text}: {shown}"))
                .to_string()
        };
        assert!(line("Show Shorts").contains("▌ [✓]"), "selected, and on");
        assert!(line("Show live streams").contains("[ ]"));
        assert!(line("SponsorBlock").contains("[✓]"));
        assert!(line("Catppuccin Mocha").contains("●"), "the theme in use");
        assert!(line("Catppuccin Latte").contains("○"));
        assert!(shown.contains("Enter on/off"));

        // In a short window, the cursor's row stays on screen.
        app.help.as_mut().unwrap().selected = HelpMenu::LAST;
        let shown = screen(&mut app, 80, 12);
        assert!(
            shown.contains("Rosé Pine") || shown.contains("Rose Pine"),
            "{shown}"
        );
        assert!(!shown.contains("Show Shorts"));
        assert!(shown.contains("Enter use"));
    }

    #[tokio::test]
    async fn the_player_bar_is_a_box_with_the_title_in_its_border() {
        let mut app = app();
        app.icons = crate::icons::PLAIN;
        with_feed(&mut app, 3);
        app.status = None;
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, false);
        let shown = screen(&mut app, 140, 40);
        let lines: Vec<&str> = shown.lines().collect();
        let title = lines
            .iter()
            .position(|l| l.contains("╭ ▶ Video vid00000002  ·  Channel"))
            .expect(&shown);
        let progress = lines[title + 1];
        assert!(progress.starts_with("│ 0:10 ━"), "{progress}");
        assert!(progress.ends_with("│"), "{progress}");
        assert!(lines[title + 2].starts_with("╰─"));
        assert!(progress.contains("0:10 ━"), "{progress}");
        assert!(progress.contains("● 1:40") || progress.contains("─ 1:40"));
        // Windows can't pause or seek mpv yet: only X and A there.
        let keys = if cfg!(unix) {
            "« ,  ⏸ Space  » .  ■ X  ⟳ A"
        } else {
            "  ■ X  ⟳ A"
        };
        assert!(progress.contains(keys), "{progress}");
        assert!(!shown.contains("Next:"));

        app.up_next = app.videos[1..].iter().cloned().collect();
        let shown = screen(&mut app, 140, 40);
        let title_line = shown
            .lines()
            .find(|l| l.contains("▶ Video vid00000002"))
            .unwrap();
        assert!(
            title_line.ends_with("─ Next: Video vid00000001 ╮"),
            "{title_line}"
        );
        assert!(shown.contains("  ⏭ N  ■ X"), "{shown}");
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

    #[tokio::test]
    async fn a_sponsorblock_skip_shows_in_yellow_where_next_goes() {
        let mut app = app();
        app.icons = crate::icons::PLAIN;
        with_feed(&mut app, 3);
        app.settings.sponsorblock = true;
        app.up_next = app.videos[1..].iter().cloned().collect();
        app.show_playing(app.videos[0].clone(), 0.0, 600.0, false);
        sponsor_at(&mut app, 10.0, 75.0, 10.0);
        let buffer = drawn(&mut app, 140, 40);
        let shown = screen(&mut app, 140, 40);
        let (y, line) = shown
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("▶ Video vid00000002"))
            .expect(&shown);
        assert!(line.contains("[SKIP] Sponsor · 1:05"), "{line}");
        assert!(!line.contains("Next:"), "{line}");
        let x = line.chars().take_while(|&c| c != '[').count() as u16;
        assert_eq!(buffer[(x, y as u16)].fg, app.colors.warning, "yellow");

        sponsor_at(&mut app, 100.0, 120.0, 100.0);
        assert!(screen(&mut app, 140, 40).contains("[SKIP] Sponsor · 20 s"));
        for width in 1..140 {
            screen(&mut app, width, 40);
        }
    }
}
