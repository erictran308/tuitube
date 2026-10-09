//! The sidebar: Home, Shorts, Search, Watch later, History, then your
//! subscriptions by name.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::fit;
use crate::app::{App, Entry, Focus, MENU};
use crate::icons::Icons;

fn label(entry: Entry, icons: &Icons) -> (&'static str, &'static str) {
    match entry {
        Entry::Home => (icons.home, "Home"),
        Entry::Shorts => (icons.shorts, "Shorts"),
        Entry::Search => (icons.search, "Search"),
        Entry::WatchLater => (icons.watch_later, "Watch later"),
        Entry::History => (icons.history, "History"),
        Entry::Channel(_) => ("", ""),
    }
}

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let c = app.colors.clone();
    frame.render_widget(Block::new().style(Style::new().bg(c.panel)), area);
    let width = area.width.saturating_sub(1) as usize;
    let focused = app.focus == Focus::Sidebar && app.prompt.is_none();

    // Every row, with the sidebar entry it selects, if any.
    let mut rows: Vec<(Option<usize>, Line)> = Vec::new();
    rows.push((None, Line::default()));
    for (i, entry) in MENU.iter().enumerate() {
        let (icon, name) = label(*entry, &app.icons);
        rows.push((Some(i), Line::from(format!("  {icon}  {name}"))));
    }
    rows.push((None, Line::default()));
    rows.push((
        None,
        Line::from(Span::styled(
            format!("  ─── Subscriptions ({}) ", app.subscriptions.len()),
            Style::new().fg(c.dim),
        )),
    ));
    if app.subscriptions.is_empty() {
        for hint in [
            "  None yet: press I to",
            "  import them from",
            "  Google Takeout",
        ] {
            rows.push((None, Line::from(Span::styled(hint, Style::new().fg(c.dim)))));
        }
    }
    for (i, channel) in app.subscriptions.iter().enumerate() {
        let name = if channel.title.is_empty() {
            channel.id.as_str()
        } else {
            &channel.title
        };
        rows.push((
            Some(MENU.len() + i),
            Line::from(format!("  {}", fit(name, width.saturating_sub(3)))),
        ));
    }

    // Keep the selected row on screen.
    let height = area.height as usize;
    let selected_row = rows
        .iter()
        .position(|(entry, _)| *entry == Some(app.sidebar_selected))
        .unwrap_or(0);
    let mut scroll = app.sidebar_scroll.get();
    if selected_row < scroll {
        scroll = selected_row.saturating_sub(1);
    } else if selected_row >= scroll + height {
        scroll = selected_row + 1 - height;
    }
    app.sidebar_scroll.set(scroll);

    for (y, (entry, line)) in rows.into_iter().skip(scroll).take(height).enumerate() {
        let row = Rect {
            y: area.y + y as u16,
            height: 1,
            width: area.width,
            ..area
        };
        let selected = entry == Some(app.sidebar_selected);
        let style = if selected && focused {
            Style::new()
                .bg(c.selected)
                .fg(c.accent)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::new().bg(c.selected).fg(c.text)
        } else {
            Style::new().fg(c.text)
        };
        frame.render_widget(Paragraph::new(line).style(style), row);
        if selected && focused {
            frame.render_widget(
                Paragraph::new(Span::styled("▌", Style::new().fg(c.accent))),
                Rect { width: 1, ..row },
            );
        }
    }
}
