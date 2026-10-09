//! The app's state and everything that changes it: keys, answers from
//! yt-dlp and the feeds, images, and mpv. All state lives in [`App`];
//! nothing else mutates it.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::feed::{self, Feed};
use crate::icons::Icons;
use crate::ids::{ChannelId, VideoId};
use crate::images::{ImageEvent, Images};
use crate::player::{Player, PlayerEvent, PlayerEventKind, Start};
use crate::settings::Settings;
use crate::store::{Channel, Store};
use crate::theme::{self, Colors};
use crate::tools::Tools;
use crate::video::{self, Video};
use crate::ytdlp::{ChannelPage, MAX_QUERY, Quality, Streams, Tab, YtDlp};
use crate::{takeout, text};

/// Rows Ctrl-d and Ctrl-u move in the sidebar; PgDn and PgUp move twice
/// as many.
const SIDEBAR_PAGE: usize = 10;

/// What the demo says when asked to fetch or play something.
const DEMO: &str =
    "This is the demo: made-up videos, nothing to fetch or play. Run tuitube without --demo";

/// How often tuitube looks for channels whose kept videos are older than
/// `refresh_hours`; only those are fetched.
const CHECK_EVERY: Duration = Duration::from_secs(30 * 60);
/// A channel's photo is looked up again after this long.
const AVATAR_EVERY: i64 = 30 * 24 * 3600;
/// Channel pages looked up in the background, at most, per session.
const MAX_CHANNEL_LOOKUPS: usize = 800;
/// Videos asked for per background look at a channel's tab: the cost is
/// the same as for one.
const CHANNEL_LOOKUP: usize = 30;
/// A live stream or premiere on screen is checked again this often, so it
/// gets its length once it's over.
const LIVE_EVERY: i64 = 10 * 60;
/// Videos a view lists, at most.
const MAX_LISTED: usize = 500;
/// Search results asked for at first, and each time the end is reached.
const SEARCH_PAGE: usize = 30;
const MAX_SEARCH: usize = 150;
/// How long a status message stays.
const STATUS_FOR: Duration = Duration::from_secs(6);
/// A video carries on from where you stopped if that's past this many
/// seconds from the start and from the end.
const RESUME_AFTER: f64 = 15.0;
/// Where you are in a video is saved this often while it plays.
const SAVE_EVERY: Duration = Duration::from_secs(10);

pub enum AppEvent {
    Feed {
        channel: ChannelId,
        result: Result<Feed>,
    },
    Search {
        request: u64,
        count: usize,
        result: Result<Vec<Video>>,
    },
    Channel {
        request: u64,
        result: Result<ChannelPage>,
    },
    ChannelInfo {
        id: ChannelId,
        tab: Tab,
        result: Result<ChannelPage>,
    },
    Resolved {
        request: u64,
        video: Video,
        audio_only: bool,
        result: Result<Streams>,
    },
    Player(PlayerEvent),
    Image(ImageEvent),
}

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Home,
    Shorts,
    Search(String),
    WatchLater,
    History,
    Channel(ChannelId, String),
}

impl View {
    pub fn title(&self) -> String {
        match self {
            View::Home => "Home".into(),
            View::Shorts => "Shorts".into(),
            View::Search(q) => format!("Results for “{q}”"),
            View::WatchLater => "Watch later".into(),
            View::History => "History".into(),
            View::Channel(_, name) => name.clone(),
        }
    }
}

/// A row of the sidebar that can be selected.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Entry {
    Home,
    Shorts,
    Search,
    WatchLater,
    History,
    /// A subscription, by its place in [`App::subscriptions`].
    Channel(usize),
}

pub const MENU: [Entry; 5] = [
    Entry::Home,
    Entry::Shorts,
    Entry::Search,
    Entry::WatchLater,
    Entry::History,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Focus {
    Sidebar,
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PromptKind {
    Search,
    Import,
}

/// A line being typed: a search, or the path of a file to import.
#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
}

pub struct Status {
    pub text: String,
    pub error: bool,
    at: Instant,
}

/// What's playing.
pub struct Playing {
    player: Player,
    pub video: Video,
    pub position: f64,
    pub duration: Option<f64>,
    pub paused: bool,
    pub audio_only: bool,
    saved: Instant,
}

impl Playing {
    pub fn controllable(&self) -> bool {
        self.player.controllable()
    }
}

/// The grid's shape in the last frame, for moving by rows and pages.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridShape {
    pub cols: usize,
    pub rows: usize,
}

pub struct App {
    pub store: Store,
    pub settings: Settings,
    settings_path: Option<PathBuf>,
    pub colors: Colors,
    pub icons: Icons,
    pub tools: Tools,
    yt: Option<YtDlp>,
    http: reqwest::Client,
    tx: UnboundedSender<AppEvent>,
    pub images: Images,

    pub subscriptions: Vec<Channel>,
    /// Channel photo URLs known this session; `None` while looked up or if
    /// the channel has none.
    avatars: HashMap<ChannelId, Option<String>>,
    channel_lookups: usize,
    /// When this session last looked at each channel's tab in the
    /// background (for photos and lengths), and which are being looked at.
    checked: HashMap<(ChannelId, Tab), Option<i64>>,
    in_flight: HashSet<ChannelId>,
    /// Background answers changed stored videos: reload after this batch.
    needs_reload: bool,

    pub view: View,
    pub videos: Vec<Video>,
    search_results: Vec<Video>,
    search_count: usize,
    /// Where you stopped in videos you watched here: (position, length).
    pub progress: HashMap<VideoId, (f64, f64)>,
    pub watch_later: HashSet<VideoId>,
    pub selected: usize,
    /// The first card row on screen.
    pub scroll: usize,
    pub grid: Cell<GridShape>,
    /// Views to go back to (Backspace), with what was selected.
    back: Vec<(View, usize)>,

