//! Drawing: a top bar with the search box, the sidebar, the grid of video
//! cards, the player bar and the status line, laid out like YouTube's page.

mod grid;
mod list;
mod sidebar;
mod visualizer;
mod watch;

use std::num::NonZeroU16;

use ratatui::Frame;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{
    App, Comments, Confirm, Focus, HelpMenu, HelpTab, InfoTab, JukeboxState, Kept, OutputWatch,
    Playing, PromptKind, View,
};
use crate::icons::Icons;
use crate::settings::{self, Repeat, Settings};
use crate::theme::{self, Colors};
use crate::ytdlp::AudioQuality;
use crate::{chapters, sponsorblock, video};

/// Below this width the sidebar and the grid take turns.
const NARROW: u16 = 70;

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.images.placed.clear();
    // Laid out again below, where the picture playing in Pepper shows.
    app.video_box.set(None);
    let area = frame.area();
    let c = app.colors.clone();
    frame.render_widget(Block::new().style(Style::new().bg(c.bg).fg(c.text)), area);
    // The player's block: what's playing in its border, where it is inside,
    // and above that the visualizer while only the sound plays.
    let bars = if app.visualizing() {
        visualizer_rows(area.height)
    } else {
        0
    };
    let player_rows = (3 + bars) * u16::from(app.playing.is_some() || app.resolving.is_some());
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
    // Full screen ends once nothing plays or loads.
    if app.playing.is_none() && app.resolving.is_none() {
        app.fullscreen = false;
    }
    if app.fills_tab() {
        // Full screen in Pepper (`f`): the picture over the whole tab, on
        // black where its shape leaves room. Popups still open over it,
        // taking it away while they're open.
        frame.render_widget(Block::new().style(Style::new().bg(Color::Black)), area);
        app.video_box.set(Some(area));
    } else {
        if app.watch.is_some() {
            watch::draw(frame, app, body);
        } else {
            if side.width > 0 {
                sidebar::draw(frame, app, side);
            }
            if grid.width > 0 {
                grid::draw(frame, app, grid);
                picture_in_picture(frame, app, grid);
            }
        }
        if player_rows > 0 {
            player_bar(frame, app, player);
        }
        status_bar(frame, app, status);
    }
    if app.help.is_some() {
        help(frame, app, area);
    }
    if app.info_popup.is_some() {
        info_popup(frame, app, area);
    }
    if app.chapter_menu.is_some() {
        chapter_menu(frame, app, area);
    }
    if app.group_menu.is_some() {
        group_menu(frame, app, area);
    }
    if app.command.is_some() {
        command_menu(frame, app, area, status);
    }
    if app.confirm.is_some() {
        confirm(frame, app, area);
    }
    pin_emoji_widths(frame.buffer_mut());
}

/// While browsing, the video playing inside Pepper shows in a box at the
/// bottom right of the videos (`P` hides it), where Pepper puts its
/// picture.
fn picture_in_picture(frame: &mut Frame, app: &mut App, area: Rect) {
    if !app.plays_inline() || app.pip_hidden {
        return;
    }
    let font = app.images.font_size();
    if let Some(place) = crate::pepper::pip_area(area, (font.width, font.height)) {
        frame.render_widget(Clear, place);
        frame.render_widget(Block::new().style(Style::new().bg(Color::Black)), place);
        app.video_box.set(Some(place));
    }
}

/// Marks each emoji with U+FE0F (⚔️, ❤️) as two columns wide when it's
/// sent to the terminal. Otherwise ratatui also sends the column after it,
/// without moving the cursor there first: in a terminal that drew the emoji
/// two columns wide (Ghostty, kitty, WezTerm), that column lands one to the
/// right, and the rest of the row after it.
fn pin_emoji_widths(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        if cell.diff_option == CellDiffOption::None
            && cell.symbol().contains('\u{FE0F}')
            && let Some(width) = NonZeroU16::new(cell.symbol().width() as u16)
            && width.get() > 1
        {
            cell.diff_option = CellDiffOption::ForcedWidth(width);
        }
    }
}

/// A video's mark while it plays from the Jukebox, in its color.
pub(super) fn jukebox_mark(
    state: JukeboxState,
    icons: &Icons,
    c: &Colors,
) -> (&'static str, &'static str, Style) {
    let red = Style::new().fg(c.red).add_modifier(Modifier::BOLD);
    match state {
        JukeboxState::Playing => (icons.play, "Playing", red),
        JukeboxState::Paused => (icons.pause, "Paused", red),
        JukeboxState::Loading => (icons.loading, "Loading", Style::new().fg(c.accent)),
    }
}

/// The mark of a video that couldn't be played, as `jukebox_mark`.
pub(super) fn failed_mark(icons: &Icons, c: &Colors) -> (&'static str, &'static str, Style) {
    let red = Style::new().fg(c.red).add_modifier(Modifier::BOLD);
    (icons.failed, "Can't play", red)
}

/// How wide the question's text goes before it takes another line.
const MAX_CONFIRM: usize = 60;

/// The question waiting for `y`, in a popup in the middle: what it does,
/// and the keys.
fn confirm(frame: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;
    let Some(confirm) = &app.confirm else {
        return;
    };
    let (title, text, hint) = match confirm {
        Confirm::ClearJukebox => {
            let n = app.jukebox.len();
            let videos = if n == 1 { "video" } else { "videos" };
            (
                " Clear the Jukebox? ".to_string(),
                format!("All {n} {videos} in it are taken off. This can't be undone."),
                " `y` clear · any other key keeps them ",
            )
        }
        Confirm::DeletePlaylist(id) => {
            let name = app
                .my_playlists
                .iter()
                .find(|p| p.id == *id)
                .map_or("", |p| p.name.as_str());
            (
                " Delete this playlist? ".to_string(),
                format!("“{name}” is deleted, not its videos. This can't be undone."),
                " `y` delete · any other key keeps it ",
            )
        }
        Confirm::DeleteGroup(id) => {
            let name = app
                .groups
                .iter()
                .find(|g| g.id == *id)
                .map_or("", |g| g.name.as_str());
            (
                " Delete this group? ".to_string(),
                format!("“{name}” is deleted, not its channels: they stay your subscriptions."),
                " `y` delete · any other key keeps it ",
            )
        }
        Confirm::TakeOff(list, _, video) => {
            let from = match list {
                Kept::WatchLater => "Watch later".to_string(),
                Kept::Jukebox => "the Jukebox".to_string(),
                Kept::History => "History".to_string(),
                Kept::Playlist(_, name) => format!("“{}”", fit(name, MAX_NAME_SHOWN)),
            };
            (
                format!(" Remove from {from}? "),
                format!("“{video}”"),
                " `y` remove · any other key keeps it ",
            )
        }
        Confirm::Unsubscribe(_, name) => (
            " Unsubscribe? ".to_string(),
            format!("From “{name}”: its videos leave Home. u subscribes again."),
            " `y` unsubscribe · any other key keeps it ",
        ),
    };
    let hint_width = hint.replace('`', "").width();
    let fits = (MAX_CONFIRM.max(hint_width).max(title.width()) + 6).min(usize::from(area.width));
    // A long video title goes on more lines, not across the screen.
    let lines = wrap(&text, fits.saturating_sub(4).max(1));
    let widest = lines.iter().map(|l| l.width()).max().unwrap_or(0);
    let width = (widest.max(hint_width).max(title.width()) + 6).min(usize::from(area.width));
    let height = (lines.len() as u16 + 4).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width as u16) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width as u16,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = bordered()
        .border_style(border(true, c))
        .title(Span::styled(
            title,
            Style::new().fg(c.text).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(hint_spans(hint, c)).right_aligned())
        .style(Style::new().bg(c.panel));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let rows = std::iter::once(Line::default()).chain(lines.iter().map(|line| {
        Line::from(Span::styled(
            fit(&format!(" {line}"), usize::from(inner.width)),
            Style::new().fg(c.text),
        ))
    }));
    frame.render_widget(Paragraph::new(rows.collect::<Vec<_>>()), inner);
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

/// `text` cut to `width` columns, with `…` if it was longer. Measured, as
/// everywhere, a character as the terminal draws it (a grapheme, as ratatui
/// does): ⚔️ is two columns, though ⚔ alone is one, and 👍🏽 two, not four.
/// None is ever cut in two.
pub fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let w = grapheme.width();
        if used + w > width - 1 {
            break;
        }
        used += w;
        out.push_str(grapheme);
    }
    out.push('…');
    out
}

