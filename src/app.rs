//! The app's state and everything that changes it: keys, answers from
//! yt-dlp and the feeds, images, and mpv. All state lives in [`App`];
//! nothing else mutates it.

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
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

/// The longest search or path typed or pasted, in characters.
const MAX_PROMPT: usize = 4096;

/// Rows Ctrl-d and Ctrl-u move in the sidebar; PgDn and PgUp move twice
/// as many.
const SIDEBAR_PAGE: usize = 10;

/// What the demo says when asked to fetch or play something.
const DEMO: &str =
    "This is the demo: made-up videos, nothing to fetch or play. Run tuitube without --demo";

/// How often tuitube looks for channels whose feed is older than
/// `refresh_minutes`; only those are fetched.
const CHECK_EVERY: Duration = Duration::from_secs(5 * 60);
/// How long feeds, or background looks at channels, wait after YouTube
/// asks tuitube to slow down (HTTP 429, or a bot check).
const SLOW_DOWN: Duration = Duration::from_secs(60 * 60);
/// Videos published longer ago than this, and not in Watch later or
/// History, are forgotten at start.
const KEEP_VIDEOS: i64 = 120 * 24 * 3600;
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
/// Looks at one channel's page per session, at most: a channel whose
/// videos keep looking unexplained can't keep yt-dlp busy.
const MAX_LOOKUPS_PER_CHANNEL: u32 = 8;
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
/// mpv saying a video ended counts as played to the end only this close to
/// its length: a stream cut off midway (an expired URL, a lost connection)
/// doesn't move on to the next video.
const ENDED_WITHIN: f64 = 30.0;
/// Videos in a row autoplay skips because they can't be played (removed,
/// private) before it gives up.
const MAX_SKIPS: u32 = 3;
/// Videos asked for in a Mix: YouTube's goes on for over a thousand.
const MIX_SIZE: usize = 50;
/// Channels whose feed was missing that one round of feeds asks yt-dlp
/// about, at most. The feed server answering 404 for everyone is an outage,
/// not a hundred removed channels, and each ask is a request YouTube counts.
const GONE_CHECKS_PER_ROUND: usize = 5;

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
    Mix {
        request: u64,
        seed: VideoId,
        result: Result<Vec<Video>>,
    },
    ChannelInfo {
        id: ChannelId,
        tab: Tab,
        result: Result<ChannelPage>,
    },
    Resolved {
        request: u64,
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
    /// YouTube's Mix of a video: similar videos, as YouTube picks them.
    Mix(VideoId, String),
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
            View::Mix(_, title) => format!("Mix – {title}"),
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
    /// mpv said the video played to its end.
    ended: bool,
}

/// A video being looked up to play.
pub struct Resolving {
    request: u64,
    pub video: Video,
    audio_only: bool,
    /// Played because the video before it ended, not asked for.
    auto: bool,
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
    /// Looks at each channel this session, kept to `MAX_LOOKUPS_PER_CHANNEL`
    /// whatever a channel's videos say.
    lookups_per_channel: HashMap<ChannelId, u32>,
    /// Channels whose feed was missing, asked about with yt-dlp this
    /// session (once each) to find out whether they're gone.
    gone_checks: HashSet<ChannelId>,
    round_gone_checks: usize,
    /// Background answers changed stored videos: reload after this batch.
    needs_reload: bool,

    pub view: View,
    pub videos: Vec<Video>,
    search_results: Vec<Video>,
    search_count: usize,
    /// Mixes loaded this session, by the video each is of.
    mixes: HashMap<VideoId, Vec<Video>>,
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
    /// When a subscription's feed was last fetched, for Home's header.
    pub feeds_updated: Option<i64>,
    /// Until when feeds, and background looks at channels, wait: YouTube
    /// asked tuitube to slow down.
    feeds_paused: Option<Instant>,
    lookups_paused: Option<Instant>,
    pub status: Option<Status>,
    pub playing: Option<Playing>,
    pub resolving: Option<Resolving>,
    /// What plays after the video playing, in order, when it ends (with
    /// `autoplay` on) or on N: the cards after it in the list it was played
    /// from, or the rest of its Mix.
    pub up_next: VecDeque<Video>,
    /// Videos autoplay skipped in a row because they couldn't be played.
    skipped: u32,
    last_unsubscribed: Option<(ChannelId, String)>,