    pub sidebar_selected: usize,
    pub sidebar_scroll: Cell<usize>,
    pub focus: Focus,
    pub prompt: Option<Prompt>,
    pub help: bool,

    /// What the grid is waiting for, said in its header.
    pub loading: Option<String>,
    /// Feeds fetched and to fetch, while they're being fetched.
    pub refreshing: Option<(usize, usize)>,
    /// When the last fetch of feeds ended, so failed ones wait for the next
    /// round instead of being retried at once.
    refreshed: Option<Instant>,
    feed_failures: usize,
    pub status: Option<Status>,
    pub playing: Option<Playing>,
    /// The video being looked up to play.
    pub resolving: Option<(u64, Video)>,
    last_unsubscribed: Option<(ChannelId, String)>,

    /// Numbers each request whose answer changes the view, so a late answer
    /// to an older one is dropped.
    request: u64,
    plays: u64,
    pending_g: bool,
    pub quit: bool,
    /// `--demo`: made-up videos; nothing is fetched or played.
    pub demo: bool,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        settings: Settings,
        settings_path: Option<PathBuf>,
        tools: Tools,
        yt: Option<YtDlp>,
        http: reqwest::Client,
        tx: UnboundedSender<AppEvent>,
        images: Images,
    ) -> Self {
        let colors = Colors::named(&settings.theme);
        let icons = Icons::pick(settings.icon_mode(), |name| std::env::var(name).ok());
        let mut app = Self {
            store,
            settings,
            settings_path,
            colors,
            icons,
            tools,
            yt,
            http,
            tx,
            images,
            subscriptions: Vec::new(),
            avatars: HashMap::new(),
            channel_lookups: 0,
            checked: HashMap::new(),
            in_flight: HashSet::new(),
            needs_reload: false,
            view: View::Home,
            videos: Vec::new(),
            search_results: Vec::new(),
            search_count: SEARCH_PAGE,
            progress: HashMap::new(),
            watch_later: HashSet::new(),
            selected: 0,
            scroll: 0,
            grid: Cell::new(GridShape { cols: 1, rows: 1 }),
            back: Vec::new(),
            sidebar_selected: 0,
            sidebar_scroll: Cell::new(0),
            focus: Focus::Grid,
            prompt: None,
            help: false,
            loading: None,
            refreshing: None,
            refreshed: None,
            feed_failures: 0,
            status: None,
            playing: None,
            resolving: None,
            last_unsubscribed: None,
            request: 0,
            plays: 0,
            pending_g: false,
            quit: false,
            demo: false,
        };
        app.reload_subscriptions();
        app.reload();
        if let Some(missing) = app.tools.missing() {
            app.error(missing);
        }
        app
    }

    pub async fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        mut rx: UnboundedReceiver<AppEvent>,
    ) -> Result<()> {
        let mut keys = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        self.refresh_feeds(false);
        loop {
            terminal.draw(|frame| crate::ui::draw(frame, self))?;
            self.images.fetch();
            if self.quit {
                break;
            }
            tokio::select! {
                event = keys.next() => match event {
                    Some(Ok(event)) => self.on_terminal(event),
                    Some(Err(_)) | None => self.quit = true,
                },
                Some(event) = rx.recv() => {
                    self.on_event(event);
                    while let Ok(event) = rx.try_recv() {
                        self.on_event(event);
                    }
                    if std::mem::take(&mut self.needs_reload) {
                        self.reload();
                    }
                }
                _ = tick.tick() => self.on_tick(),
            }
        }
        self.stop_playing();
        Ok(())
    }

    // Messages.

    pub fn info(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
            at: Instant::now(),
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
            at: Instant::now(),
        });
    }

    fn on_tick(&mut self) {
        if self
            .status
            .as_ref()
            .is_some_and(|s| s.at.elapsed() > STATUS_FOR)
        {
            self.status = None;
        }
        let due = self.refreshed.is_none_or(|at| at.elapsed() > CHECK_EVERY);
        if self.refreshing.is_none() && due {
            self.refresh_feeds(false);
        }
    }

    // Lists.

    fn reload_subscriptions(&mut self) {
        self.subscriptions = self.store.subscriptions().unwrap_or_default();
        for channel in &self.subscriptions {
            let fresh = channel
                .avatar_checked
                .is_some_and(|at| now() - at < AVATAR_EVERY);
            if fresh {
                self.avatars
                    .insert(channel.id.clone(), channel.avatar.clone());
            }
        }
        let entries = self.sidebar_len();
        self.sidebar_selected = self.sidebar_selected.min(entries.saturating_sub(1));
    }

    /// Reloads the view's videos from what's stored, keeping the selected
    /// video selected.
    pub fn reload(&mut self) {
        let keep = self.videos.get(self.selected).map(|v| v.id.clone());
        let videos = match &self.view {
            View::Home => self.store.feed(false, MAX_LISTED),
            View::Shorts => self.store.feed(true, MAX_LISTED),
            View::Search(_) => Ok(self.search_results.clone()),
            View::WatchLater => self.store.watch_later(),
            View::History => self.store.history(MAX_LISTED),
            View::Channel(id, _) => self.store.channel_videos(id, MAX_LISTED),
        };
        self.videos = videos.unwrap_or_default();
        self.watch_later = self
            .store
            .watch_later()
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.id)
            .collect();
        self.progress = self.store.all_progress().unwrap_or_default();
        self.selected = keep
            .and_then(|id| self.videos.iter().position(|v| v.id == id))
            .unwrap_or(self.selected)
            .min(self.videos.len().saturating_sub(1));
    }

    /// Shows `view`. `fetch` also asks YouTube for it (a channel's newest
    /// videos); otherwise only what's stored is shown.
    pub fn show(&mut self, view: View, fetch: bool) {
        if view != self.view {
            self.back.push((self.view.clone(), self.selected));
            if self.back.len() > 50 {
                self.back.remove(0);
            }
            self.view = view;
            self.selected = 0;
            self.scroll = 0;
            self.loading = None;
            self.request += 1;
        }
        self.reload();
        // A channel's newest videos are fetched when the kept ones are
        // older than `refresh_hours`; R fetches them anyway.
        if fetch && let View::Channel(id, _) = &self.view {
            let id = id.clone();
            let kept = self
                .tab_checked(&id, Tab::Videos)
                .is_some_and(|at| now() - at < self.settings.refresh_every());
            if !kept {
                self.fetch_channel(id);
            }
        }
    }

    fn go_back(&mut self) {
        if let Some((view, selected)) = self.back.pop() {
            self.view = view;
            self.loading = None;
            self.request += 1;
            self.videos.clear();
            self.reload();
            self.selected = selected.min(self.videos.len().saturating_sub(1));
            self.sync_sidebar();
        }
    }

    /// Selects the sidebar row of the view shown.
    fn sync_sidebar(&mut self) {
        let entry = match &self.view {
            View::Home => Some(Entry::Home),
            View::Shorts => Some(Entry::Shorts),
            View::Search(_) => Some(Entry::Search),
            View::WatchLater => Some(Entry::WatchLater),
            View::History => Some(Entry::History),
            View::Channel(id, _) => self
                .subscriptions
                .iter()
                .position(|c| c.id == *id)
                .map(Entry::Channel),
        };
        if let Some(i) = entry.and_then(|e| (0..self.sidebar_len()).find(|&i| self.entry(i) == e)) {
            self.sidebar_selected = i;
        }
    }

    pub fn sidebar_len(&self) -> usize {
        MENU.len() + self.subscriptions.len()
    }

    pub fn entry(&self, i: usize) -> Entry {
        MENU.get(i)
            .copied()
            .unwrap_or_else(|| Entry::Channel(i - MENU.len()))
    }

    /// The view a sidebar row shows.
    fn entry_view(&self, entry: Entry) -> Option<View> {
        Some(match entry {
            Entry::Home => View::Home,
            Entry::Shorts => View::Shorts,
            Entry::Search => View::Search(self.last_query()),
            Entry::WatchLater => View::WatchLater,
            Entry::History => View::History,
            Entry::Channel(i) => {
                let c = self.subscriptions.get(i)?;
                View::Channel(c.id.clone(), c.title.clone())
            }
        })
    }

    fn last_query(&self) -> String {
        match &self.view {
            View::Search(q) => q.clone(),
            _ => self
                .back
                .iter()
                .rev()
                .find_map(|(v, _)| match v {
                    View::Search(q) => Some(q.clone()),
                    _ => None,
                })
                .unwrap_or_default(),
        }
    }

    pub fn selected_video(&self) -> Option<&Video> {
        self.videos.get(self.selected)
    }

    // Channel photos.

    /// The channel's photo URL if known; else it's looked up in the
    /// background (once), with yt-dlp.
    pub fn avatar_url(&mut self, id: &ChannelId) -> Option<String> {
        if let Some(known) = self.avatars.get(id) {
            return known.clone();
        }
        if let Ok(Some(channel)) = self.store.channel(id)
            && channel
                .avatar_checked
                .is_some_and(|at| now() - at < AVATAR_EVERY)
        {
            self.avatars.insert(id.clone(), channel.avatar.clone());
            return channel.avatar;
        }
        self.avatars.insert(id.clone(), None);
        self.look_up_channel(id, Tab::Videos);
        None
    }

    /// Asks yt-dlp, in the background, for a tab of the channel's page: its
    /// photo and newest videos, with the lengths feeds don't give.
    fn look_up_channel(&mut self, id: &ChannelId, tab: Tab) {
        let Some(yt) = self.yt.clone() else {
            return;
        };
        if self.in_flight.contains(id) || self.channel_lookups >= MAX_CHANNEL_LOOKUPS {
            return;
        }
        self.channel_lookups += 1;
        self.in_flight.insert(id.clone());
        let tx = self.tx.clone();
        let id = id.clone();
        tokio::spawn(async move {
            let result = yt.channel(&id, tab, CHANNEL_LOOKUP, true).await;
            let _ = tx.send(AppEvent::ChannelInfo { id, tab, result });
        });
    }

    /// When the channel's tab was last looked at, this session or before.
    fn tab_checked(&mut self, id: &ChannelId, tab: Tab) -> Option<i64> {
        let key = (id.clone(), tab);
        if let Some(&at) = self.checked.get(&key) {
            return at;
        }
        let at = self
            .store
            .tab_checked(id, tab == Tab::Streams)
            .ok()
            .flatten();
        self.checked.insert(key, at);
        at
    }

    fn set_tab_checked(&mut self, id: &ChannelId, tab: Tab) {
        let now = now();
        let _ = self.store.set_tab_checked(id, tab == Tab::Streams, now);
        self.checked.insert((id.clone(), tab), Some(now));
    }

    /// A card on screen has no length (it came from a feed): its channel's
    /// Videos tab is looked at, then its Live tab, once each (and kept
    /// across runs), again for a video newer than the last look, and every
    /// `LIVE_EVERY` for a live stream or premiere until it has ended.
    pub fn want_length(&mut self, video: &Video) {
        if video.duration.is_some() || video.short {
            return;
        }
        let Some(id) = video.channel_id.clone() else {
            return;
        };
        let now = now();
        for tab in [Tab::Videos, Tab::Streams] {
            let due = match self.tab_checked(&id, tab) {
                None => true,
                Some(at) => {
                    video.published.is_some_and(|p| p > at)
                        || (tab == Tab::Streams
                            && (video.live || video.upcoming)
                            && now - at > LIVE_EVERY)
                }
            };
            if due {
                self.look_up_channel(&id, tab);
                return;
            }
        }
    }

    // Feeds.

    /// Fetches the feeds of subscriptions not fetched lately, or all of
    /// them if `all`.
    pub fn refresh_feeds(&mut self, all: bool) {
        if self.refreshing.is_some() || self.demo {
            return;
        }
        let since = if all {
            i64::MAX
        } else {
            now() - self.settings.refresh_every()
        };
        let due = self.store.feeds_due(since).unwrap_or_default();
        if due.is_empty() {
            self.refreshed = Some(Instant::now());
            if all {
                self.info("No subscriptions to update");
            }
            return;
        }
        self.refreshing = Some((0, due.len()));
        self.feed_failures = 0;
        let limit = Arc::new(Semaphore::new(feed::PARALLEL));
        for channel in due {
            let (tx, http, limit) = (self.tx.clone(), self.http.clone(), limit.clone());
            tokio::spawn(async move {
                let result = feed::fetch(&http, &limit, &channel).await;
                let _ = tx.send(AppEvent::Feed { channel, result });
            });
        }
    }

    fn on_feed(&mut self, channel: ChannelId, result: Result<Feed>) {
        match result {
            Ok(feed) => {
                let _ = self.store.save_videos(&feed.videos);
                let _ = self.store.feed_checked(&channel, now());
                let untitled = self
                    .subscriptions
                    .iter()
                    .any(|c| c.id == channel && c.title.is_empty());
                if untitled && !feed.channel.is_empty() {
                    let _ = self.store.subscribe(&channel, &feed.channel);
                    self.reload_subscriptions();
                }
            }
            Err(_) => self.feed_failures += 1,
        }
        let Some((done, total)) = self.refreshing.as_mut() else {
            return;
        };
        *done += 1;
        let (done, total) = (*done, *total);
        let shows_feeds = matches!(self.view, View::Home | View::Shorts)
            || matches!(&self.view, View::Channel(id, _) if *id == channel);
        if shows_feeds && (done % 10 == 0 || done == total) {
            self.reload();
        }
        if done == total {
            self.refreshing = None;
            self.refreshed = Some(Instant::now());
            if self.feed_failures > 0 {
                self.error(format!(
                    "{} of {total} channels didn't load: YouTube's feed server is flaky. R tries again",
                    self.feed_failures
                ));
            }
        }
    }

    // Searching and channels.

    fn search(&mut self, query: &str, count: usize) {
        if self.demo {
            return self.info(DEMO);
        }
        let Some(yt) = self.yt.clone() else {
            return self.error(self.tools.missing().unwrap_or_default());
        };
        let query = video::one_line(query, MAX_QUERY);
        if query.is_empty() {
            return;
        }
        if count == SEARCH_PAGE {
            self.search_results.clear();
            self.show(View::Search(query.clone()), false);
            self.focus = Focus::Grid;
            self.sync_sidebar();
        }
        self.search_count = count;
        self.request += 1;
        let request = self.request;
        self.loading = Some("Searching…".into());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = yt.search(&query, count).await;
            let _ = tx.send(AppEvent::Search {
                request,
                count,
                result,
            });
        });
    }

    fn fetch_channel(&mut self, id: ChannelId) {
        let Some(yt) = self.yt.clone() else {
            return;
        };
        self.request += 1;
        let request = self.request;
        self.loading = Some("Loading the channel's videos…".into());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = yt.channel(&id, Tab::Videos, 60, false).await;
            let _ = tx.send(AppEvent::Channel { request, result });
        });
    }

    /// Near the end of the search results: ask for more.
    fn load_more(&mut self) {
        let View::Search(query) = &self.view else {
            return;
        };
        let near_end = self.selected + self.grid.get().cols * 2 >= self.videos.len();
        let full = self.videos.len() >= self.search_count;
        if near_end && full && self.loading.is_none() && self.search_count < MAX_SEARCH {
            let query = query.clone();
            self.search(&query, self.search_count + SEARCH_PAGE);
        }
    }

    // Playing.

    fn play(&mut self, audio_only: bool) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        if self.demo {
            return self.info(DEMO);
        }
        let (Some(yt), Some(_)) = (self.yt.clone(), self.tools.mpv.as_ref()) else {
            return self.error(self.tools.missing().unwrap_or_default());
        };
        self.request += 1;
        let request = self.request;
        self.resolving = Some((request, video.clone()));
        let quality = if audio_only {
            Quality::AudioOnly
        } else {
            Quality::Video(self.settings.max_height.clamp(144, 4320))
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = yt.resolve(&video.id, quality).await;
            let _ = tx.send(AppEvent::Resolved {
                request,
                video,
                audio_only,
                result,
            });
        });
    }

    fn on_resolved(&mut self, video: Video, audio_only: bool, streams: Streams) {
        self.stop_playing();
        let Some(mpv) = self.tools.mpv.clone() else {
            return;
        };
        let start_at = self
            .store
            .progress(&video.id)
            .ok()
            .flatten()
            .filter(|&(at, length)| at > RESUME_AFTER && at < length - RESUME_AFTER)
            .map(|(at, _)| at);
        self.plays += 1;
        let start = Start {
            mpv: &mpv,
            tools: &self.tools,
            streams: &streams,
            title: &video.title,
            start_at,
            audio_only,
        };
        match Player::start(self.plays, start, self.tx.clone()) {
            Ok(player) => {
                let _ = self.store.save_videos(std::slice::from_ref(&video));
                if self.settings.history {
                    let _ = self
                        .store
                        .record_watch(&video.id, now(), None, streams.duration);
                }
                let what = if audio_only {
                    "Listening to"
                } else {
                    "Playing"
                };
                let from = start_at.map_or(String::new(), |at| {
                    format!(" from {}", video::duration(at as u32))
                });
                self.info(format!("{what} “{}”{from}", video.title));
                self.playing = Some(Playing {
                    player,
                    video,
                    position: start_at.unwrap_or(0.0),
                    duration: streams.duration,
                    paused: false,
                    audio_only,
                    saved: Instant::now(),
                });
            }
            Err(e) => self.error(format!("mpv didn't start: {e}")),
        }
    }

    fn on_player(&mut self, event: PlayerEvent) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        if playing.player.play != event.play {
            return;
        }
        match event.kind {
            PlayerEventKind::Position(at) => {
                playing.position = at;
                if playing.saved.elapsed() > SAVE_EVERY {
                    playing.saved = Instant::now();
                    self.save_progress();
                }
            }
            PlayerEventKind::Duration(length) => playing.duration = Some(length),
            PlayerEventKind::Paused(paused) => playing.paused = paused,
            PlayerEventKind::Exited => self.stop_playing(),
        }
    }

    /// Remembers where the playing video is, if history is on.
    fn save_progress(&mut self) {
        let Some(playing) = &self.playing else {
            return;
        };
        if !self.settings.history || !playing.controllable() {
            return;
        }
        let id = playing.video.id.clone();
        let (at, length) = (playing.position, playing.duration);
        let _ = self.store.record_watch(&id, now(), Some(at), length);
        if let Some(length) = length {
            self.progress.insert(id, (at, length));
        }
    }

    /// Shows `video` in the player bar, `position` seconds in, with no
    /// mpv behind it: the demo's.
    pub fn show_playing(&mut self, video: Video, position: f64, duration: f64, audio_only: bool) {
        self.plays += 1;
        self.playing = Some(Playing {
            player: Player::detached(self.plays),
            video,
            position,
            duration: Some(duration),
            paused: false,
            audio_only,
            saved: Instant::now(),
        });
    }

    fn stop_playing(&mut self) {
        self.save_progress();
        if let Some(mut playing) = self.playing.take() {
            playing.player.stop();
        }
        if self.view == View::History {
            self.reload();
        }
    }

    // Lists you keep.

    fn toggle_watch_later(&mut self) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        let _ = self.store.save_videos(std::slice::from_ref(&video));
        match self.store.toggle_watch_later(&video.id, now()) {
            Ok(true) => {
                self.watch_later.insert(video.id.clone());
                self.info(format!("Saved to Watch later: “{}”", video.title));
            }
            Ok(false) => {
                self.watch_later.remove(&video.id);
                self.info(format!("Removed from Watch later: “{}”", video.title));
                if self.view == View::WatchLater {
                    self.reload();
                }
            }
            Err(e) => self.error(format!("Couldn't save: {e}")),
        }
    }

    /// Takes the selected video off the list shown, if it's one you keep.
    fn remove_selected(&mut self) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        match self.view {
            View::WatchLater => self.toggle_watch_later(),
            View::History => {
                let _ = self.store.forget_watch(&video.id);
                self.progress.remove(&video.id);
                self.info(format!("Removed from History: “{}”", video.title));
                self.reload();
            }
            _ => {}
        }
    }

    /// Subscribes to the selected video's channel, or unsubscribes.
    fn toggle_subscription(&mut self) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        let Some(id) = video.channel_id.clone() else {
            return self.error("YouTube didn't say whose video this is");
        };
        if self.store.is_subscribed(&id).unwrap_or(false) {
            self.unsubscribe(&id, &video.channel);
        } else {
            let _ = self.store.subscribe(&id, &video.channel);
            self.reload_subscriptions();
            self.info(format!("Subscribed to {}", video.channel));
            self.refresh_feeds(false);
        }
    }

    fn unsubscribe(&mut self, id: &ChannelId, name: &str) {
        let _ = self.store.unsubscribe(id);
        self.last_unsubscribed = Some((id.clone(), name.to_string()));
        self.reload_subscriptions();
        self.info(format!("Unsubscribed from {name}. u undoes it"));
        if matches!(self.view, View::Home | View::Shorts) {
            self.reload();
        }
    }

    fn undo_unsubscribe(&mut self) {
        if let Some((id, name)) = self.last_unsubscribed.take() {
            let _ = self.store.subscribe(&id, &name);
            self.reload_subscriptions();
            self.info(format!("Subscribed to {name} again"));
            if matches!(self.view, View::Home | View::Shorts) {
                self.reload();
            }
        }
    }

    /// Imports subscriptions from a Takeout `subscriptions.csv`.
    pub fn import(&mut self, path: &str) {
        if self.demo {
            return self.info(DEMO);
        }
        let path = PathBuf::from(unquote(path));
        match takeout::read(&path) {
            Ok(channels) => {
                let mut added = 0;
                for (id, name) in &channels {
                    if !self.store.is_subscribed(id).unwrap_or(false) {
                        added += 1;
                    }
                    let _ = self.store.subscribe(id, name);
                }
                self.reload_subscriptions();
                self.info(format!(
                    "Imported {} subscriptions ({added} new). Fetching their videos…",
                    channels.len()
                ));
                self.show(View::Home, false);
                self.sync_sidebar();
                self.refresh_feeds(false);
            }
            Err(e) => self.error(text::clean(&format!("{e:#}"))),
        }
    }

    fn open_in_browser(&mut self) {
        if let Some(video) = self.selected_video() {
            // Built from the checked id: the browser only ever gets YouTube.
            let url = video.id.url();
            if let Err(e) = open::that_detached(&url) {
                self.error(format!("Couldn't open the browser: {e}"));
            }
        }
    }

    fn copy_link(&mut self) {
        let Some(url) = self.selected_video().map(|v| v.id.url()) else {
            return;
        };
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(url.clone())) {
            Ok(()) => self.info(format!("Copied {url}")),
            Err(e) => self.error(format!("Couldn't copy: {e}")),
        }
    }

    fn cycle_theme(&mut self) {
        self.settings.theme = theme::next(&self.settings.theme).to_string();
        self.colors = Colors::named(&self.settings.theme);
        self.save_settings();
        self.info(format!("Theme: {}", self.colors.name));
    }

    fn save_settings(&mut self) {
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            self.error(format!("{e:#}"));
        }
    }

    // Events.

    fn on_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Feed { channel, result } => self.on_feed(channel, result),
            AppEvent::Search {
                request,
                count,
                result,
            } => {
                if request != self.request {
                    return;
                }
                self.loading = None;
                match result {
                    Ok(videos) => {
                        let _ = self.store.save_videos(&videos);
                        self.search_results = videos;
                        self.search_count = count;
                        self.reload();
                    }
                    Err(e) => self.error(format!("Search failed: {e:#}")),
                }
            }
            AppEvent::Channel { request, result } => {
                if request != self.request {
                    return;
                }
                self.loading = None;
                match result {
                    Ok(page) => {
                        let _ = self.store.save_videos(&page.videos);
                        self.set_tab_checked(&page.id, Tab::Videos);
                        let _ = self.store.set_avatar(
                            &page.id,
                            &page.title,
                            page.avatar.as_deref(),
                            now(),
                        );
                        self.avatars.insert(page.id.clone(), page.avatar);
                        self.reload();
                    }
                    Err(e) => self.error(format!("The channel didn't load: {e:#}")),
                }
            }
            AppEvent::ChannelInfo { id, tab, result } => {
                self.in_flight.remove(&id);
                // Tried, found or not: a failure (a bot check, no network)
                // isn't retried until there's a newer video to look for.
                self.set_tab_checked(&id, tab);
                if let Ok(page) = result {
                    let _ = self.store.save_videos(&page.videos);
                    if page.avatar.is_some() || tab == Tab::Videos {
                        let _ =
                            self.store
                                .set_avatar(&id, &page.title, page.avatar.as_deref(), now());
                        self.avatars.insert(id, page.avatar);
                    }
                    self.needs_reload = true;
                }
            }
            AppEvent::Resolved {
                request,
                video,
                audio_only,
                result,
            } => {
                if self.resolving.as_ref().map(|(r, _)| *r) != Some(request) {
                    return;
                }
                self.resolving = None;
                match result {
                    Ok(streams) => self.on_resolved(video, audio_only, streams),
                    Err(e) => self.error(format!("Can't play “{}”: {e:#}", video.title)),
                }
            }
            AppEvent::Player(event) => self.on_player(event),
            AppEvent::Image(event) => self.images.on_built(event),
        }
    }

    fn on_terminal(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Paste(text) => {
                if let Some(prompt) = self.prompt.as_mut() {
                    let line = video::one_line(&text, 4096);
                    prompt.text.push_str(&line);
                }
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.prompt.is_some() {
            return self.on_prompt_key(key);
        }
        if self.help {
            self.help = false;
            return;
        }
        if std::mem::take(&mut self.pending_g) && key.code == KeyCode::Char('g') {
            return self.jump_to(0);
        }
        // Letters act here only without Ctrl: Ctrl-u and Ctrl-d move half a
        // page in the grid and the sidebar.
        match key.code {
            KeyCode::Char('q') if !ctrl => self.quit = true,
            KeyCode::Char('?') if !ctrl => self.help = true,
            KeyCode::Char('/' | 's') if !ctrl => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Search,
                    text: self.last_query(),
                });
            }
            KeyCode::Char('I') if !ctrl => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Import,
                    text: String::new(),
                });
            }
            KeyCode::Char('T') if !ctrl => self.cycle_theme(),
            KeyCode::Char('R') if !ctrl => self.refresh(),
            KeyCode::Char('u') if !ctrl => self.undo_unsubscribe(),
            KeyCode::Char(' ') if !ctrl && self.playing.is_some() => {
                if let Some(p) = &self.playing {
                    p.player.toggle_pause();
                }
            }
            KeyCode::Char(c @ (',' | '.' | '<' | '>')) if !ctrl && self.playing.is_some() => {
                let seconds = match c {
                    ',' => -10,
                    '.' => 10,
                    '<' => -60,
                    _ => 60,
                };
                if let Some(p) = &self.playing {
                    p.player.seek(seconds);
                }
            }
            KeyCode::Char('X') if !ctrl && self.playing.is_some() => {
                self.stop_playing();
                self.info("Stopped");
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Grid,
                    Focus::Grid => Focus::Sidebar,
                };
            }
            KeyCode::Backspace => self.go_back(),
            KeyCode::Char('o') if ctrl => self.go_back(),
            _ => match self.focus {
                Focus::Sidebar => self.on_sidebar_key(key),
                Focus::Grid => self.on_grid_key(key),
            },
        }
    }

    fn refresh(&mut self) {
        self.images.retry_failed();
        match self.view.clone() {
            View::Search(q) => self.search(&q, SEARCH_PAGE),
            View::Channel(id, _) => {
                self.fetch_channel(id);
            }
            _ => {
                self.refresh_feeds(true);
                if self.refreshing.is_some() {
                    self.info("Updating your subscriptions…");
                }
            }
        }
    }

    fn on_prompt_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let Some(prompt) = self.prompt.take() else {
                    return;
                };
                match prompt.kind {
                    PromptKind::Search => self.search(&prompt.text, SEARCH_PAGE),
                    PromptKind::Import => self.import(&prompt.text),
                }
            }
            KeyCode::Backspace => {
                prompt.text.pop();
            }
            KeyCode::Char('u') if ctrl => prompt.text.clear(),
            KeyCode::Char('w') if ctrl => {
                let trimmed = prompt.text.trim_end().len();
                prompt.text.truncate(trimmed);
                let cut = prompt.text.rfind(' ').map_or(0, |i| i + 1);
                prompt.text.truncate(cut);
            }
            KeyCode::Char(c)
                if !ctrl && !text::is_hidden(c) && prompt.text.chars().count() < 4096 =>
            {
                prompt.text.push(c);
            }
            _ => {}
        }
    }

    fn on_sidebar_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = self.sidebar_len().saturating_sub(1);
        let moved = match key.code {
            KeyCode::Char('d') if ctrl => Some((self.sidebar_selected + SIDEBAR_PAGE).min(last)),
            KeyCode::Char('u') if ctrl => Some(self.sidebar_selected.saturating_sub(SIDEBAR_PAGE)),
            KeyCode::PageDown => Some((self.sidebar_selected + 2 * SIDEBAR_PAGE).min(last)),
            KeyCode::PageUp => Some(self.sidebar_selected.saturating_sub(2 * SIDEBAR_PAGE)),
            KeyCode::Char('j') | KeyCode::Down => Some((self.sidebar_selected + 1).min(last)),
            KeyCode::Char('k') | KeyCode::Up => Some(self.sidebar_selected.saturating_sub(1)),
            KeyCode::Char('G') | KeyCode::End => Some(last),
            KeyCode::Home => Some(0),
            KeyCode::Char('g') => {
                self.pending_g = true;
                None
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                let entry = self.entry(self.sidebar_selected);
                if entry == Entry::Search {
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Search,
                        text: self.last_query(),
                    });
                    return;
                }
                if let Some(view) = self.entry_view(entry) {
                    self.show(view, matches!(entry, Entry::Channel(_)));
                }
                self.focus = Focus::Grid;
                None
            }
            KeyCode::Char('x' | 'd') if !ctrl => {
                if let Entry::Channel(i) = self.entry(self.sidebar_selected)
                    && let Some(c) = self.subscriptions.get(i).cloned()
                {
                    self.unsubscribe(&c.id, &c.title);
                }
                None
            }
            _ => None,
        };
        if let Some(i) = moved
            && i != self.sidebar_selected
        {
            self.sidebar_selected = i;
            // Moving shows what's stored at once; Enter also fetches.
            if let Some(view) = self.entry_view(self.entry(i)) {
                self.show(view, false);
            }
        }
    }

    fn jump_to(&mut self, i: usize) {
        match self.focus {
            Focus::Sidebar => {
                self.sidebar_selected = i.min(self.sidebar_len().saturating_sub(1));
                if let Some(view) = self.entry_view(self.entry(self.sidebar_selected)) {
                    self.show(view, false);
                }
            }
            Focus::Grid => self.select(i),
        }
    }

    fn select(&mut self, i: usize) {
        self.selected = i.min(self.videos.len().saturating_sub(1));
        self.load_more();
    }

    fn on_grid_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let GridShape { cols, rows } = self.grid.get();
        let cols = cols.max(1);
        let page = rows.max(1) * cols;
        let last = self.videos.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('h') | KeyCode::Left => {
                if self.selected.is_multiple_of(cols) {
                    self.focus = Focus::Sidebar;
                } else {
                    self.select(self.selected - 1);
                }
            }
            KeyCode::Char('l') | KeyCode::Right => self.select((self.selected + 1).min(last)),
            KeyCode::Char('j') | KeyCode::Down => {
                // Into a shorter last row: its last card.
                if self.selected / cols < last / cols {
                    self.select((self.selected + cols).min(last));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.selected >= cols {
                    self.select(self.selected - cols);
                }
            }
            KeyCode::Char('d') if ctrl => self.select((self.selected + page / 2).min(last)),
            KeyCode::Char('u') if ctrl => self.select(self.selected.saturating_sub(page / 2)),
            KeyCode::PageDown => self.select((self.selected + page).min(last)),
            KeyCode::PageUp => self.select(self.selected.saturating_sub(page)),
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(last),
            KeyCode::Enter => self.play(false),
            KeyCode::Char('a') => self.play(true),
            KeyCode::Char('w') => self.toggle_watch_later(),
            KeyCode::Char('x') | KeyCode::Delete => self.remove_selected(),
            KeyCode::Char('c') => {
                if let Some(video) = self.selected_video().cloned()
                    && let Some(id) = video.channel_id
                {
                    let name = if video.channel.is_empty() {
                        "Channel".into()
                    } else {
                        video.channel
                    };
                    self.show(View::Channel(id, name), true);
                    self.sync_sidebar();
                }
            }
            KeyCode::Char('S') => self.toggle_subscription(),
            KeyCode::Char('o') => self.open_in_browser(),
            KeyCode::Char('y') => self.copy_link(),
            KeyCode::Esc => self.focus = Focus::Sidebar,
            _ => {}
        }
    }
}