/// The end of `text` that fits in `width` columns, found in one pass from
/// the end.
fn tail(text: &str, width: usize) -> &str {
    let mut used = 0;
    let mut start = text.len();
    for (i, grapheme) in text.grapheme_indices(true).rev() {
        let w = grapheme.width();
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
        // What was left typed in it, else what it's for.
        None => {
            let typed = app.search_typed.as_deref().filter(|t| !t.is_empty());
            let room = inner.saturating_sub(4);
            let shown = typed.map_or_else(|| fit("Search", room), |t| tail(t, room).to_string());
            vec![
                Span::styled(format!(" {} ", app.icons.search), Style::new().fg(c.dim)),
                Span::styled(shown, Style::new().fg(c.dim)),
            ]
        }
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

/// The visualizer's rows in the player bar, in a window `height` rows high:
/// fewer in a short one, none where the videos can't spare any.
fn visualizer_rows(height: u16) -> u16 {
    match height {
        0..22 => 0,
        22..28 => 2,
        28..36 => 3,
        _ => 4,
    }
}

/// What's playing, in a box: the title in its border (and on the right
/// what plays next, when there's room), and inside how far along it is and
/// the keys that control it, under the visualizer's bars while they show.
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
                    &format!("Loading “{}”…", app.title(&resolving.video)),
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
    let [bars, inner] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
    if let Some(playing) = app.playing.as_ref().filter(|_| app.visualizing()) {
        // In from the border as the time below it.
        visualizer::draw(
            frame.buffer_mut(),
            bars.inner(Margin::new(1, 0)),
            &playing.spectrum,
            app.settings.visualizer_white.then_some(c.text),
        );
    }
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
    let title = app.title(&playing.video);
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
    let next = match app.plays_next() {
        Some(video) if skip.is_empty() && spare >= MIN_NEXT => {
            fit(&format!(" Next: {} ", app.title(video)), spare - 2)
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

/// The bar is at least this wide beside the chapter's title, or the title
/// isn't shown.
const MIN_BAR: usize = 20;
/// The most columns the chapter's title takes in the player bar.
const MAX_CHAPTER_SHOWN: usize = 32;

/// Where it is and the chapter it's in, a bar across with a dot where it
/// is, its length and quality, then the keys, each after its icon and in
/// the accent color, as in the status bar (autoplay's and repeat's icons
/// lit while they're on). The keys and the quality go first when the window is too narrow
/// for a useful bar, then the chapter.
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
        keys.push((icons.chapters, "t", dim));
    }
    // Lit while they show; only for a video that has them.
    if playing.subtitles.is_some() {
        let style = if app.settings.subtitles {
            Style::new().fg(c.accent)
        } else {
            dim
        };
        keys.push((icons.subtitles, "T", style));
    }
    if app.next_video().is_some() {
        keys.push((icons.next, "N", dim));
    }
    keys.push((icons.stop, "X", dim));
    let autoplay = if app.settings.autoplay {
        Style::new().fg(c.accent)
    } else {
        dim
    };
    keys.push((icons.autoplay, "A", autoplay));
    let (repeat, style) = match app.settings.repeat {
        Repeat::Off => (icons.repeat, dim),
        Repeat::All => (icons.repeat, Style::new().fg(c.accent)),
        Repeat::One => (icons.repeat_one, Style::new().fg(c.accent)),
    };
    keys.push((repeat, "r", style));
    // Lit while on; only where it changes what plays next.
    if app.shuffle_applies() {
        let style = if app.settings.shuffle {
            Style::new().fg(c.accent)
        } else {
            dim
        };
        keys.push((icons.shuffle, "z", style));
    }
    let key = Style::new().fg(c.accent).add_modifier(Modifier::BOLD);
    let mut key_spans = vec![Span::raw(" ")];
    if let Some(quality) = &playing.quality {
        key_spans.push(Span::styled(format!(" {quality}"), dim));
    }
    for (icon, name, style) in keys {
        key_spans.push(Span::raw("  "));
        key_spans.push(Span::styled(format!("{icon} "), style));
        key_spans.push(Span::styled(name, key));
    }
    // How loud, and its keys; muted in yellow.
    if controllable {
        let (icon, style) = if app.muted {
            (icons.muted, Style::new().fg(c.warning))
        } else {
            (icons.volume, dim)
        };
        key_spans.push(Span::raw("  "));
        key_spans.push(Span::styled(
            format!("{icon} {}% ", app.settings.volume()),
            style,
        ));
        key_spans.push(Span::styled("-+", key));
    }
    key_spans.push(Span::raw(" "));
    let keys_width: usize = key_spans.iter().map(Span::width).sum();

    let mut bar = width.saturating_sub(at.width() + length.width() + keys_width);
    if bar < 10 {
        key_spans.clear();
        bar = width.saturating_sub(at.width() + length.width() + 1);
    }
    let chapter = chapters::current(&playing.chapters, playing.position)
        .map(|i| playing.chapters[i].title.as_str())
        .filter(|title| !title.is_empty());
    let room = MAX_CHAPTER_SHOWN.min(bar.saturating_sub(MIN_BAR));
    let chapter = match chapter {
        Some(title) if room >= 8 => format!("· {} ", fit(title, room - 3)),
        _ => String::new(),
    };
    bar -= chapter.width();
    let mut spans = vec![
        Span::styled(at, Style::new().fg(c.subtext)),
        Span::styled(chapter, Style::new().fg(c.text)),
    ];
    match playing.duration.filter(|l| *l > 0.0) {
        Some(l) if bar > 0 => {
            let ratio = (playing.position / l).clamp(0.0, 1.0);
            let segments: Vec<(f64, f64, Color)> = playing
                .segments()
                .map(|s| (s.start / l, s.end / l, c.segment(s.category)))
                .collect();
            spans.extend(seek_bar(c, ratio, &segments, bar));
        }
        // A live stream: no length to measure against.
        _ => spans.push(Span::styled("─".repeat(bar), Style::new().fg(c.border))),
    }
    spans.push(Span::styled(length, Style::new().fg(c.subtext)));
    spans.extend(key_spans);
    Line::from(spans)
}

/// The seek bar, `width` cells: red as far as it played, the dot, then the
/// rest, with SponsorBlock's segments (from and to, as parts of the length)
/// over it in their colors, a cell at least each, so what will be skipped
/// shows before it is.
fn seek_bar(
    c: &Colors,
    ratio: f64,
    segments: &[(f64, f64, Color)],
    width: usize,
) -> Vec<Span<'static>> {
    let last = width.saturating_sub(1);
    let cell = |part: f64| (part.clamp(0.0, 1.0) * last as f64).round() as usize;
    let done = cell(ratio);
    let mut marks: Vec<Option<Color>> = vec![None; width];
    for &(from, to, color) in segments {
        let (from, to) = (cell(from), cell(to));
        for mark in marks.iter_mut().take(to + 1).skip(from) {
            *mark = Some(color);
        }
    }
    // Cells of one color go in one span.
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, mark) in marks.into_iter().enumerate() {
        let (glyph, color) = match mark {
            _ if i == done => ("●", c.red),
            Some(color) => ("━", color),
            None if i < done => ("━", c.red),
            None => ("─", c.border),
        };
        match spans.last_mut() {
            Some(span) if span.style.fg == Some(color) => span.content.to_mut().push_str(glyph),
            _ => spans.push(Span::styled(glyph, Style::new().fg(color))),
        }
    }
    spans
}

/// The command line, as vim's: `:` and what's typed, then why it didn't
/// run, or its keys, on the right.
fn command_line(frame: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;
    let Some(line) = &app.command else {
        return;
    };
    let width = usize::from(area.width);
    let right = match &line.error {
        Some(error) => vec![Span::styled(
            fit(&format!(" {error} "), width / 2),
            Style::new().fg(c.red),
        )],
        None => hints_fitting(" `Tab` complete · `Enter` run · `Esc` close ", width / 2, c),
    };
    let right_width: usize = right.iter().map(Span::width).sum();
    let room = width.saturating_sub(right_width + 2);
    let typed = tail(&line.text, room);
    let used = 2 + typed.width();
    let mut spans = vec![
        Span::styled(":", Style::new().fg(c.accent).add_modifier(Modifier::BOLD)),
        Span::styled(typed.to_string(), Style::new().fg(c.text)),
        Span::styled("▏", Style::new().fg(c.accent)),
        Span::raw(" ".repeat(width.saturating_sub(used + right_width))),
    ];
    spans.extend(right);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(c.bg)),
        area,
    );
}

/// Completions shown at once, at most; the rest scroll.
const MAX_COMPLETIONS: usize = 12;
/// The most columns a completion's name, and what it does, take.
const MAX_NAME_SHOWN: usize = 36;
const MAX_ABOUT_SHOWN: usize = 64;

