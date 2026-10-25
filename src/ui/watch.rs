//! The watch page (`App.watch`): one video, laid out as YouTube's watch
//! page. Its picture is at the top left: inside Pepper while it plays there
//! (`crate::pepper`), its thumbnail anywhere else. Under it are its title,
//! channel and views, then its description or comments. Up next is on the
//! right, or in the description's place on a narrow screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui_image::Image;
use unicode_width::UnicodeWidthStr;

use super::{border, bordered, cells, fit, hint_spans, video_info, wrap};
use crate::app::{App, InfoTab, Playing, WatchPane};
use crate::images::{Key, Subject};
use crate::pepper;
use crate::theme::{self, Colors};
use crate::video::{self, Video};

/// From this width up next has a column of its own.
const UP_NEXT_BESIDE: u16 = 100;
/// Rows the picture leaves under it at least, for the title and the start
/// of the description.
const ROWS_UNDER: u16 = 8;
/// And at least this part of the page's height (a third): on a tall page
/// the description and comments get more than a line or two.
const UNDER_PART: u16 = 3;
/// Returns where the picture says it waits, for the light across it.
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) -> Option<Rect> {
    let watch = app.watch.as_ref()?;
    let c = app.colors.clone();
    let pane = watch.pane;
    let video = watch.info.video.clone();
    let beside = area.width >= UP_NEXT_BESIDE;
    let side_width = if beside {
        (area.width / 3).clamp(32, 60)
    } else {
        0
    };
    let [main, side] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(side_width)]).areas(area);

    let hint = match (pane, watch.info.tab) {
        (WatchPane::Info, InfoTab::Description) => {
            " `Tab` comments · `Ctrl-l` up next · `Esc` back "
        }
        (WatchPane::Info, InfoTab::Comments) => {
            " `Tab` description · `Ctrl-l` up next · `Esc` back "
        }
        (WatchPane::UpNext, InfoTab::Description) => {
            " `Enter` play · `Ctrl-h` description · `Esc` back "
        }
        (WatchPane::UpNext, InfoTab::Comments) => " `Enter` play · `Ctrl-h` comments · `Esc` back ",
    };
    let block = bordered()
        .border_style(border(pane == WatchPane::Info || !beside, &c))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(app.icons.play, Style::new().fg(c.accent)),
            Span::raw(" Watch "),
        ]))
        .title_bottom(Line::from(hint_spans(hint, &c)).right_aligned());
    let inner = block.inner(main).inner(Margin::new(1, 0));
    frame.render_widget(block, main);

    // The picture, as wide as the column, as long as rows are left under it.
    let font = app.images.font_size();
    let under = ROWS_UNDER.max(inner.height / UNDER_PART);
    let max_rows = inner.height.saturating_sub(under).max(3);
    let size = pepper::video_area(inner, (font.width, font.height), max_rows);
    // In the middle of the column, at its top.
    let top = Rect {
        height: size.height,
        ..inner
    };
    let picture = pepper::centered(top, size);
    // A row left free under it, then the rest. What the picture says goes
    // in that row where the terminal would paint over it.
    let gap = picture.bottom();
    let free_under = (gap < inner.bottom()).then_some(gap);
    let waiting = draw_picture(frame, app, &c, &video, picture, picture, free_under);

    let under = Rect {
        y: gap + 1,
        height: inner.bottom().saturating_sub(gap + 1),
        ..inner
    };
    if under.height > 0 {
        if beside || pane == WatchPane::Info {
            info(frame, app, &c, under, pane == WatchPane::Info);
        } else {
            up_next(frame, app, &c, under, true);
        }
    }

    if beside {
        let block = bordered()
            .border_style(border(pane == WatchPane::UpNext, &c))
            .title(" Up next ");
        let inner = block.inner(side).inner(Margin::new(1, 0));
        frame.render_widget(block, side);
        up_next(frame, app, &c, inner, pane == WatchPane::UpNext);
    }
    waiting
}