    /// Numbers each request whose answer changes the view, so a late answer
    /// to an older one is dropped.
    request: u64,
    /// Numbers each video looked up to play, so a late answer is dropped.
    resolves: u64,
    plays: u64,
    pending_g: bool,
    pub quit: bool,
    /// `--demo`: made-up videos; nothing is fetched or played.
    pub demo: bool,
    /// The system clipboard, kept for the whole run: on Linux, copied text
    /// is held by the program that copied it, and goes when it lets go.
    clipboard: Option<arboard::Clipboard>,
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
            lookups_per_channel: HashMap::new(),
            gone_checks: HashSet::new(),
            round_gone_checks: 0,
            needs_reload: false,
            view: View::Home,
            videos: Vec::new(),
            search_results: Vec::new(),
            search_count: SEARCH_PAGE,
            mixes: HashMap::new(),
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
            feeds_updated: None,
            feeds_paused: None,
            lookups_paused: None,
            status: None,
            playing: None,
            resolving: None,
            up_next: VecDeque::new(),
            skipped: 0,
            last_unsubscribed: None,
            request: 0,
            resolves: 0,
            plays: 0,
            pending_g: false,
            quit: false,
            demo: false,
            clipboard: None,
        };
        let _ = app.store.prune(now().saturating_sub(KEEP_VIDEOS));
        app.feeds_updated = app.store.feeds_updated().ok().flatten();
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
                .is_some_and(|at| now().saturating_sub(at) < AVATAR_EVERY);
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
            View::Mix(id, _) => Ok(self.mixes.get(id).cloned().unwrap_or_default()),
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
        // older than `refresh_minutes`; R fetches them anyway.
        if fetch && let View::Channel(id, _) = &self.view {
            let id = id.clone();
            let kept = self
                .tab_checked(&id, Tab::Videos)
                .is_some_and(|at| now().saturating_sub(at) < self.settings.refresh_every());
            if !kept && !self.is_gone(&id) {
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
            View::Mix(..) => None,
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

    /// Whether YouTube said the subscribed channel is gone.
    pub fn is_gone(&self, id: &ChannelId) -> bool {
        self.subscriptions.iter().any(|c| c.id == *id && c.gone)
    }

    /// YouTube says the channel is gone: its feed isn't fetched again, and
    /// it's marked in the sidebar for you to unsubscribe.
    fn mark_gone(&mut self, id: &ChannelId) {
        let _ = self.store.set_gone(id, now());
        self.reload_subscriptions();
        let gone: Vec<&Channel> = self.subscriptions.iter().filter(|c| c.gone).collect();
        if !gone.iter().any(|c| c.id == *id) {
            return;
        }
        let text = match gone.as_slice() {
            [one] => format!(
                "{} is no longer on YouTube: x on it in the sidebar unsubscribes",
                one.title
            ),
            many => format!(
                "{} of your channels are no longer on YouTube (marked in the sidebar): x on one unsubscribes",
                many.len()
            ),
        };
        self.info(text);
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
                .is_some_and(|at| now().saturating_sub(at) < AVATAR_EVERY)
        {
            self.avatars.insert(id.clone(), channel.avatar.clone());
            return channel.avatar;
        }
        self.avatars.insert(id.clone(), None);
        self.look_up_channel(id, Tab::Videos);
        None
    }

    /// Asks yt-dlp, in the background, for a tab of the channel's page: its
    /// photo and newest videos, with the lengths feeds don't give. Whether
    /// it asked: not past the session's limits, or while paused.
    fn look_up_channel(&mut self, id: &ChannelId, tab: Tab) -> bool {
        let Some(yt) = self.yt.clone() else {
            return false;
        };
        let per_channel = self.lookups_per_channel.entry(id.clone()).or_default();
        if self.in_flight.contains(id)
            || self.channel_lookups >= MAX_CHANNEL_LOOKUPS
            || *per_channel >= MAX_LOOKUPS_PER_CHANNEL
            || self
                .lookups_paused
                .is_some_and(|until| Instant::now() < until)
        {
            return false;
        }
        *per_channel += 1;
        self.channel_lookups += 1;
        self.in_flight.insert(id.clone());
        let tx = self.tx.clone();
        let id = id.clone();
        tokio::spawn(async move {
            let result = yt.channel(&id, tab, CHANNEL_LOOKUP, true).await;
            let _ = tx.send(AppEvent::ChannelInfo { id, tab, result });
        });
        true
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
        let scheduled = video.live || video.upcoming;
        for tab in [Tab::Videos, Tab::Streams] {
            let due = match self.tab_checked(&id, tab) {
                None => true,
                // A live stream or premiere has a start that may be ahead
                // of now, so it's looked at again by the clock alone.
                Some(at) if scheduled => tab == Tab::Streams && now.saturating_sub(at) > LIVE_EVERY,
                // Published after the last look: the look didn't have it.
                // A date ahead of now counts as now, so it can't be after
                // every look to come.
                Some(at) => video.published.is_some_and(|p| p.min(now) > at),
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
        if let Some(until) = self.feeds_paused {
            if Instant::now() < until {
                if all {
                    let minutes = (until - Instant::now()).as_secs() / 60 + 1;
                    self.error(format!(
                        "YouTube asked tuitube to slow down: feeds wait {minutes} more minutes"
                    ));
                }
                return;
            }
            self.feeds_paused = None;
        }
        let since = if all {
            i64::MAX
        } else {
            now().saturating_sub(self.settings.refresh_every())
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
        self.round_gone_checks = 0;
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
                // The feed's name is the channel's own: a name a Takeout
                // file (or an old feed) gave it gives way.
                let renamed = self
                    .subscriptions
                    .iter()
                    .any(|c| c.id == channel && c.title != feed.channel);
                if renamed && !feed.channel.is_empty() {
                    let _ = self.store.rename(&channel, &feed.channel);
                    self.reload_subscriptions();
                }
            }
            Err(e) if e.is::<feed::TooManyRequests>() => {
                if self.feeds_paused.is_none() {
                    self.feeds_paused = Some(Instant::now() + SLOW_DOWN);
                    self.error("YouTube asked tuitube to slow down: feeds wait an hour. Playing still works");
                }
            }
            Err(e) => {
                self.feed_failures += 1;
                // Missing on every try: yt-dlp can tell whether the channel
                // is gone. Once a session per channel, a few per round.
                if e.is::<feed::NotFound>()
                    && self.round_gone_checks < GONE_CHECKS_PER_ROUND
                    && !self.gone_checks.contains(&channel)
                    && self.look_up_channel(&channel, Tab::Videos)
                {
                    self.gone_checks.insert(channel.clone());
                    self.round_gone_checks += 1;
                }
            }
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
            self.feeds_updated = self.store.feeds_updated().ok().flatten();
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

    /// Shows YouTube's Mix of the selected video. Playing a card in it plays
    /// the rest of the Mix after it; if the video is playing already, the
    /// rest of its Mix plays next.
    fn open_mix(&mut self) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        if self.demo {
            return self.info(DEMO);
        }
        if self.yt.is_none() {
            return self.error(self.tools.missing().unwrap_or_default());
        }
        // The video stays in view while its Mix loads.
        self.mixes
            .entry(video.id.clone())
            .or_insert_with(|| vec![video.clone()]);
        self.show(View::Mix(video.id.clone(), video.title), false);
        self.fetch_mix(video.id);
    }

    fn fetch_mix(&mut self, id: VideoId) {
        let Some(yt) = self.yt.clone() else {
            return;
        };
        self.request += 1;
        let request = self.request;
        self.loading = Some("Loading the Mix…".into());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = yt.mix(&id, MIX_SIZE).await;
            let _ = tx.send(AppEvent::Mix {
                request,
                seed: id,
                result,
            });
        });
    }

    fn on_mix(&mut self, seed: VideoId, result: Result<Vec<Video>>) {
        match result {
            Ok(videos) => {
                let _ = self.store.save_videos(&videos);
                let current = self
                    .playing
                    .as_ref()
                    .map(|p| &p.video.id)
                    .or(self.resolving.as_ref().map(|r| &r.video.id));
                if current == Some(&seed) {
                    self.up_next = playable(videos.iter().filter(|v| v.id != seed));
                    self.info("Up next: the rest of the Mix");
                }
                self.mixes.insert(seed, videos);
                self.reload();
            }
            Err(e) => {
                if matches!(&self.view, View::Mix(id, _) if *id == seed) {
                    self.go_back();
                }
                self.error(format!("No Mix: {e:#}"));
            }
        }
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

    /// Plays the selected video, then the cards after it, one by one.
    fn play_selected(&mut self, audio_only: bool) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        self.up_next = playable(self.videos.iter().skip(self.selected + 1));
        self.play(video, audio_only, false);
    }

    /// Plays the first video up next, if there is one. `auto`: the video
    /// before it ended.
    fn play_next(&mut self, audio_only: bool, auto: bool) -> bool {
        let Some(video) = self.up_next.pop_front() else {
            return false;
        };
        self.play(video, audio_only, auto);
        true
    }

    /// N: the next video now, the same way (picture or sound only).
    fn skip_to_next(&mut self) {
        let audio_only = self
            .playing
            .as_ref()
            .map(|p| p.audio_only)
            .or(self.resolving.as_ref().map(|r| r.audio_only));
        if let Some(audio_only) = audio_only
            && !self.play_next(audio_only, false)
        {
            self.info("Nothing up next");
        }
    }

    fn play(&mut self, video: Video, audio_only: bool, auto: bool) {
        if self.demo {
            return self.info(DEMO);
        }
        let (Some(yt), Some(_)) = (self.yt.clone(), self.tools.mpv.as_ref()) else {
            return self.error(self.tools.missing().unwrap_or_default());
        };
        self.resolves += 1;
        let request = self.resolves;
        let id = video.id.clone();
        self.resolving = Some(Resolving {
            request,
            video,
            audio_only,
            auto,
        });
        let quality = if audio_only {
            Quality::AudioOnly
        } else {
            Quality::Video(self.settings.max_height.clamp(144, 4320))
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = yt.resolve(&id, quality).await;
            let _ = tx.send(AppEvent::Resolved { request, result });
        });
    }

    fn on_play_failed(&mut self, resolving: Resolving, error: anyhow::Error) {
        let error = format!("{error:#}");
        self.error(format!("Can't play “{}”: {error}", resolving.video.title));
        // One in the list that can't be played (removed, private) is
        // skipped, a few in a row at most; a bot check stops autoplay.
        if resolving.auto && self.skipped < MAX_SKIPS && !slowed_down(&error) {
            self.skipped += 1;
            self.play_next(resolving.audio_only, true);
        }
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
                self.skipped = 0;
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
                    ended: false,
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
            PlayerEventKind::Ended => playing.ended = true,
            PlayerEventKind::Exited => {
                // Played to its end, not closed or cut off midway.
                let finished = playing.ended
                    && playing
                        .duration
                        .is_none_or(|length| playing.position > length - ENDED_WITHIN);
                let audio_only = playing.audio_only;
                self.stop_playing();
                // A video you chose while this one played is still loading:
                // it plays, not the next.
                if finished && self.settings.autoplay && self.resolving.is_none() {
                    self.play_next(audio_only, true);
                }
            }
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
            ended: false,
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
            // Gone before? It may be back: asked about again if it still is.
            self.gone_checks.remove(&id);
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
            self.gone_checks.remove(&id);
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
                let added = match self.store.subscribe_all(&channels) {
                    Ok(added) => added,
                    Err(e) => return self.error(format!("Couldn't save: {e:#}")),
                };
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
        let Some(video) = self.selected_video() else {
            return;
        };
        // Built from the checked id: the browser only ever gets YouTube.
        let url = video.id.url();
        if let Err(e) = open_url(&url) {
            self.error(format!("Couldn't open the browser: {e}"));
        }
    }

    fn copy_link(&mut self) {
        let Some(url) = self.selected_video().map(|v| v.id.url()) else {
            return;
        };
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let copied = self
            .clipboard
            .as_mut()
            .is_some_and(|clipboard| clipboard.set_text(url.clone()).is_ok());
        // No system clipboard (over SSH, say): the terminal's own copy
        // (OSC 52), which most terminals take.
        if copied {
            self.info(format!("Copied {url}"));
        } else if copy_through_terminal(&url).is_ok() {
            self.info(format!("Copied {url} (through the terminal)"));
        } else {
            self.error("Couldn't copy: there's no clipboard here");
        }
    }

    fn toggle_autoplay(&mut self) {
        self.settings.autoplay = !self.settings.autoplay;
        self.save_settings();
        self.info(if self.settings.autoplay {
            "Autoplay on: when a video ends, the next one plays"
        } else {
            "Autoplay off"
        });
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
            AppEvent::Mix {
                request,
                seed,
                result,
            } => {
                if request != self.request {
                    return;
                }
                self.loading = None;
                self.on_mix(seed, result);
            }
            AppEvent::ChannelInfo { id, tab, result } => {
                self.in_flight.remove(&id);
                // Tried, found or not: a failure (a bot check, no network)
                // isn't retried until there's a newer video to look for.
                self.set_tab_checked(&id, tab);
                // A bot check or a 429 pauses the background looks: they're
                // what YouTube counts, and playing needs the same address.
                if let Err(e) = &result
                    && slowed_down(&format!("{e:#}"))
                    && self.lookups_paused.is_none()
                {
                    self.lookups_paused = Some(Instant::now() + SLOW_DOWN);
                }
                if let Err(e) = &result
                    && channel_gone(&format!("{e:#}"))
                {
                    self.mark_gone(&id);
                }
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
            AppEvent::Resolved { request, result } => {
                let Some(resolving) = self.resolving.take_if(|r| r.request == request) else {
                    return;
                };
                match result {
                    Ok(streams) => {
                        self.on_resolved(resolving.video, resolving.audio_only, streams);
                    }
                    Err(e) => self.on_play_failed(resolving, e),
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
                    // Cut before cleaning, so a huge paste costs nothing,
                    // and the prompt never holds more than `MAX_PROMPT`.
                    let room = MAX_PROMPT.saturating_sub(prompt.text.chars().count());
                    let line = video::one_line(text::first_chars(&text, 2 * MAX_PROMPT), room);
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
            KeyCode::Char('A') if !ctrl => self.toggle_autoplay(),
            KeyCode::Char('N') if !ctrl => self.skip_to_next(),
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
            KeyCode::Char('X') if !ctrl && (self.playing.is_some() || self.resolving.is_some()) => {
                self.stop_playing();
                self.resolving = None;
                self.up_next.clear();
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
            View::Mix(id, _) => self.fetch_mix(id),
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
                if !ctrl && !text::is_hidden(c) && prompt.text.chars().count() < MAX_PROMPT =>
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
            KeyCode::Enter => self.play_selected(false),
            KeyCode::Char('a') => self.play_selected(true),
            KeyCode::Char('m') => self.open_mix(),
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

/// Asks the terminal to put `text` on the clipboard (OSC 52). Only ever a
/// URL tuitube built from a checked id.
fn copy_through_terminal(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

/// Standard base64, with padding.
fn base64(data: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(CHARS[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// What autoplay can play of `videos`, in order: not a premiere or live
/// stream that hasn't started.
fn playable<'a>(videos: impl Iterator<Item = &'a Video>) -> VecDeque<Video> {
    videos.filter(|v| !v.upcoming).cloned().collect()
}

/// Whether yt-dlp's error says YouTube wants tuitube to slow down.
fn slowed_down(error: &str) -> bool {
    [
        "429",
        "Too Many Requests",
        "not a bot",
        "Sign in to confirm",
    ]
    .iter()
    .any(|sign| error.contains(sign))
}

/// Whether yt-dlp's error says the channel is no longer on YouTube: removed
/// by YouTube, deleted, or its account terminated.
fn channel_gone(error: &str) -> bool {
    [
        "This channel was removed",
        "This channel does not exist",
        "account has been terminated",
    ]
    .iter()
    .any(|sign| error.contains(sign))
}

/// Opens `url` in the browser. macOS (`/usr/bin/open`) and Windows
/// (ShellExecute) need no program found; elsewhere `xdg-open` is, as an
/// absolute path like yt-dlp and mpv, never by name, which would let a
/// relative `PATH` entry pick a planted one.
fn open_url(url: &str) -> std::io::Result<()> {
    if cfg!(any(target_os = "macos", windows)) {
        return open::that_detached(url);
    }
    let opener = crate::tools::on_path("xdg-open").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "xdg-open isn't installed")
    })?;
    open::with_detached(url, opener.to_string_lossy())
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
    async fn a_premiere_set_for_tomorrow_is_looked_at_once_not_forever() {
        let mut app = app();
        let ch = ChannelId::parse(CH).unwrap();
        let mut premiere = video("aaaaaaaaaa1", CH, Some(now() + 86_400));
        premiere.upcoming = true;
        // Both tabs looked at a moment ago: a date ahead of now no longer
        // makes the Videos tab due again and again.
        app.set_tab_checked(&ch, Tab::Videos);
        app.set_tab_checked(&ch, Tab::Streams);
        for _ in 0..5 {
            app.want_length(&premiere);
        }
        assert!(
            app.in_flight.is_empty() && app.channel_lookups == 0,
            "nothing due"
        );
        let mut future = video("aaaaaaaaaa2", CH, Some(now() + 86_400));
        future.upcoming = false;
        app.want_length(&future);
        assert_eq!(app.channel_lookups, 0, "a future date counts as now");
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
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(
            base64(b"https://www.youtube.com/watch?v=x"),
            "aHR0cHM6Ly93d3cueW91dHViZS5jb20vd2F0Y2g/dj14"
        );
    }

    #[test]
    fn slow_down_signs_are_recognized() {
        assert!(slowed_down("Sign in to confirm you're not a bot"));
        assert!(slowed_down("HTTP Error 429: Too Many Requests"));
        assert!(!slowed_down("Video unavailable"));
    }

    #[tokio::test]
    async fn a_channel_youtube_removed_is_marked_and_its_feed_left_alone() {
        let mut app = app();
        with_feed(&mut app, 1);
        let ch = ChannelId::parse(CH).unwrap();
        assert_eq!(
            app.store.feeds_due(i64::MAX).unwrap(),
            std::slice::from_ref(&ch)
        );
        // A bot check isn't a removal.
        app.on_event(AppEvent::ChannelInfo {
            id: ch.clone(),
            tab: Tab::Videos,
            result: Err(anyhow::anyhow!("Sign in to confirm you're not a bot")),
        });
        assert!(!app.is_gone(&ch));
        app.on_event(AppEvent::ChannelInfo {
            id: ch.clone(),
            tab: Tab::Videos,
            result: Err(anyhow::anyhow!(
                "UC7EVSn5inapL20oPSwAwEUg: YouTube said: This channel was removed because it violated our Community Guidelines."
            )),
        });
        assert!(app.is_gone(&ch));
        assert!(app.store.feeds_due(i64::MAX).unwrap().is_empty());
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .text
                .contains("no longer on YouTube")
        );
        assert_eq!(app.subscriptions.len(), 1, "still yours to unsubscribe");
    }

    #[tokio::test]
    async fn a_feed_outage_asks_about_only_a_few_channels() {
        let mut app = app();
        let dir = std::env::temp_dir().join(format!("tuitube-gone-{}", std::process::id()));
        // A yt-dlp that can't be there: each look fails at once, offline. Not
        // in the temporary folder, which other users can write to on Linux.
        let tools = Tools {
            yt_dlp: Some("/dev/null/no-yt-dlp".into()),
            ..Tools::default()
        };
        app.yt = YtDlp::new(&tools, &dir).unwrap();
        let channels: Vec<ChannelId> = (0..20)
            .map(|i| ChannelId::parse(&format!("UC{i:0>22}")).unwrap())
            .collect();
        app.refreshing = Some((0, channels.len()));
        for channel in &channels {
            app.on_event(AppEvent::Feed {
                channel: channel.clone(),
                result: Err(feed::NotFound.into()),
            });
        }
        assert_eq!(app.channel_lookups, GONE_CHECKS_PER_ROUND);
        assert_eq!(app.feed_failures, 20, "all reported");
        // The next round asks about the next few, never the same twice.
        app.round_gone_checks = 0;
        app.refreshing = Some((0, channels.len()));
        for channel in &channels {
            app.on_event(AppEvent::Feed {
                channel: channel.clone(),
                result: Err(feed::NotFound.into()),
            });
        }
        assert_eq!(app.channel_lookups, 2 * GONE_CHECKS_PER_ROUND);
        assert_eq!(app.gone_checks.len(), 2 * GONE_CHECKS_PER_ROUND);
        let _ = std::fs::remove_dir_all(&dir);
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

    fn ids(videos: &VecDeque<Video>) -> Vec<&str> {
        videos.iter().map(|v| v.id.as_str()).collect()
    }

    /// Tells the app what mpv said about the playback shown.
    fn mpv_says(app: &mut App, kinds: impl IntoIterator<Item = PlayerEventKind>) {
        for kind in kinds {
            let play = app.plays;
            app.on_event(AppEvent::Player(PlayerEvent { play, kind }));
        }
    }

    #[tokio::test]
    async fn a_video_played_to_its_end_plays_the_next_card() {
        let mut app = app();
        with_feed(&mut app, 5);
        let mut premiere = video("vid00000001", CH, Some(1001));
        premiere.upcoming = true;
        app.store.save_videos(&[premiere]).unwrap();
        app.reload();
        let listed: Vec<Video> = app.videos.clone();
        assert_eq!(listed[3].id.as_str(), "vid00000001");
        app.select(1);
        // No yt-dlp in tests, so nothing plays; the cards after it are next,
        // but not the premiere that hasn't started.
        app.on_key(key(KeyCode::Enter));
        assert_eq!(ids(&app.up_next), ["vid00000002", "vid00000000"]);

        // Its window closed: nothing more plays.
        app.show_playing(listed[1].clone(), 100.0, 600.0, false);
        mpv_says(&mut app, [PlayerEventKind::Exited]);
        assert!(app.playing.is_none());
        assert_eq!(app.up_next.len(), 2);
        // Ended long before its end (the stream was cut off): the same.
        app.show_playing(listed[1].clone(), 100.0, 600.0, false);
        mpv_says(&mut app, [PlayerEventKind::Ended, PlayerEventKind::Exited]);
        assert_eq!(app.up_next.len(), 2);
        // Autoplay off: the same.
        app.settings.autoplay = false;
        app.show_playing(listed[1].clone(), 599.0, 600.0, false);
        mpv_says(&mut app, [PlayerEventKind::Ended, PlayerEventKind::Exited]);
        assert_eq!(app.up_next.len(), 2);

        app.settings.autoplay = true;
        app.show_playing(listed[1].clone(), 599.0, 600.0, false);
        mpv_says(&mut app, [PlayerEventKind::Ended, PlayerEventKind::Exited]);
        assert_eq!(
            ids(&app.up_next),
            ["vid00000000"],
            "the next one was taken to play"
        );

        // A card you chose is still loading when the one playing ends: it
        // isn't replaced by the next.
        app.show_playing(listed[1].clone(), 599.0, 600.0, false);
        app.resolving = Some(Resolving {
            request: app.resolves,
            video: listed[2].clone(),
            audio_only: false,
            auto: false,
        });
        mpv_says(&mut app, [PlayerEventKind::Ended, PlayerEventKind::Exited]);
        assert_eq!(ids(&app.up_next), ["vid00000000"], "nothing taken");
        assert_eq!(
            app.resolving.take().map(|r| r.video.id),
            Some(listed[2].id.clone())
        );

        // N takes the next one at once; X forgets the rest.
        app.show_playing(listed[2].clone(), 10.0, 600.0, false);
        app.up_next = listed[3..].iter().cloned().collect();
        app.on_key(key(KeyCode::Char('N')));
        assert_eq!(ids(&app.up_next), ["vid00000000"]);
        app.on_key(key(KeyCode::Char('X')));
        assert!(app.playing.is_none() && app.up_next.is_empty());
    }

    #[tokio::test]
    async fn a_mix_of_the_video_playing_plays_next_and_no_mix_goes_back() {
        let mut app = app();
        with_feed(&mut app, 3);
        let seed = app.videos[0].clone();
        let mix = vec![
            seed.clone(),
            video("aaaaaaaaaa1", CH, None),
            video("aaaaaaaaaa2", CH, None),
        ];
        app.show_playing(seed.clone(), 30.0, 200.0, true);
        app.show(View::Mix(seed.id.clone(), seed.title.clone()), false);
        app.on_event(AppEvent::Mix {
            request: app.request,
            seed: seed.id.clone(),
            result: Ok(mix),
        });
        assert_eq!(app.videos.len(), 3, "the Mix is in view");
        assert_eq!(ids(&app.up_next), ["aaaaaaaaaa1", "aaaaaaaaaa2"]);
        // A Mix of a video in it, then back: the first Mix's own videos.
        let inner = app.videos[1].clone();
        app.mixes.insert(inner.id.clone(), vec![inner.clone()]);
        app.show(View::Mix(inner.id.clone(), inner.title.clone()), false);
        assert_eq!(app.videos.len(), 1);
        app.go_back();
        assert_eq!(app.videos.len(), 3);

        let other = app.store.feed(false, 10).unwrap()[1].clone();
        app.show(View::Home, false);
        app.show(View::Mix(other.id.clone(), other.title.clone()), false);
        app.on_event(AppEvent::Mix {
            request: app.request,
            seed: other.id.clone(),
            result: Err(anyhow::anyhow!("no Mix")),
        });
        assert_eq!(app.view, View::Home, "back where m was pressed");
        assert!(app.status.as_ref().is_some_and(|s| s.error));
        assert_eq!(app.up_next.len(), 2, "what plays next is unchanged");
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