/// The command line's completions, in a box over it, lined up under the
/// word they complete: each command (with its argument) or value, what it
/// does and its key. The one Enter takes is lit, the value in use marked,
/// and a command that can't run now dimmed.
fn command_menu(frame: &mut Frame, app: &mut App, area: Rect, status: Rect) {
    let c = app.colors.clone();
    let Some(line) = app.command.as_mut() else {
        return;
    };
    let list = &line.completion.candidates;
    let room = usize::from(status.y.saturating_sub(area.y)).saturating_sub(2);
    let rows = list.len().min(MAX_COMPLETIONS).min(room);
    if rows == 0 {
        return;
    }
    let names: Vec<String> = list
        .iter()
        .map(|k| {
            if k.hint.is_empty() {
                k.text.clone()
            } else {
                format!("{} {}", k.text, k.hint)
            }
        })
        .collect();
    let widest = |widths: &mut dyn Iterator<Item = usize>| widths.max().unwrap_or(0);
    let name_width = widest(&mut names.iter().map(|n| n.width())).min(MAX_NAME_SHOWN);
    let about_width = widest(&mut list.iter().map(|k| k.about.width())).min(MAX_ABOUT_SHOWN);
    let key_width = widest(&mut list.iter().map(|k| k.key.width()));
    let key_room = if key_width > 0 { key_width + 2 } else { 0 };
    let title = fit(&format!(" {} ", line.completion.title), MAX_ABOUT_SHOWN);
    // The border, a bar, the mark, the name, two spaces, what it does,
    // the key, a space and the border.
    let width = (name_width + about_width + key_room + 8)
        .max(title.width() + 4)
        .min(usize::from(area.width)) as u16;
    let height = rows as u16 + 2;
    // The names start four columns in: under the word, after the `:`.
    let word = 1 + line.source()[..line.completion.from].width() as u16;
    let x = (status.x + word)
        .saturating_sub(4)
        .min(area.right().saturating_sub(width))
        .max(area.x);
    let popup = Rect {
        x,
        y: status.y - height,
        width,
        height,
    };
    let lit = line.selected.unwrap_or(0);
    line.scroll = line.scroll.min(lit);
    if lit >= line.scroll + rows {
        line.scroll = lit + 1 - rows;
    }
    frame.render_widget(Clear, popup);
    let block = bordered()
        .border_style(border(true, &c))
        .title(Span::styled(
            title,
            Style::new().fg(c.text).add_modifier(Modifier::BOLD),
        ))
        .style(Style::new().bg(c.panel));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let inner_width = usize::from(inner.width);
    let name_room = name_width.min(inner_width.saturating_sub(3));
    let about_room = inner_width.saturating_sub(3 + name_room + 2 + key_room + 1);
    let lines: Vec<Line> = list
        .iter()
        .zip(&names)
        .enumerate()
        .skip(line.scroll)
        .take(rows)
        .map(|(i, (k, name))| {
            let chosen = i == lit;
            let bar = if chosen {
                Span::styled("▌", Style::new().fg(c.accent))
            } else {
                Span::raw(" ")
            };
            let mark = if k.current {
                Span::styled("● ", Style::new().fg(c.accent))
            } else {
                Span::raw("  ")
            };
            let name_color = if k.ready { c.text } else { c.dim };
            let name = fit(name, name_room);
            let pad = name_room.saturating_sub(name.width()) + 2;
            let about = fit(&k.about, about_room);
            let gap = about_room.saturating_sub(about.width());
            let mut spans = vec![
                bar,
                mark,
                Span::styled(
                    name,
                    Style::new().fg(name_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" ".repeat(pad)),
                Span::styled(about, Style::new().fg(c.subtext)),
            ];
            if key_room > 0 && inner_width >= 3 + name_room + 2 + key_room {
                spans.push(Span::raw(" ".repeat(gap + 2)));
                spans.push(Span::styled(
                    format!("{:>key_width$}", k.key),
                    Style::new().fg(c.accent),
                ));
            }
            let row = Line::from(spans);
            if chosen {
                row.style(Style::new().bg(c.selected))
            } else {
                row
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    let hidden = list.len().saturating_sub(rows);
    if hidden > 0 {
        let mut state = ScrollbarState::new(hidden).position(line.scroll);
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

/// A message, or the keys for where you are; your subscription count on
/// the right. The command line takes its place while it's open.
fn status_bar(frame: &mut Frame, app: &App, area: Rect) {
    if app.command.is_some() {
        return command_line(frame, app, area);
    }
    let c = &app.colors;
    let width = area.width as usize;
    let right = format!(" {} subscriptions ", app.subscriptions.len());
    let room = width.saturating_sub(right.width());
    let failed = app.failed_selected();
    let left = match (&app.status, &app.prompt) {
        (_, Some(prompt)) => {
            let keys = match prompt.kind {
                PromptKind::Search => {
                    " `Enter` search or open a link · `Ctrl-j` videos · `Esc` cancel · `Ctrl-u` clear"
                }
                PromptKind::Import => {
                    " Paste or drop a Takeout file, or the unzipped Takeout folder · `Enter` import · `Esc` cancel"
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
        // The selected video couldn't be played: why, and that Enter tries
        // it again.
        (None, None) if failed.is_some() => {
            let hint = hint_spans(" · `Enter` try again", c);
            let used: usize = hint.iter().map(Span::width).sum();
            let why = format!(
                " {} Can't play: {}",
                app.icons.failed,
                failed.unwrap_or_default()
            );
            let mut spans = vec![Span::styled(
                fit(&why, room.saturating_sub(used)),
                Style::new().fg(c.red),
            )];
            spans.extend(hint);
            spans
        }
        (None, None) if app.watch.is_some() => {
            let keys = if app.watching_what_plays() {
                " `Esc` back · `Space` pause · `,` `.` seek · `Tab` comments · `Ctrl-l` up next · `e` jukebox · `w` watch later · `c` channel · `?` help"
            } else {
                " `Esc` back · `Enter` play · `Space` pause · `Tab` comments · `Ctrl-l` up next · `e` jukebox · `w` watch later · `c` channel · `?` help"
            };
            hints_fitting(keys, room, c)
        }
        (None, None) => {
            let keys = match app.focus {
                Focus::Grid if app.view == View::Jukebox => {
                    " `Enter` play · `:` commands · `a` listen · `J` `K` move · `E` next · `x` remove · `C` clear · `v` list or cards · `?` help"
                }
                Focus::Grid if matches!(app.view, View::MyPlaylist(..)) => {
                    " `Enter` play · `:` commands · `a` listen · `e` jukebox · `w` watch later · `x` remove · `C` delete playlist · `?` help"
                }
                Focus::Grid => {
                    " `Enter` play · `:` commands · `a` listen · `i` info · `e` jukebox · `M` mix · `w` watch later · `c` channel · `S` subscribe · `F` group · `/` search · `?` help"
                }
                Focus::Sidebar => {
                    " `↑↓` move · `Enter` open · `:` commands · `Ctrl-l` videos · `F` group · `/` search · `I` import · `?` help"
                }
            };
            // Audio only: Enter listens, as a does.
            if app.settings.audio_only {
                let keys = keys
                    .replace("`Enter` play", "`Enter` listen")
                    .replace(" · `a` listen", "");
                hints_fitting(&keys, room, c)
            } else {
                hints_fitting(keys, room, c)
            }
        }
    };
    let used: usize = left.iter().map(Span::width).sum();
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(room.saturating_sub(used))));
    spans.push(Span::styled(right, Style::new().fg(c.dim)));
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line).style(Style::new().bg(c.bg)), area);
}

const HELP: [(&str, &str); 39] = [
    ("←↓↑→  h j k l", "move, in the pane that has the keys"),
    (
        "Ctrl + ←↓↑→  h j k l",
        "to the pane that way: sidebar, search box, videos; on a video's page, up next",
    ),
    ("gg  G  Home  End", "first, last"),
    ("PgUp PgDn  Ctrl-u Ctrl-d", "a page, half a page"),
    ("Tab", "sidebar ↔ videos"),
    ("Enter", "play, on the video's page"),
    (
        "p",
        "the page of the video playing (or selected): Esc or q goes back, X too",
    ),
    (
        "P",
        "in Pepper: the picture-in-picture box while browsing, on or off",
    ),
    (
        "f",
        "full screen: in Pepper the whole tab (f or Esc goes back), else mpv's window",
    ),
    ("a", "listen: sound only (Enter too, with audio only on)"),
    (
        "0",
        "back to the start of what plays; nothing playing: the video from its start",
    ),
    ("M", "YouTube's Mix of the video (music only)"),
    (
        "e  /  E",
        "into the Jukebox: last  /  next, after the one playing",
    ),
    ("J  K  /  C", "in the Jukebox: move  /  clear it"),
    ("v", "a list or cards (in the Jukebox, just it)"),
    ("N  /  A", "next video  /  autoplay on or off"),
    ("r", "repeat: the whole list, then one video, then off"),
    (
        "z",
        "shuffle the Jukebox, playlists and Mixes: each video once a round",
    ),
    ("T", "subtitles on or off"),
    ("i", "the video's whole description, and its comments"),
    ("w", "watch later (again to remove)"),
    (
        "x",
        "remove from the Jukebox, Watch later, History or a playlist",
    ),
    (
        "C",
        "clear the Jukebox, or delete the playlist or group shown",
    ),
    ("c", "the video's channel"),
    ("S  /  u", "subscribe or unsubscribe  /  undo"),
    (
        "F",
        "groups: put the channel in some; each is a feed of its channels in the sidebar",
    ),
    (
        "/  s",
        "search YouTube, or open a YouTube link (or paste one)",
    ),
    ("Backspace  Ctrl-o", "back"),
    ("o  /  y", "open in the browser  /  copy the link"),
    ("R", "refresh"),
    (
        "I",
        "import from Google Takeout: subscriptions, playlists, history",
    ),
    ("Space", "pause or play"),
    ("-  +  /  m", "volume down or up  /  mute"),
    (",  .  /  <  >", "back or ahead 10 s  /  1 min;  X stops"),
    ("t", "chapters, or go to a time: 1:23, +30, 50%"),
    ("[  ]", "the chapter before  /  the next chapter"),
    (
        ":",
        "commands: any action or setting by name, Tab completes (:quality 720)",
    ),
    (
        "?  /  O",
        "these keys  /  settings: quality, theme, Shorts…",
    ),
    ("q", "quit (on a video's page, back to the list)"),
];

/// The `?` popup, centered: a tab with every key, and one with the
/// settings. Both are as big as the bigger one, so the tabs don't move.
/// `text` in rows at most `width` columns wide, broken between words (a
/// word wider than a row is broken where it must be).
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split(' ') {
        let gap = usize::from(!row.is_empty());
        if row.width() + gap + word.width() <= width {
            if gap == 1 {
                row.push(' ');
            }
            row.push_str(word);
            continue;
        }
        if !row.is_empty() {
            rows.push(std::mem::take(&mut row));
        }
        for grapheme in word.graphemes(true) {
            // A character wider than a row takes one of its own.
            if !row.is_empty() && row.width() + grapheme.width() > width {
                rows.push(std::mem::take(&mut row));
            }
            row.push_str(grapheme);
        }
    }
    rows.push(row);
    rows
}

/// The `i` popup: the video's title, channel and numbers, then its whole
/// description (the cursor's line lit: Enter goes to the time in it) or its
/// top comments, each wrapped to the popup.
fn info_popup(frame: &mut Frame, app: &mut App, area: Rect) {
    let c = app.colors.clone();
    let title = app
        .info_popup
        .as_ref()
        .map(|p| app.title(&p.video).to_string());
    let (Some(popup), Some(title)) = (app.info_popup.as_mut(), title) else {
        return;
    };
    let width = area.width.saturating_sub(8).clamp(20, 100).min(area.width);
    let height = area.height.saturating_sub(4).max(8).min(area.height);
    let place = Rect {
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
        tab(" Description ", popup.tab == InfoTab::Description),
        Span::raw(" "),
        tab(" Comments ", popup.tab == InfoTab::Comments),
        Span::raw(" "),
    ]);
    let description: Vec<&str> = popup.video.description.lines().collect();
    let time = description
        .get(popup.line)
        .and_then(|line| crate::app::time_in(line));
    let hint = match (popup.tab, time) {
        (InfoTab::Description, Some(at)) => format!(
            " `j k` move · `Enter` go to {} · `Tab` comments · `Esc` close ",
            video::duration(at as u32)
        ),
        (InfoTab::Description, None) => " `j k` move · `Tab` comments · `Esc` close ".into(),
        (InfoTab::Comments, _) => " `j k` scroll · `Tab` description · `Esc` close ".into(),
    };
    frame.render_widget(Clear, place);
    let block = bordered()
        .border_style(border(true, &c))
        .title(tabs)
        .title_bottom(Line::from(hint_spans(&hint, &c)).right_aligned())
        .style(Style::new().bg(c.panel));
    let inner = block.inner(place).inner(Margin::new(1, 0));
    frame.render_widget(block, place);
    video_info(frame, &c, &app.comments, popup, &title, inner, None, true);
}

/// A video's title, channel and views, then its description or comments
/// (`info.tab`) under them, in `inner`: the `i` popup's, and the watch
/// page's. `tabs`, if given, goes between the two (the popup has its tabs
/// in its border). The description's cursor line shows while `active`.
#[allow(clippy::too_many_arguments)]
pub(super) fn video_info(
    frame: &mut Frame,
    c: &Colors,
    comments: &std::collections::HashMap<crate::ids::VideoId, Comments>,
    popup: &mut crate::app::InfoPopup,
    title: &str,
    inner: Rect,
    tabs: Option<Line>,
    active: bool,
) {
    let columns = usize::from(inner.width);
    let description: Vec<&str> = popup.video.description.lines().collect();

    // The video, on top whichever the tab.
    let v = &popup.video;
    let mut about = vec![v.channel.clone()];
    about.extend(v.views.map(video::views));
    about.extend(v.published.map(|at| video::age(at, crate::app::now())));
    about.retain(|s| !s.is_empty());
    let mut head: Vec<Line> = wrap(title, columns)
        .into_iter()
        .take(2)
        .map(|row| Line::styled(row, Style::new().fg(c.text).add_modifier(Modifier::BOLD)))
        .collect();
    // DeArrow's title is on top: YouTube's is said too.
    if title != v.title {
        head.push(Line::styled(
            fit(&format!("On YouTube: {}", v.title), columns),
            Style::new().fg(c.subtext),
        ));
    }
    head.push(Line::styled(
        fit(&about.join(" · "), columns),
        Style::new().fg(c.dim),
    ));
    head.push(Line::default());
    if let Some(tabs) = tabs {
        head.push(tabs);
        head.push(Line::default());
    }
    let head_rows = head.len() as u16;
    frame.render_widget(
        Paragraph::new(head),
        Rect {
            height: head_rows.min(inner.height),
            ..inner
        },
    );
    let body = Rect {
        y: inner.y + head_rows.min(inner.height),
        height: inner.height.saturating_sub(head_rows),
        ..inner
    };
    let visible = usize::from(body.height);

    let lines: Vec<Line> = match popup.tab {
        InfoTab::Description if description.is_empty() => {
            vec![Line::styled("No description.", Style::new().fg(c.dim))]
        }
        InfoTab::Description => {
            let mut rows: Vec<(Line, usize)> = Vec::new();
            for (i, text) in description.iter().enumerate() {
                let style = if active && i == popup.line {
                    Style::new().bg(c.selected).fg(c.text)
                } else {
                    Style::new().fg(c.text)
                };
                for row in wrap(text, columns) {
                    let pad = columns.saturating_sub(row.width());
                    rows.push((Line::styled(format!("{row}{}", " ".repeat(pad)), style), i));
                }
            }
            // As little scrolling as keeps the cursor's line in view.
            let first = rows.iter().position(|(_, i)| *i == popup.line).unwrap_or(0);
            let last = rows
                .iter()
                .rposition(|(_, i)| *i == popup.line)
                .unwrap_or(0);
            let mut scroll = popup.description_scroll.get();
            if first < scroll {
                scroll = first;
            } else if last >= scroll + visible {
                scroll = (last + 1).saturating_sub(visible);
            }
            popup.description_scroll.set(scroll);
            rows.into_iter()
                .skip(scroll)
                .map(|(line, _)| line)
                .collect()
        }
        InfoTab::Comments => {
            let rows: Vec<Line> = match comments.get(&v.id) {
                None | Some(Comments::Loading) => vec![
                    Line::styled("Loading the top comments…", Style::new().fg(c.subtext)),
                    Line::styled(
                        "yt-dlp reads the video's page for them, as it does to play it.",
                        Style::new().fg(c.dim),
                    ),
                ],
                Some(Comments::Failed(error)) => wrap(&format!("No comments: {error}"), columns)
                    .into_iter()
                    .map(|row| Line::styled(row, Style::new().fg(c.red)))
                    .collect(),
                Some(Comments::Loaded(list)) if list.is_empty() => vec![Line::styled(
                    "No comments, or they're turned off.",
                    Style::new().fg(c.dim),
                )],
                Some(Comments::Loaded(list)) => {
                    let mut rows = Vec::new();
                    for comment in list {
                        let name = if comment.by_uploader {
                            Style::new().fg(c.accent).add_modifier(Modifier::BOLD)
                        } else {
                            Style::new().fg(c.text).add_modifier(Modifier::BOLD)
                        };
                        let mut meta = Vec::new();
                        if comment.pinned {
                            meta.push("pinned".to_string());
                        }
                        meta.extend(comment.likes.filter(|&n| n > 0).map(|n| {
                            format!(
                                "{} {}",
                                video::short_number(n),
                                if n == 1 { "like" } else { "likes" }
                            )
                        }));
                        meta.extend(comment.at.map(|at| video::age(at, crate::app::now())));
                        let meta = meta.iter().map(|m| format!(" · {m}")).collect::<String>();
                        let author = fit(&comment.author, columns.saturating_sub(meta.width()));
                        rows.push(Line::from(vec![
                            Span::styled(author, name),
                            Span::styled(meta, Style::new().fg(c.dim)),
                        ]));
                        for text in comment.text.lines() {
                            rows.extend(
                                wrap(text, columns)
                                    .into_iter()
                                    .map(|row| Line::styled(row, Style::new().fg(c.text))),
                            );
                        }
                        rows.push(Line::default());
                    }
                    rows
                }
            };
            popup.scroll = popup.scroll.min(rows.len().saturating_sub(visible));
            rows.into_iter().skip(popup.scroll).collect()
        }
    };
    frame.render_widget(Paragraph::new(lines), body);
}

fn help(frame: &mut Frame, app: &mut App, area: Rect) {
    let c = app.colors.clone();
    let Some(menu) = app.help.as_mut() else {
        return;
    };
    let keys = key_lines(&c);
    let unplug_failed = match &app.output_watch {
        OutputWatch::Failed(why) => Some(why.as_str()),
        _ => None,
    };
    let (settings, cursor) = settings_lines(&app.settings, unplug_failed, menu.selected, &c);
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
        HelpTab::Settings if HelpMenu::is_choice(menu.selected) => {
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

/// The groups popup (`F`): the groups of your subscriptions, those the
/// channel is in ticked, then "New group"; a name being typed above them.
fn group_menu(frame: &mut Frame, app: &mut App, area: Rect) {
    let c = app.colors.clone();
    let groups = app.groups.clone();
    let Some(menu) = app.group_menu.as_mut() else {
        return;
    };
    let counts: Vec<String> = groups.iter().map(|g| g.count.to_string()).collect();
    let count_width = counts.iter().map(|n| n.width()).max().unwrap_or(0);
    let widest = groups.iter().map(|g| g.name.width()).max().unwrap_or(0);
    // A bar, the box, the name, two spaces, the count, a space.
    let width = (widest + count_width + 10).clamp(60, 72) + 2;
    let width = width.min(usize::from(area.width)) as u16;
    let typing = usize::from(menu.naming.is_some()) * 2;
    let rows_wanted = groups.len() + 1 + typing;
    let height = (rows_wanted as u16 + 2).min(area.height.saturating_sub(2).max(5));
    let height = height.min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let title = format!(" Groups · {} ", fit(&menu.name, MAX_NAME_SHOWN));
    let hint = match &menu.naming {
        Some(_) => " `Enter` keep · `Esc` cancel ",
        None if menu.selected < groups.len() => {
            " `Enter` tick · `n` new · `r` rename · `x` delete · `Esc` close "
        }
        None => " `Enter` new group · `Esc` close ",
    };
    frame.render_widget(Clear, popup);
    let block = bordered()
        .border_style(border(true, &c))
        .title(Span::styled(
            title,
            Style::new().fg(c.text).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(hints_fitting(
                hint,
                usize::from(width).saturating_sub(4),
                &c,
            ))
            .right_aligned(),
        )
        .style(Style::new().bg(c.panel));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let inner_width = usize::from(inner.width);

    let mut lines = Vec::new();
    if let Some(naming) = &menu.naming {
        let asked = match naming.group {
            Some(_) => " New name ",
            None => " New group ",
        };
        let room = inner_width.saturating_sub(asked.width() + 1);
        lines.push(Line::from(vec![
            Span::styled(asked, Style::new().fg(c.dim)),
            Span::styled(
                tail(&naming.text, room),
                Style::new().fg(c.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled("▏", Style::new().fg(c.accent)),
        ]));
        lines.push(Line::default());
    }
    // Scrolled as little as keeps the cursor in view.
    let rows = usize::from(inner.height).saturating_sub(lines.len());
    menu.selected = menu.selected.min(groups.len());
    menu.scroll = menu.scroll.min(menu.selected);
    if rows > 0 && menu.selected >= menu.scroll + rows {
        menu.scroll = menu.selected + 1 - rows;
    }
    let end = (menu.scroll + rows).min(groups.len() + 1);
    for i in menu.scroll..end {
        let chosen = i == menu.selected && menu.naming.is_none();
        let bar = if chosen {
            Span::styled("▌", Style::new().fg(c.accent))
        } else {
            Span::raw(" ")
        };
        let line = match groups.get(i) {
            Some(group) => {
                let check = if menu.inside.contains(&group.id) {
                    "[✓] "
                } else {
                    "[ ] "
                };
                let count = format!("  {:>count_width$} ", group.count);
                let room = inner_width.saturating_sub(1 + check.width() + count.width());
                let name = fit(&group.name, room);
                let pad = room.saturating_sub(name.width());
                Line::from(vec![
                    bar,
                    Span::styled(check, Style::new().fg(c.accent)),
                    Span::styled(name, Style::new().fg(c.text)),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(count, Style::new().fg(c.dim)),
                ])
            }
            None => Line::from(vec![
                bar,
                Span::styled(" +  ", Style::new().fg(c.accent)),
                Span::styled("New group", Style::new().fg(c.subtext)),
            ]),
        };
        lines.push(if chosen {
            line.style(Style::new().bg(c.selected))
        } else {
            line
        });
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The chapters popup (`t`), centered: where a typed time goes, then the
/// chapters, each at its start, the one playing marked and the one under
/// the cursor lit.
fn chapter_menu(frame: &mut Frame, app: &mut App, area: Rect) {
    let c = app.colors.clone();
    let icons = app.icons;
    let (Some(menu), Some(playing)) = (app.chapter_menu.as_mut(), app.playing.as_ref()) else {
        return;
    };
    let list = &playing.chapters;
    let playing_now = chapters::current(list, playing.position);
    let times: Vec<String> = list
        .iter()
        .map(|ch| video::duration(ch.start as u32))
        .collect();
    let time_width = times.iter().map(|t| t.width()).max().unwrap_or(0);
    let widest = list.iter().map(|ch| ch.title.width()).max().unwrap_or(0);
    // A bar, the mark, the time, two spaces, the title, a space.
    let width = (widest + time_width + 7).clamp(48, 80) + 2;
    let width = width.min(usize::from(area.width)) as u16;
    let height = (list.len().max(1) as u16 + 4).min(area.height.saturating_sub(2).max(5));
    let height = height.min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let title = if list.is_empty() {
        " Go to a time ".to_string()
    } else {
        format!(" Chapters · {} ", list.len())
    };
    let hint = if list.is_empty() {
        " `Enter` go · `Esc` close "
    } else if menu.typed.is_empty() {
        " `Enter` go · `j k` choose · `Esc` close "
    } else {
        " `Enter` go to the time · `Esc` close "
    };
    frame.render_widget(Clear, popup);
    let block = bordered()
        .border_style(border(true, &c))
        .title(Span::styled(
            title,
            Style::new().fg(c.text).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(hint_spans(hint, &c)).right_aligned())
        .style(Style::new().bg(c.panel));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let inner_width = usize::from(inner.width);

    let mut lines = Vec::new();
    let mut typed = vec![
        Span::styled(" Go to ", Style::new().fg(c.dim)),
        Span::styled(
            menu.typed.clone(),
            Style::new().fg(c.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled("▏", Style::new().fg(c.accent)),
    ];
    if menu.typed.is_empty() {
        typed.push(Span::styled(
            "type a time: 1:23 · 90 · +30 · 50%",
            Style::new().fg(c.dim),
        ));
    }
    lines.push(Line::from(typed));
    lines.push(Line::default());
    let rows = usize::from(inner.height).saturating_sub(lines.len());
    if list.is_empty() {
        lines.push(Line::from(Span::styled(
            " This video has no chapters",
            Style::new().fg(c.dim),
        )));
    } else {
        // Scrolled as little as keeps the cursor in view.
        menu.selected = menu.selected.min(list.len() - 1);
        menu.scroll = menu.scroll.min(menu.selected);
        if rows > 0 && menu.selected >= menu.scroll + rows {
            menu.scroll = menu.selected + 1 - rows;
        }
        let end = (menu.scroll + rows).min(list.len());
        for i in menu.scroll..end {
            let chosen = i == menu.selected;
            let bar = if chosen {
                Span::styled("▌", Style::new().fg(c.accent))
            } else {
                Span::raw(" ")
            };
            let mark = if playing_now == Some(i) {
                Span::styled(
                    format!("{} ", icons.play),
                    Style::new().fg(c.red).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };
            let time = format!("{:>time_width$}  ", times[i]);
            let room = inner_width.saturating_sub(3 + time.width() + 1);
            let line = Line::from(vec![
                bar,
                mark,
                Span::styled(time, Style::new().fg(c.accent)),
                Span::styled(fit(&list[i].title, room), Style::new().fg(c.text)),
            ]);
            lines.push(if chosen {
                line.style(Style::new().bg(c.selected))
            } else {
                line
            });
        }
        let hidden = list.len().saturating_sub(rows);
        if hidden > 0 && rows > 0 {
            let mut state = ScrollbarState::new(hidden).position(menu.scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .style(Style::new().fg(c.dim)),
                // Beside the chapters, on the right border.
                Rect {
                    y: inner.y + 2,
                    height: inner.height.saturating_sub(2),
                    ..popup
                },
                &mut state,
            );
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
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
fn settings_lines(
    settings: &Settings,
    unplug_failed: Option<&str>,
    selected: usize,
    c: &Colors,
) -> (Vec<Line<'static>>, usize) {
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
    add(
        &mut lines,
        HelpMenu::LIST,
        check(settings.list),
        "Show videos as a list, without thumbnails",
    );
    // Not a row: block lists are written in settings.toml.
    let blocked = match settings.blocked() {
        (0, 0) => "No channels or words blocked".to_string(),
        (channels, words) => {
            let s = |n: usize| if n == 1 { "" } else { "s" };
            format!(
                "Blocked: {channels} channel{} and {words} word{}",
                s(channels),
                s(words)
            )
        }
    };
    lines.push(Line::styled(
        format!("     {blocked} (blocked_channels, blocked_words in settings.toml)"),
        Style::new().fg(c.dim),
    ));
    lines.push(Line::default());
    lines.push(heading(" Playing"));
    add(
        &mut lines,
        HelpMenu::AUTOPLAY,
        check(settings.autoplay),
        "Autoplay: the next video when one ends (the Jukebox always)",
    );
    add(
        &mut lines,
        HelpMenu::SHUFFLE,
        check(settings.shuffle),
        "Shuffle the Jukebox, playlists and Mixes, each video once a round",
    );
    add(
        &mut lines,
        HelpMenu::SPONSORBLOCK,
        check(settings.sponsorblock),
        "Skip sponsors with SponsorBlock (asks sponsor.ajay.app)",
    );
    add(
        &mut lines,
        HelpMenu::DEARROW,
        check(settings.dearrow),
        "Better titles from DeArrow (asks sponsor.ajay.app)",
    );
    add(
        &mut lines,
        HelpMenu::SUBTITLES,
        check(settings.subtitles),
        "Subtitles, in your language when a video has them",
    );
    add(
        &mut lines,
        HelpMenu::PAUSE_ON_UNPLUG,
        check(settings.pause_on_unplug),
        "Pause when the headphones are unplugged (macOS)",
    );
    // Not a row: why unplugging can't be seen, if it can't.
    if let Some(why) = unplug_failed {
        lines.push(Line::styled(
            format!("     Unplugging can't be seen: {why}"),
            Style::new().fg(c.dim),
        ));
    }
    lines.push(Line::default());
    // The dot marks the one in use, as for the themes.
    let dot = |on: bool| if on { " ● " } else { " ○ " };
    lines.push(heading(" Video quality"));
    for (i, &height) in settings::HEIGHTS.iter().enumerate() {
        let on = settings.height_choice() == Some(i);
        let label = settings::height_label(height);
        add(&mut lines, HelpMenu::VIDEO_QUALITY + i, dot(on), &label);
    }
    lines.push(Line::default());
    lines.push(heading(" Sound quality"));
    for (i, quality) in AudioQuality::ALL.iter().enumerate() {
        let label = match quality {
            AudioQuality::High => "High: about 130 kbps, the best without an account",
            AudioQuality::Low => "Low: about 70 kbps, to save data",
        };
        let on = settings.audio_quality == *quality;
        add(&mut lines, HelpMenu::AUDIO_QUALITY + i, dot(on), label);
    }
    lines.push(Line::default());
    lines.push(heading(" Listening"));
    add(
        &mut lines,
        HelpMenu::AUDIO_ONLY,
        check(settings.audio_only),
        "Audio only, as YouTube Music: Enter plays the sound alone",
    );
    add(
        &mut lines,
        HelpMenu::VISUALIZER,
        check(settings.visualizer),
        "Visualizer: colored bars that move with the sound",
    );
    add(
        &mut lines,
        HelpMenu::VISUALIZER_WHITE,
        check(settings.visualizer_white),
        "Visualizer in white, not the rainbow",
    );
    lines.push(Line::default());
    lines.push(heading(" Jukebox"));
    add(
        &mut lines,
        HelpMenu::JUKEBOX_LIST,
        check(settings.jukebox_list),
        "Show it as a list, even when the rest are cards",
    );
    add(
        &mut lines,
        HelpMenu::JUKEBOX_AUTOREMOVE,
        check(settings.jukebox_autoremove),
        "Take a video off once it has played to its end",
    );
    lines.push(Line::default());
    lines.push(heading(" Privacy"));
    add(
        &mut lines,
        HelpMenu::HISTORY,
        check(settings.history),
        "Keep a history of what you watch, here only",
    );
    add(
        &mut lines,
        HelpMenu::NOW_PLAYING,
        check(settings.now_playing),
        "Show what plays in Now Playing, with the media keys (macOS)",
    );
    lines.push(Line::default());
    lines.push(heading(" Pepper"));
    add(
        &mut lines,
        HelpMenu::INLINE_VIDEO,
        check(settings.inline_video),
        "Play the picture in the terminal, not in mpv's window",
    );
    // Not a row: where Pepper comes from.
    lines.push(Line::styled(
        "     Pepper: Ghostty with video inside the terminal, github.com/erictran308/pepper",
        Style::new().fg(c.dim),
    ));
    lines.push(Line::default());
    lines.push(heading(" Theme"));
    for (i, (id, name)) in theme::names().iter().enumerate() {
        add(
            &mut lines,
            HelpMenu::THEMES + i,
            dot(*id == settings.theme),
            name,
        );
    }
    (lines, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Prompt;
    use crate::app::tests::{CH, app, listening, mpv_control, sponsor_at, with_feed};
    use crate::ids::ChannelId;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    async fn imported_playlists_have_a_section_of_the_sidebar() {
        let mut app = app();
        with_feed(&mut app, 2);
        let id = crate::ids::VideoId::parse("vid00000001").unwrap();
        app.store
            .import_playlist("Road trip", &[(id, None)])
            .unwrap();
        app.status = None;
        app.my_playlists = app.store.playlists().unwrap();
        let shown = screen(&mut app, 140, 30);
        let rows: Vec<&str> = shown.lines().collect();
        let at = |text: &str| rows.iter().position(|l| l.contains(text)).expect(text);
        assert!(at("History") < at("Playlists (1)"));
        assert!(at("Playlists (1)") < at("Road trip  1"));
        assert!(at("Road trip  1") < at("Subscriptions (1)"));
        assert!(at("Subscriptions (1)") < at("BekBrace"));
    }

    #[tokio::test]
    async fn the_info_popup_shows_the_description_then_the_comments() {
        let mut app = app();
        app.status = None;
        with_feed(&mut app, 1);
        let mut v = app.videos[0].clone();
        v.description = "First line of it\n\n1:23 The good part".into();
        app.store.save_videos(std::slice::from_ref(&v)).unwrap();
        app.on_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for _ in 0..2 {
            app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        let shown = screen(&mut app, 120, 40);
        assert!(shown.contains("First line of it"), "{shown}");
        assert!(shown.contains("Enter go to 1:23"), "{shown}");
        app.info_popup.as_mut().unwrap().tab = InfoTab::Comments;
        app.comments.insert(
            v.id.clone(),
            Comments::Loaded(vec![crate::ytdlp::Comment {
                author: "@someone".into(),
                text: "Great video".into(),
                likes: Some(1200),
                at: None,
                pinned: true,
                by_uploader: false,
            }]),
        );
        let shown = screen(&mut app, 120, 40);
        assert!(shown.contains("@someone · pinned · 1.2K likes"), "{shown}");
        assert!(shown.contains("Great video"));
        for width in 1..120 {
            screen(&mut app, width, 40);
        }
    }

    #[tokio::test]
    async fn the_watch_page_shows_the_video_its_description_and_up_next() {
        let mut app = app();
        app.status = None;
        with_feed(&mut app, 3);
        let mut v = app.videos[0].clone();
        v.description = "What this video is about\n\n2:00 The middle".into();
        app.store.save_videos(std::slice::from_ref(&v)).unwrap();
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let next = app.up_next[0].title.clone();
        let shown = screen(&mut app, 150, 45);
        assert!(shown.contains("Watch"), "{shown}");
        assert!(shown.contains(&v.title), "{shown}");
        assert!(shown.contains("What this video is about"), "{shown}");
        assert!(shown.contains("Up next"), "{shown}");
        assert!(shown.contains(&next), "{shown}");
        assert!(
            !shown.contains("subscriptions ·"),
            "the sidebar is behind it"
        );
        // Narrow: up next takes the description's place, on Ctrl-l.
        let shown = screen(&mut app, 80, 45);
        assert!(!shown.contains("Up next"), "{shown}");
        app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        let shown = screen(&mut app, 80, 45);
        assert!(shown.contains(&next), "{shown}");
        assert!(!shown.contains("What this video is about"), "{shown}");
        for width in 1..160 {
            screen(&mut app, width, 45);
        }
        for height in 1..45 {
            screen(&mut app, 120, height);
        }
    }

    #[tokio::test]
    async fn the_watch_page_leaves_a_third_of_a_tall_window_for_the_description() {
        let mut app = app();
        app.status = None;
        with_feed(&mut app, 3);
        let mut v = app.videos[0].clone();
        v.description = (1..=12).map(|n| format!("Line {n}\n")).collect();
        app.store.save_videos(std::slice::from_ref(&v)).unwrap();
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Wide enough that the picture would take all but a few rows.
        let shown = screen(&mut app, 300, 70);
        assert!(shown.contains("Line 12"), "{shown}");
    }

    #[tokio::test]
    async fn in_pepper_the_picture_shows_on_its_page_or_in_a_corner() {
        let mut app = app();
        with_feed(&mut app, 2);
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, false);
        // Not inside Pepper: nowhere.
        drawn(&mut app, 160, 45);
        assert_eq!(app.video_box.get(), None);
        // Browsing: the bottom right corner of the videos.
        app.playing.as_mut().unwrap().pepper = Some("tok".into());
        drawn(&mut app, 160, 45);
        let corner = app.video_box.get().expect("a corner box");
        assert_eq!(corner.right(), 159);
        assert!(corner.y > 20 && corner.width >= crate::pepper::PIP_MIN_COLS);
        // P hides it.
        app.on_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));
        drawn(&mut app, 160, 45);
        assert_eq!(app.video_box.get(), None);
        // Its page: at the top, wide.
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        drawn(&mut app, 160, 45);
        let page = app.video_box.get().expect("the page's picture");
        assert!(page.y < 3 && page.width > corner.width, "{page:?}");
        for width in 1..160 {
            drawn(&mut app, width, 45);
        }
        // f: the whole tab; Esc back to the page, still open.
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        drawn(&mut app, 160, 45);
        assert_eq!(app.video_box.get(), Some(Rect::new(0, 0, 160, 45)));
        for width in 1..160 {
            drawn(&mut app, width, 45);
        }
        drawn(&mut app, 160, 45);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        drawn(&mut app, 160, 45);
        assert_eq!(app.video_box.get(), Some(page));
        assert!(app.watch.is_some());
        // f again, then f back.
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        drawn(&mut app, 160, 45);
        assert_eq!(app.video_box.get(), Some(page));
        // Pepper is told the whole tab; a popup over it takes the picture
        // away while it's open.
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        drawn(&mut app, 160, 45);
        assert_eq!(
            app.video_placement(),
            crate::pepper::place("tok", Rect::new(0, 0, 160, 45))
        );
        app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        drawn(&mut app, 160, 45);
        assert_eq!(app.video_placement(), crate::pepper::remove("tok"));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        drawn(&mut app, 160, 45);
        assert_eq!(app.video_box.get(), Some(Rect::new(0, 0, 160, 45)));
        // Full screen ends with the playback.
        app.on_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
        assert!(!app.fullscreen);
    }

    #[tokio::test]
    async fn the_player_bar_shows_subtitles_key_only_for_a_video_with_them() {
        let mut app = app();
        app.icons = crate::icons::PLAIN;
        with_feed(&mut app, 1);
        app.show_playing(app.videos[0].clone(), 5.0, 100.0, false);
        assert!(!screen(&mut app, 140, 40).contains("CC T"));
        app.playing.as_mut().unwrap().subtitles = Some("English".into());
        let shown = screen(&mut app, 140, 40);
        assert!(shown.contains("CC T"), "{shown}");
    }

    #[tokio::test]
    async fn home_says_while_youtubes_feeds_are_down() {
        let mut app = app();
        app.status = None;
        with_feed(&mut app, 3);
        assert!(!screen(&mut app, 140, 30).contains("feeds"));
        app.feeds_down = Some(crate::app::FeedsDown::Broken);
        let shown = screen(&mut app, 140, 30);
        assert!(
            shown.contains("YouTube's feeds are down: filling in with yt-dlp"),
            "{shown}"
        );
        app.feeds_down = Some(crate::app::FeedsDown::Unreachable);
        assert!(screen(&mut app, 140, 30).contains("YouTube's feeds can't be reached"));
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
        // Left with Ctrl, it keeps what was typed, dim and with no cursor.
        app.prompt = None;
        app.search_typed = Some("rust".into());
        let top = screen(&mut app, 140, 45);
        let top = top.lines().next().unwrap();
        assert!(top.contains("rust") && !top.contains("rust▏"), "{top}");
        assert!(!top.contains("Search"), "{top}");
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

        let hints = " `Enter` play · `a` listen · `M` mix";
        let text = |spans: Vec<Span>| {
            spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert_eq!(
            text(hints_fitting(hints, 40, &c)),
            " Enter play · a listen · M mix"
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
        assert!(status.starts_with(" Enter play · : commands"), "{status}");
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
        assert!(shown.contains("listen: sound only (Enter too, with audio only on)"));
        assert!(shown.contains("autoplay on or off"));
        assert!(shown.contains(" Keys ") && shown.contains(" Settings "));
        assert!(!shown.contains("Show Shorts"));

        app.help = Some(HelpMenu::new(HelpTab::Settings));
        app.settings.sponsorblock = true;
        app.settings.live = false;
        let shown = screen(&mut app, 120, 52);
        assert!(!shown.contains("listen: sound only"));
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
        assert!(
            line("Audio only, as YouTube Music").contains("[ ]"),
            "off at first"
        );
        assert!(line("Visualizer").contains("[✓]"), "on at first");
        assert!(
            line("Pause when the headphones").contains("[✓]"),
            "on at first"
        );
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
    async fn listening_the_player_bar_grows_bars_that_move_with_the_sound() {
        let mut app = app();
        with_feed(&mut app, 3);
        app.status = None;
        listening(&mut app);
        // Loud bass, heard by now; nothing in the treble.
        let mut levels = [-100.0; crate::visualizer::BANDS];
        levels[..4].fill(-12.0);
        let playing = app.playing.as_mut().unwrap();
        let start = std::time::Instant::now();
        playing.spectrum.push(levels, start);
        playing
            .spectrum
            .step(start + std::time::Duration::from_millis(250));
        let buffer = drawn(&mut app, 140, 40);
        let row = |y: u16| {
            (0..140)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };
        let progress = (0..40)
            .find(|&y| row(y).contains(" 0:10 "))
            .expect("the progress row");
        assert_eq!(progress, 37, "the bar is 4 rows taller");
        assert!(row(progress - 5).starts_with('╭'), "{}", row(progress - 5));
        // Thin bars standing on the progress row, a column in from the
        // border, with a column between them.
        let bottom = progress - 1;
        assert!(row(bottom).starts_with("│ █ █ █"), "{}", row(bottom));
        let bass = &buffer[(2, bottom)];
        assert!(
            matches!(bass.fg, ratatui::style::Color::Rgb(..)),
            "{:?}",
            bass.fg
        );
        assert_eq!(buffer[(130, bottom)].symbol(), " ", "quiet treble");
        // Each bar its own color.
        let bars = (2..20)
            .step_by(2)
            .filter(|&x| buffer[(x, bottom)].symbol() == "█");
        let colors: std::collections::HashSet<_> = bars
            .map(|x| format!("{:?}", buffer[(x, bottom)].fg))
            .collect();
        assert!(colors.len() > 3, "{colors:?}");

        // A short window keeps its rows for the videos.
        let shown = screen(&mut app, 140, 20);
        let lines: Vec<&str> = shown.lines().collect();
        assert!(lines[16].starts_with('╭'), "{shown}");
        // Off: the player bar as it was.
        app.settings.visualizer = false;
        let shown = screen(&mut app, 140, 40);
        assert!(shown.lines().nth(36).unwrap().starts_with('╭'), "{shown}");
        // Where mpv can't be asked (Windows), none.
        app.settings.visualizer = true;
        mpv_control(&mut app, false);
        let shown = screen(&mut app, 140, 40);
        assert!(shown.lines().nth(36).unwrap().starts_with('╭'), "{shown}");
    }

    #[tokio::test]
    async fn a_video_that_cant_be_played_is_marked_and_says_why_when_selected() {
        let mut app = app();
        app.icons = crate::icons::PLAIN;
        with_feed(&mut app, 3);
        app.status = None;
        app.focus = Focus::Grid;
        let id = app.videos[1].id.clone();
        app.failed.insert(id, "Video unavailable".into());
        app.selected = 1;
        let shown = screen(&mut app, 140, 40);
        assert!(shown.contains("✗ Can't play · "), "{shown}");
        let status = shown.lines().last().unwrap();
        assert!(
            status.starts_with(" ✗ Can't play: Video unavailable · Enter try again"),
            "{status}"
        );
        // Another one selected: the keys, as ever.
        app.selected = 0;
        let shown = screen(&mut app, 140, 40);
        let status = shown.lines().last().unwrap();
        assert!(status.starts_with(" Enter play"), "{status}");
        // As a list: marked after its title.
        app.settings.list = true;
        let shown = screen(&mut app, 140, 40);
        assert!(shown.contains("Video vid00000001 ✗"), "{shown}");
    }

    #[tokio::test]
    async fn groups_show_in_the_sidebar_and_in_their_popup() {
        let mut app = app();
        app.icons = crate::icons::PLAIN;
        with_feed(&mut app, 3);
        app.status = None;
        let ch = ChannelId::parse(CH).unwrap();
        let rust = app.store.create_group("Rust").unwrap().unwrap();
        app.store.create_group("Music").unwrap().unwrap();
        app.store.set_in_group(rust, &ch, true).unwrap();
        let first = app.videos[0].id.clone();
        app.store
            .import_playlist("Road trip", &[(first, None)])
            .unwrap();
        app.reload_subscriptions();
        let shown = screen(&mut app, 140, 40);
        let row = |text: &str| shown.lines().position(|l| l.contains(text)).expect(text);
        // Under the playlists, right above the subscriptions they group.
        assert!(row("├─ Playlists (1) ") < row("├─ Groups (2) "), "{shown}");
        assert!(row("├─ Groups (2) ") < row("  Music  0"), "{shown}");
        assert!(row("  Music  0") < row("  Rust  1"), "by name");
        assert!(row("  Rust  1") < row("├─ Subscriptions (1) "));

        // F on a card: its channel's groups, Rust ticked.
        app.focus = Focus::Grid;
        app.on_key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE));
        let shown = screen(&mut app, 140, 40);
        assert!(shown.contains("Groups · BekBrace"), "{shown}");
        assert!(shown.contains("[ ] Music"), "{shown}");
        assert!(shown.contains("[✓] Rust"), "{shown}");
        assert!(shown.contains("+  New group"), "{shown}");
        // n: a name typed, above them.
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        for c in "Jazz".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let shown = screen(&mut app, 140, 40);
        assert!(shown.contains("New group Jazz▏"), "{shown}");
        for width in 1..140 {
            screen(&mut app, width, 40);
        }
        for height in 1..20 {
            screen(&mut app, 140, height);
        }
    }

    #[tokio::test]
    async fn with_audio_only_the_status_bar_says_enter_listens() {
        let mut app = app();
        app.status = None;
        with_feed(&mut app, 3);
        app.focus = Focus::Grid;
        app.settings.audio_only = true;
        let shown = screen(&mut app, 140, 40);
        let status = shown.lines().last().unwrap();
        assert!(
            status.starts_with(" Enter listen · : commands · i info"),
            "{status}"
        );
        assert!(!status.contains("a listen"));
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
        let progress_row = |app: &mut App| {
            let shown = screen(app, 140, 40);
            let row = shown
                .lines()
                .find(|l| l.starts_with("│ 0:10 "))
                .map(str::to_string);
            row.expect(&shown)
        };
        for (controllable, keys) in [
            (false, "1:40   ■ X  ⟳ A"),
            (true, "« ,  ⏸ Space  » .  § t  ■ X  ⟳ A"),
        ] {
            mpv_control(&mut app, controllable);
            let progress = progress_row(&mut app);
            assert!(progress.contains(keys), "{progress}");
        }
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
    async fn the_jukebox_shows_as_cards_or_as_a_list() {
        let mut app = app();
        app.icons = crate::icons::PLAIN;
        with_feed(&mut app, 6);
        app.show(View::Jukebox, false);
        app.status = None;
        assert!(screen(&mut app, 140, 45).contains("The Jukebox is empty"));

        app.show(View::Home, false);
        for i in [1, 3] {
            app.selected = i;
            app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        }
        app.status = None;
        let shown = screen(&mut app, 140, 45);
        assert!(shown.contains("≡  Jukebox  2"), "its count: {shown}");
        assert!(
            shown.contains(" ≡1") && shown.contains(" ≡2"),
            "the cards say their place"
        );

        app.show(View::Jukebox, false);
        let shown = screen(&mut app, 140, 45);
        assert!(shown.contains("Jukebox  2 videos"), "{shown}");
        assert!(shown.contains("Video vid00000004"));
        assert!(!shown.contains(" ≡1"), "no place on its own cards");
        assert!(app.grid.get().cols >= 2, "cards");
        let status = shown.lines().last().unwrap().to_string();
        assert!(status.contains("J K move"), "{status}");
        // The one playing from it is marked.
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, false);
        let shown = screen(&mut app, 140, 45);
        assert!(shown.contains("▶ Playing ·"), "{shown}");

        app.settings.jukebox_list = true;
        let shown = screen(&mut app, 140, 45);
        assert_eq!(app.grid.get().cols, 1, "a list");
        let row = |n: &str| {
            shown
                .lines()
                .find(|l| l.contains(n))
                .unwrap_or_else(|| panic!("{n}: {shown}"))
                .to_string()
        };
        assert!(row("Video vid00000004").contains("▌ ▶  Video vid00000004"));
        assert!(row("Video vid00000002").contains(" 2  Video vid00000002"));
        // The channel on the row under it, under the title.
        let under = shown
            .lines()
            .skip_while(|l| !l.contains("Video vid00000002"))
            .nth(1)
            .unwrap();
        let column = |line: &str, text: &str| line[..line.find(text).unwrap()].width();
        assert_eq!(
            column(under, "Channel"),
            column(&row("Video vid00000002"), "Video"),
            "{under}"
        );
        assert!(!shown.contains("Video vid00000005"), "only the Jukebox's");
        // Clearing it asks first, in a popup.
        app.confirm = Some(Confirm::ClearJukebox);
        let shown = screen(&mut app, 140, 45);
        assert!(shown.contains("Clear the Jukebox?"), "{shown}");
        assert!(shown.contains("All 2 videos in it are taken off"));
        assert!(shown.contains("y clear"));
        for width in 1..140 {
            screen(&mut app, width, 45);
        }
        for height in 1..12 {
            screen(&mut app, 140, height);
        }
        // So does taking a video off: a long title goes on more lines.
        let long = "A very long title that would otherwise run right across the whole \
                    screen and then some, ending here";
        let id = app.jukebox[0].id.clone();
        app.confirm = Some(Confirm::TakeOff(Kept::Jukebox, id, long.into()));
        let shown = screen(&mut app, 140, 45);
        assert!(shown.contains("Remove from the Jukebox?"), "{shown}");
        assert!(shown.contains("“A very long title"), "{shown}");
        assert!(shown.contains("ending here”"), "{shown}");
        assert!(shown.contains("y remove"));
        let line_of = |text: &str| shown.lines().position(|l| l.contains(text));
        assert!(
            line_of("“A very long title") < line_of("ending here”"),
            "{shown}"
        );
        app.confirm = Some(Confirm::Unsubscribe(
            ChannelId::parse(CH).unwrap(),
            "BekBrace".into(),
        ));
        let shown = screen(&mut app, 140, 45);
        assert!(shown.contains("Unsubscribe?"), "{shown}");
        assert!(shown.contains("From “BekBrace”"), "{shown}");
        for width in 1..140 {
            screen(&mut app, width, 45);
        }
        for height in 1..12 {
            screen(&mut app, 140, height);
        }
    }

    #[test]
    fn emoji_are_measured_as_the_terminal_draws_them() {
        // ⚔️ is two columns (⚔ alone is one), 👍🏽 and 👨‍👩‍👧 two each.
        let title = "DROPS✅⚔️[Deathmarch] 👍🏽 👨\u{200D}👩\u{200D}👧 Henry";
        // Whether `part` ends where a character of `title` ends.
        let whole = |part: &str| {
            part.is_empty()
                || title
                    .grapheme_indices(true)
                    .any(|(i, g)| i + g.len() == part.len())
        };
        for width in 0..=title.width() + 1 {
            let cut = fit(title, width);
            assert!(cut.width() <= width, "{width}: {cut}");
            assert!(whole(cut.trim_end_matches('…')), "{width}: {cut}");
            let end = tail(title, width);
            assert!(end.width() <= width, "{width}: {end}");
            if width >= 2 {
                for row in wrap(title, width) {
                    assert!(row.width() <= width, "{width}: {row}");
                }
            }
        }
        assert_eq!(fit("ab⚔️cd", 5), "ab⚔️…");
        assert_eq!(fit("ab⚔️cd", 4), "ab…", "not half of it");
        assert_eq!(tail("ab⚔️", 2), "⚔️");
        assert_eq!(wrap("⚔️⚔️⚔️", 4), ["⚔️⚔️", "⚔️"]);
        assert_eq!(wrap("⚔️⚔️", 1), ["⚔️", "⚔️"], "no empty row first");
    }

    #[test]
    fn an_emoji_with_fe0f_doesnt_push_the_rest_of_its_row() {
        let before = Buffer::with_lines(["xxxxxxxx"]);
        let mut after = Buffer::empty(before.area);
        after.set_string(0, 0, "a⚔️b", Style::new());
        pin_emoji_widths(&mut after);
        let sent: Vec<u16> = before.diff(&after).iter().map(|&(x, _, _)| x).collect();
        // Not the column the emoji covers: it would go out with no move
        // there first, a column right of where the emoji ends, and the `b`
        // after it.
        assert_eq!(sent, [0, 1, 3, 4, 5, 6, 7]);
        // Wide characters without it were never sent twice.
        let mut after = Buffer::empty(before.area);
        after.set_string(0, 0, "a日b", Style::new());
        pin_emoji_widths(&mut after);
        assert_eq!(after[(1, 0)].diff_option, CellDiffOption::None);
    }

    #[tokio::test]
    async fn emoji_in_titles_are_pinned_on_cards_and_in_the_list() {
        let mut app = app();
        with_feed(&mut app, 3);
        app.videos[0].title = "DROPS✅⚔️[Deathmarch] Henry Cavill returns!".into();
        for list in [false, true] {
            app.settings.list = list;
            let buffer = drawn(&mut app, 140, 40);
            let at = buffer
                .content
                .iter()
                .position(|cell| cell.symbol() == "⚔\u{FE0F}")
                .unwrap_or_else(|| panic!("list {list}"));
            let (x, y) = buffer.pos_of(at);
            assert_eq!(
                buffer[(x, y)].diff_option,
                CellDiffOption::ForcedWidth(NonZeroU16::new(2).unwrap()),
                "list {list}"
            );
            assert_eq!(buffer[(x + 2, y)].symbol(), "[", "list {list}");
        }
    }

    #[tokio::test]
    async fn every_view_can_be_a_list() {
        let mut app = app();
        with_feed(&mut app, 3);
        app.settings.list = true;
        let shown = screen(&mut app, 140, 30);
        assert_eq!(app.grid.get().cols, 1, "a list");
        // A video's title, and the row under it.
        let rows = |shown: &str, title: &str| -> (String, String) {
            let mut lines = shown.lines().skip_while(|l| !l.contains(title));
            let row = lines.next().unwrap_or_else(|| panic!("{shown}"));
            (row.to_string(), lines.next().unwrap().to_string())
        };
        let (row, under) = rows(&shown, "Video vid00000001");
        assert!(under.contains("Channel · 56 years ago"), "{under}");
        assert!(
            !row.contains("1  Video"),
            "numbered only in the Jukebox: {row}"
        );
        let (selected, under) = rows(&shown, "Video vid00000002");
        assert!(selected.contains("▌ Video vid00000002"), "{shown}");
        assert!(under.contains("▌ Channel"), "both rows marked: {under}");
        // The titles start under the view's name and the search box's edge.
        let column = |line: &str, text: &str| line[..line.find(text).unwrap()].width();
        let header = shown
            .lines()
            .find(|l| l.contains("Home  3 videos"))
            .unwrap();
        let top = shown.lines().next().unwrap();
        let home = column(header, "Home  3");
        assert_eq!(column(&row, "Video"), home, "{shown}");
        assert_eq!(home, column(top, "╭─") + 3, "{shown}");
        // A blank row above each video, the first under the view's name.
        let narrow = screen(&mut app, 60, 30);
        let lines: Vec<&str> = narrow.lines().collect();
        let blank = |at: usize| lines[at].trim_matches('│').trim().is_empty();
        for title in ["Video vid00000002", "Video vid00000001"] {
            let at = lines.iter().position(|l| l.contains(title)).unwrap();
            assert!(blank(at - 1) && !blank(at + 1), "{narrow}");
        }
        // Narrow, the second row still has room for them.
        let shown = screen(&mut app, 60, 30);
        assert!(shown.contains("Channel · 56 years ago"), "{shown}");
        // Watched halfway: the bar beside its title, as the player's.
        let id = app.videos[1].id.clone();
        app.progress.insert(id, (300.0, 600.0));
        let shown = screen(&mut app, 180, 30);
        let (row, under) = rows(&shown, "Video vid00000001");
        assert!(row.contains("5:00 ━━━━"), "{row}");
        assert!(row.contains("━━━━ 10:00 "), "{row}");
        assert!(!row.contains('●'), "{row}");
        assert!(under.contains("Channel"), "{under}");
        let other = shown
            .lines()
            .find(|l| l.contains("Video vid00000000"))
            .unwrap();
        assert!(!other.contains('━'), "only where it was watched: {other}");
        for width in 1..180 {
            screen(&mut app, width, 30);
        }
        for height in 1..12 {
            screen(&mut app, 140, height);
        }
    }

    /// Chapters at 0:00, 1:00 and 3:00 of the video playing.
    fn with_chapters(app: &mut App) {
        let playing = app.playing.as_mut().unwrap();
        playing.chapters = [(0.0, "Intro"), (60.0, "The middle part"), (180.0, "Outro")]
            .map(|(start, title)| crate::chapters::Chapter {
                start,
                title: title.into(),
            })
            .into();
    }

    #[tokio::test]
    async fn dearrows_title_shows_on_the_card_and_youtubes_in_the_popup() {
        let mut app = app();
        with_feed(&mut app, 2);
        app.status = None;
        app.settings.dearrow = true;
        let video = app.videos[0].clone();
        screen(&mut app, 140, 45);
        let asked = app.titles_wanted();
        assert!(asked.contains(&video.id), "cards on screen are asked about");
        crate::app::tests::dearrow_says(&mut app, &video.id, "A calmer title");
        let shown = screen(&mut app, 140, 45);
        assert!(shown.contains("A calmer title"), "{shown}");
        assert!(!shown.contains(&video.title), "{shown}");
        app.on_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let shown = screen(&mut app, 140, 45);
        let youtubes = format!("On YouTube: {}", video.title);
        assert!(shown.contains(&youtubes), "{shown}");
    }

    #[tokio::test]
    async fn the_settings_say_whats_blocked() {
        let mut app = app();
        app.status = None;
        app.on_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE));
        let shown = screen(&mut app, 140, 50);
        assert!(shown.contains("No channels or words blocked"), "{shown}");
        assert!(shown.contains("Better titles from DeArrow"), "{shown}");
        app.settings.blocked_channels = vec!["Someone".into()];
        app.settings.blocked_words = vec!["a".into(), "b".into()];
        let shown = screen(&mut app, 140, 50);
        assert!(shown.contains("Blocked: 1 channel and 2 words"), "{shown}");
        assert!(shown.contains("github.com/erictran308/pepper"), "{shown}");
    }

    #[tokio::test]
    async fn the_player_bar_says_the_chapter_and_the_quality() {
        let mut app = app();
        app.icons = crate::icons::PLAIN;
        with_feed(&mut app, 3);
        app.status = None;
        app.show_playing(app.videos[0].clone(), 75.0, 600.0, false);
        with_chapters(&mut app);
        app.playing.as_mut().unwrap().quality = Some("1080p60".into());
        // The keys after the quality, then the volume: Windows has only X,
        // A and r.
        for (controllable, after) in [
            (false, "10:00  1080p60  ■ X  ⟳ A  Loop r │"),
            (true, "10:00  1080p60  « ,"),
        ] {
            mpv_control(&mut app, controllable);
            let shown = screen(&mut app, 140, 40);
            let progress = shown
                .lines()
                .find(|l| l.starts_with("│ 1:15 "))
                .expect(&shown);
            assert!(progress.contains("1:15 · The middle part ━"), "{progress}");
            assert!(progress.contains(after), "{progress}");
            assert_eq!(
                progress.contains("⟳ A  Loop r  Vol 100% -+ │"),
                controllable,
                "{progress}"
            );
        }
        // Repeat one has its own icon.
        app.settings.repeat = Repeat::One;
        let shown = screen(&mut app, 140, 40);
        assert!(shown.contains("⟳ A  Loop1 r  Vol"), "{shown}");
        app.settings.repeat = Repeat::Off;
        // Narrow: the keys and the quality go, then the chapter.
        let shown = screen(&mut app, 50, 40);
        assert!(!shown.contains("1080p60") && shown.contains("The middle"));
        let shown = screen(&mut app, 30, 40);
        assert!(!shown.contains("The middle"), "{shown}");
        for width in 1..140 {
            screen(&mut app, width, 40);
        }
    }

    #[tokio::test]
    async fn the_chapters_popup_lists_them_and_marks_the_one_playing() {
        let mut app = app();
        app.icons = crate::icons::PLAIN;
        with_feed(&mut app, 3);
        app.status = None;
        app.show_playing(app.videos[0].clone(), 75.0, 600.0, false);
        with_chapters(&mut app);
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert!(app.popup_open());
        let shown = screen(&mut app, 120, 40);
        assert!(shown.contains(" Chapters · 3 "), "{shown}");
        assert!(shown.contains("type a time: 1:23 · 90 · +30 · 50%"));
        let line = |text: &str| {
            shown
                .lines()
                .find(|l| l.contains(text))
                .unwrap_or_else(|| panic!("{text}: {shown}"))
                .to_string()
        };
        assert!(
            line("Intro").contains("│   0:00  Intro"),
            "{}",
            line("Intro")
        );
        assert!(
            line("The middle part").contains("│▌▶ 1:00  The middle part"),
            "selected, and playing: {}",
            line("The middle part")
        );
        assert!(line("Outro").contains("3:00  Outro"));
        assert!(shown.contains("Enter go · j k choose · Esc close"));

        // A time typed shows, and Enter goes there instead.
        for c in "2:30".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let shown = screen(&mut app, 120, 40);
        assert!(shown.contains("Go to 2:30▏"), "{shown}");
        assert!(shown.contains("Enter go to the time"));

        // A long list scrolls to keep the cursor in view; small windows
        // don't break it.
        let playing = app.playing.as_mut().unwrap();
        playing.chapters = (0..60)
            .map(|i| crate::chapters::Chapter {
                start: f64::from(i) * 10.0,
                title: format!("Part {i}"),
            })
            .collect();
        app.chapter_menu.as_mut().unwrap().selected = 59;
        let shown = screen(&mut app, 120, 30);
        assert!(
            shown.contains("Part 59") && !shown.contains("Part 1 "),
            "{shown}"
        );
        app.playing.as_mut().unwrap().chapters.clear();
        let shown = screen(&mut app, 120, 30);
        assert!(shown.contains(" Go to a time ") && shown.contains("no chapters"));
        for width in 1..120 {
            screen(&mut app, width, 30);
        }
        for height in 1..12 {
            screen(&mut app, 120, height);
        }
    }

    #[tokio::test]
    async fn the_settings_show_the_video_and_sound_quality_in_use() {
        let mut app = app();
        app.settings.max_height = 900;
        app.settings.audio_quality = AudioQuality::Low;
        let mut menu = HelpMenu::new(HelpTab::Settings);
        menu.selected = HelpMenu::VIDEO_QUALITY + 1;
        app.help = Some(menu);
        let shown = screen(&mut app, 120, 60);
        let line = |text: &str| {
            shown
                .lines()
                .find(|l| l.contains(text))
                .unwrap_or_else(|| panic!("{text}: {shown}"))
                .to_string()
        };
        assert!(shown.contains(" Video quality") && shown.contains(" Sound quality"));
        assert!(line("Up to 720p").contains("●"), "900 plays as 720p");
        assert!(line("Up to 1080p (HD)").contains("○"));
        assert!(line("Up to 1440p").contains("▌ ○"), "the cursor");
        assert!(line("Low: about 70 kbps").contains("●"));
        assert!(line("High: about 130 kbps").contains("○"));
        assert!(shown.contains("Enter use"), "a choice, not on/off");
    }

    #[tokio::test]
    async fn the_command_line_lists_what_completes_it_under_the_word() {
        let mut app = app();
        with_feed(&mut app, 3);
        let typed = |app: &mut App, text: &str| {
            for c in text.chars() {
                app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
        };
        typed(&mut app, ":");
        let shown = screen(&mut app, 140, 40);
        let lines: Vec<&str> = shown.lines().collect();
        assert!(lines[39].starts_with(":▏"), "{}", lines[39]);
        assert!(lines[39].contains("Tab complete · Enter run · Esc close"));
        assert!(shown.contains(" Commands "), "{shown}");
        let row = |shown: &str, text: &str| {
            shown
                .lines()
                .find(|l| l.contains(text))
                .unwrap_or_else(|| panic!("{text}: {shown}"))
                .to_string()
        };
        let quality = row(&shown, "quality <height>");
        assert!(quality.contains("│▌  quality <height>"), "lit: {quality}");
        assert!(quality.contains("Video quality: up to 360p"), "{quality}");
        let autoplay = row(&shown, "autoplay [on|off]");
        assert!(
            autoplay.contains("when one ends") && autoplay.contains("  A "),
            "its key"
        );

        // The argument's values, under it, the one in use marked.
        typed(&mut app, "quality ");
        let shown = screen(&mut app, 140, 40);
        assert!(shown.lines().last().unwrap().starts_with(":quality ▏"));
        let column = |line: &str, text: &str| line[..line.find(text).unwrap()].chars().count();
        let in_use = row(&shown, "Up to 1080p (HD)");
        assert!(in_use.contains("● 1080"), "{in_use}");
        assert_eq!(
            column(&in_use, "1080"),
            ":quality ".len(),
            "under the word: {in_use}"
        );
        // Why a line didn't run shows in red on its right.
        typed(&mut app, "999");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let buffer = drawn(&mut app, 140, 40);
        let status = screen(&mut app, 140, 40)
            .lines()
            .last()
            .unwrap()
            .to_string();
        assert!(status.contains("quality takes 2160, 1440"), "{status}");
        let x = column(&status, "quality takes") as u16;
        assert_eq!(buffer[(x, 39)].fg, app.colors.red, "{status}");
        assert!(app.popup_open());
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

        // Both segments are on the seek bar, in sponsors' green.
        let buffer = drawn(&mut app, 140, 40);
        let green = (0..140)
            .filter(|&x| buffer[(x, y as u16 + 1)].fg == app.colors.segment("sponsor"))
            .count();
        assert!(green >= 2, "{green}");
    }

    #[test]
    fn the_seek_bar_shows_sponsorblock_segments_in_their_colors() {
        let c = Colors::named(theme::DEFAULT);
        let green = c.segment("sponsor");
        let cells = |spans: Vec<Span<'static>>| -> Vec<(char, Color)> {
            spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |ch| (ch, s.style.fg.unwrap())))
                .collect()
        };
        // Eleven cells, a tenth of the video each: a fifth played, and a
        // sponsor from half way to 70%.
        let bar = cells(seek_bar(&c, 0.2, &[(0.5, 0.7, green)], 11));
        let text: String = bar.iter().map(|(ch, _)| ch).collect();
        assert_eq!(text, "━━●──━━━───");
        let colors: Vec<Color> = bar.iter().map(|&(_, color)| color).collect();
        let (red, gray) = (c.red, c.border);
        assert_eq!(
            colors,
            [
                red, red, red, gray, gray, green, green, green, gray, gray, gray
            ]
        );
        // A short one takes a cell; the dot stays over one it's in.
        let bar = cells(seek_bar(&c, 0.9, &[(0.91, 0.92, green)], 11));
        assert_eq!(bar[9], ('●', red));
        let bar = cells(seek_bar(&c, 0.0, &[(0.91, 0.92, green)], 11));
        assert_eq!(bar[9], ('━', green));
        // No room: nothing.
        assert!(seek_bar(&c, 0.5, &[(0.1, 0.2, green)], 0).is_empty());
    }
}