/// A video's picture, in `area` (the watch page's, the picture-in-picture
/// box or the whole tab), its thumbnail filling `thumb`. While it plays
/// inside Pepper, Pepper puts the picture over `area`; until the first
/// frame, and while a popup takes the picture away, the thumbnail shows
/// there instead of a black box, saying what's going on: yt-dlp looking it
/// up, the player opening its streams, or that it couldn't be played.
/// Where the terminal paints pictures over text (sixel, iTerm2's), what it
/// says goes in `free_under`, the row under it that's free, if the caller
/// has one (the watch page; not Pepper's corner box or full screen).
/// Returns where it says it waits (loads or buffers), for the light
/// across it.
pub(super) fn draw_picture(
    frame: &mut Frame,
    app: &mut App,
    c: &Colors,
    video: &Video,
    area: Rect,
    thumb: Rect,
    free_under: Option<u16>,
) -> Option<Rect> {
    if area.is_empty() {
        return None;
    }
    let playing = app.playing.as_ref().filter(|p| p.video.id == video.id);
    let inline = playing.is_some_and(|p| p.pepper.is_some());
    let started = playing.is_some_and(Playing::started);
    if inline {
        frame.render_widget(Block::new().style(Style::new().bg(theme::BLACK)), area);
        app.video_box.set(Some(area));
        if started && app.picture_shown() {
            return None;
        }
    }
    // yt-dlp looking it up: not another quality of it, which plays on.
    let doing = if playing.is_none()
        && app
            .resolving
            .as_ref()
            .is_some_and(|r| r.video.id == video.id)
    {
        Some((app.icons.loading, "Loading…", theme::WAITING, true))
    } else if inline && !started {
        Some((app.icons.loading, "Buffering…", theme::WAITING, true))
    } else if app.failed.contains_key(&video.id) {
        Some((app.icons.failed, "Can't play", c.red, false))
    } else {
        None
    };
    let covered = app.popup_open() && app.images.paints_over();
    let key = Key {
        subject: Subject::Thumbnail(video.id.clone()),
        cols: thumb.width,
        rows: thumb.height,
    };
    let drawn = !covered
        && match app.images.get(&key, &video.thumbnail_urls()) {
            Some(image) => {
                frame.render_widget(Image::new(image), thumb);
                true
            }
            None => false,
        };
    if drawn {
        app.images.place(&key.subject, thumb, thumb);
    } else {
        frame.render_widget(Block::new().style(Style::new().bg(c.selected)), thumb);
    }
    let middle = |width: u16| Rect {
        x: thumb.x + thumb.width.saturating_sub(width) / 2,
        y: thumb.y + thumb.height / 2,
        width: width.min(thumb.width),
        height: 1,
    };
    // What's going on, in the middle, as the cards' length badge: over the
    // thumbnail, or, for terminals that would paint the image over it, in
    // the free row under it, if there's one.
    let mut waiting = None;
    if let Some((icon, words, color, waits)) = doing {
        let text = fit(&format!(" {icon} {words} "), usize::from(thumb.width));
        let mut place = middle(cells(&text) as u16);
        // Where the terminal paints pictures over text, under it from the
        // start, so it doesn't move once the picture's there.
        let row = match (app.images.paints_over(), free_under) {
            (true, Some(free)) => Some(free),
            (true, None) if drawn => None,
            _ => Some(place.y),
        };
        if let Some(row) = row {
            place.y = row;
            frame.render_widget(
                Paragraph::new(Span::styled(text, Style::new().fg(color).bg(theme::BADGE))),
                place,
            );
            waiting = waits.then_some(place);
        }
    } else if !drawn {
        let mark = if app.images.is_broken(&key) {
            "✕"
        } else {
            app.icons.play
        };
        frame.render_widget(
            Paragraph::new(Span::styled(mark, Style::new().fg(c.dim))),
            middle(1),
        );
    }
    waiting
}