/// A path as typed or dropped on the terminal: quotes around it, `\ ` for
/// spaces, `~` for home.
fn unquote(path: &str) -> String {
    let path = path.trim();
    let path = path
        .strip_prefix('\'')
        .and_then(|p| p.strip_suffix('\''))
        .or_else(|| path.strip_prefix('"').and_then(|p| p.strip_suffix('"')))
        .map_or_else(|| path.replace("\\ ", " "), str::to_string);
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|home| home.join(rest).to_string_lossy().into_owned())
            .unwrap_or(path),
        None => path,
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::store::tests::video;
    use ratatui_image::picker::Picker;

    pub fn app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let http = feed::client();
        let images = Images::new(Picker::halfblocks(), tx.clone(), http.clone(), None);
        App::new(
            Store::in_memory(),
            Settings::default(),
            None,
            Tools::default(),
            None,
            http,
            tx,
            images,
        )
    }

    pub const CH: &str = "UC7EVSn5inapL20oPSwAwEUg";

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    pub fn with_feed(app: &mut App, n: usize) {
        let ch = ChannelId::parse(CH).unwrap();
        app.store.subscribe(&ch, "BekBrace").unwrap();
        let videos: Vec<Video> = (0..n)
            .map(|i| video(&format!("vid{i:08}"), CH, Some(1000 + i as i64)))
            .collect();
        app.store.save_videos(&videos).unwrap();
        app.reload_subscriptions();
        app.reload();
    }

    #[tokio::test]
    async fn arrows_and_vim_keys_move_through_the_grid() {
        let mut app = app();
        with_feed(&mut app, 10);
        app.grid.set(GridShape { cols: 3, rows: 2 });
        assert_eq!(app.selected, 0);
        app.on_key(key(KeyCode::Char('l')));
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.selected, 2);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 5);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected, 9, "into the shorter last row");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected, 9, "no row below");
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.selected, 0);
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.selected, 9);
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.selected, 6);
        app.on_key(key(KeyCode::Char('l')));
        app.on_key(key(KeyCode::Char('h')));
        assert_eq!(app.selected, 6);
        // Left from the first column moves to the sidebar.
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.focus, Focus::Sidebar);
        assert_eq!(app.selected, 6, "the card stays selected");
    }

    #[tokio::test]
    async fn ctrl_u_and_ctrl_d_move_half_a_page_not_undo_or_unsubscribe() {
        let mut app = app();
        with_feed(&mut app, 30);
        app.grid.set(GridShape { cols: 3, rows: 2 });
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        app.on_key(ctrl('d'));
        assert_eq!(app.selected, 3);
        app.on_key(ctrl('d'));
        app.on_key(ctrl('u'));
        assert_eq!(app.selected, 3, "Ctrl-u moved up, not taken for u (undo)");

        app.focus = Focus::Sidebar;
        app.on_key(ctrl('d'));
        assert_eq!(app.sidebar_selected, MENU.len(), "the one channel, at most");
        app.on_key(ctrl('d'));
        assert_eq!(app.subscriptions.len(), 1, "Ctrl-d isn't d (unsubscribe)");
        app.on_key(ctrl('u'));
        assert_eq!(app.sidebar_selected, 0);
    }

    #[tokio::test]
    async fn the_sidebar_switches_views_and_lists_subscriptions() {
        let mut app = app();
        with_feed(&mut app, 3);
        app.focus = Focus::Sidebar;
        assert_eq!(app.sidebar_len(), MENU.len() + 1);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.view, View::Shorts);
        assert!(app.videos.is_empty(), "no shorts");
        app.on_key(key(KeyCode::Char('G')));
        assert!(matches!(app.view, View::Channel(_, ref name) if name == "BekBrace"));
        assert_eq!(app.videos.len(), 3);
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.subscriptions.is_empty(), "unsubscribed");
        app.on_key(key(KeyCode::Char('u')));
        assert_eq!(app.subscriptions.len(), 1, "undone");
    }

    #[tokio::test]
    async fn watch_later_is_kept_and_listed() {
        let mut app = app();
        with_feed(&mut app, 3);
        app.on_key(key(KeyCode::Char('w')));
        let saved = app.videos[0].id.clone();
        assert!(app.watch_later.contains(&saved));
        app.show(View::WatchLater, false);
        assert_eq!(app.videos.len(), 1);
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.videos.is_empty());
    }

    #[tokio::test]
    async fn typing_a_search_keeps_out_hidden_characters() {
        let mut app = app();
        app.on_key(key(KeyCode::Char('/')));
        for c in ['r', '\u{202E}', 'u', 's', 't'] {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_terminal(Event::Paste(" lang\nuage\u{1b}".into()));
        assert_eq!(app.prompt.as_ref().unwrap().text, "rustlang uage");
        app.on_key(key(KeyCode::Esc));
        assert!(app.prompt.is_none());
    }

    #[test]
    fn dropped_paths_are_unquoted() {
        assert_eq!(
            unquote("'/a b/subscriptions.csv'"),
            "/a b/subscriptions.csv"
        );
        assert_eq!(
            unquote("/a\\ b/subscriptions.csv "),
            "/a b/subscriptions.csv"
        );
        assert_eq!(unquote("\"/x.csv\""), "/x.csv");
    }

    #[tokio::test]
    async fn answers_to_older_requests_are_dropped() {
        let mut app = app();
        app.request = 5;
        app.on_event(AppEvent::Search {
            request: 4,
            count: SEARCH_PAGE,
            result: Ok(vec![video("aaaaaaaaaa1", CH, None)]),
        });
        assert!(app.search_results.is_empty());
    }
}