/// The title, channel and views, then the description or the comments, as
/// in the `i` popup, with the tabs between.
fn info(frame: &mut Frame, app: &mut App, c: &Colors, area: Rect, active: bool) {
    let title = app
        .watch
        .as_ref()
        .map(|w| app.title(&w.info.video).to_string())
        .unwrap_or_default();
    let Some(watch) = app.watch.as_mut() else {
        return;
    };
    let tab = |label: &'static str, on: bool| {
        if on && active {
            Span::styled(
                label,
                Style::new()
                    .fg(c.bg)
                    .bg(c.accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else if on {
            Span::styled(label, Style::new().fg(c.text).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(label, Style::new().fg(c.dim))
        }
    };
    let tabs = Line::from(vec![
        tab(" Description ", watch.info.tab == InfoTab::Description),
        Span::raw(" "),
        tab(" Comments ", watch.info.tab == InfoTab::Comments),
    ]);
    video_info(
        frame,
        c,
        &app.comments,
        &mut watch.info,
        &title,
        area,
        Some(tabs),
        active,
    );
}

/// What plays after the video playing: the rest of the Jukebox, then up
/// next, each with its thumbnail where pictures can be drawn.
fn up_next(frame: &mut Frame, app: &mut App, c: &Colors, area: Rect, active: bool) {
    let list: Vec<(Video, bool)> = app
        .up_next_list()
        .into_iter()
        .map(|(video, from_jukebox)| (video.clone(), from_jukebox))
        .collect();
    if list.is_empty() {
        let lines = vec![
            Line::styled("Nothing up next.", Style::new().fg(c.subtext)),
            Line::default(),
        ]
        .into_iter()
        .chain(
            wrap(
                "A video played from a list plays the rest of it after (with autoplay, A); e puts one in the Jukebox, which plays first.",
                usize::from(area.width),
            )
            .into_iter()
            .map(|row| Line::styled(row, Style::new().fg(c.dim))),
        )
        .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }

    // Each video: its thumbnail, with its title and channel beside it.
    let photos = app.images.draws_photos();
    let font = app.images.font_size();
    let thumb = if photos {
        let cols = (area.width / 3).clamp(10, 22);
        pepper::video_area(
            Rect::new(0, 0, cols, area.height),
            (font.width, font.height),
            6,
        )
    } else {
        Rect::default()
    };
    // The videos that fit whole (the last needs no blank row after it), then
    // the top of the next, cut off at the bottom, as the grid's next row.
    let rows_each = thumb.height.max(3) + 1;
    let whole = usize::from(((area.height + 1) / rows_each).max(1));
    let Some(watch) = app.watch.as_ref() else {
        return;
    };
    let selected = watch.next_selected.min(list.len() - 1);
    let mut scroll = watch.next_scroll.get().min(list.len() - 1);
    if selected < scroll {
        scroll = selected;
    } else if selected >= scroll + whole {
        scroll = selected + 1 - whole;
    }
    watch.next_scroll.set(scroll);

    for (row, (video, from_jukebox)) in list.iter().enumerate().skip(scroll).take(whole + 1) {
        let y = area.y + (row - scroll) as u16 * rows_each;
        let item = Rect {
            y,
            height: rows_each
                .saturating_sub(1)
                .min(area.bottom().saturating_sub(y)),
            ..area
        };
        if item.is_empty() {
            break;
        }
        let picked = active && row == selected;
        if picked {
            frame.render_widget(Block::new().style(Style::new().bg(c.selected)), item);
        }
        let text_x = if photos {
            let full = Rect {
                x: item.x,
                y: item.y,
                width: thumb.width.min(item.width),
                height: thumb.height,
            };
            let place = full.intersection(item);
            let key = Key {
                subject: Subject::Thumbnail(video.id.clone()),
                cols: full.width,
                rows: full.height,
            };
            // Kitty's images can be cut; sixel and iTerm2 ones can't.
            let cuttable = !app.images.paints_over();
            let covered = app.popup_open() && app.images.paints_over();
            let drawn = match app.images.get(&key, &video.thumbnail_urls()) {
                Some(image) if !covered && place == full => {
                    frame.render_widget(Image::new(image), place);
                    true
                }
                Some(image) if !covered && cuttable => {
                    frame.render_widget(Image::new(image).allow_clipping(true), place);
                    true
                }
                _ => false,
            };
            if drawn {
                app.images.place(&key.subject, full, place);
            } else {
                frame.render_widget(Block::new().style(Style::new().bg(c.panel)), place);
            }
            item.x + place.width + 1
        } else {
            item.x
        };
        let text = Rect {
            x: text_x,
            width: item.right().saturating_sub(text_x),
            ..item
        };
        let columns = usize::from(text.width);
        let mut lines: Vec<Line> = wrap(app.title(video), columns)
            .into_iter()
            .take(2)
            .map(|row| Line::styled(row, Style::new().fg(c.text).add_modifier(Modifier::BOLD)))
            .collect();
        let mut about = vec![video.channel.clone()];
        about.extend(video.duration.map(video::duration));
        about.retain(|s| !s.is_empty());
        let about = about.join(" · ");
        let mark = if *from_jukebox { " · Jukebox" } else { "" };
        let failed = if app.failed.contains_key(&video.id) {
            format!(" {}", app.icons.failed)
        } else {
            String::new()
        };
        lines.push(Line::from(vec![
            Span::styled(
                fit(
                    &about,
                    columns.saturating_sub(mark.width() + failed.width()),
                ),
                Style::new().fg(c.dim),
            ),
            Span::styled(mark, Style::new().fg(c.accent)),
            Span::styled(failed, Style::new().fg(c.red).add_modifier(Modifier::BOLD)),
        ]));
        frame.render_widget(Paragraph::new(lines), text);
    }
}
