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

use crate::chapters::{self, Chapter};
use crate::commands::{self, Command, CommandLine, JukeboxAction, Toggle};
use crate::dearrow::{self, Titles};
use crate::feed::{self, Feed};
use crate::icons::Icons;
use crate::ids::{ChannelId, ChannelName, PlaylistId, VideoId};
use crate::images::{ImageEvent, Images};
use crate::links::{self, Link};
use crate::player::{Player, PlayerEvent, PlayerEventKind, Start};
use crate::settings::{self, Settings};
use crate::sponsorblock::{self, Segment, Skipper};
use crate::store::{self, Channel, Store};
use crate::takeout::{self, Takeout};
use crate::text;
use crate::theme::{self, Colors};
use crate::tools::Tools;
use crate::video::{self, Video};
use crate::ytdlp::{
    AudioQuality, ChannelPage, MAX_DETAILS, MAX_QUERY, Quality, Streams, Tab, YtDlp,
};

/// The longest search or path typed or pasted, in characters.
const MAX_PROMPT: usize = 4096;
/// Different keys read together, with no box open to type in, that are
/// taken for a paste and ignored rather than run as commands.
const PASTED_KEYS: usize = 3;

/// Rows Ctrl-d and Ctrl-u move in the sidebar; PgDn and PgUp move twice
/// as many.
const SIDEBAR_PAGE: usize = 10;

/// Lines Ctrl-d and Ctrl-u move in the `?` popup; PgDn and PgUp move twice
/// as many.
const HELP_PAGE: isize = 5;

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
/// How long the player bar says a SponsorBlock segment was skipped.
const SKIP_SHOWN_FOR: Duration = Duration::from_secs(4);
/// How much `-` and `+` change the volume, in percent.
const VOLUME_STEP: u32 = 5;
/// The longest time typed in the chapters popup, in characters.
const MAX_TYPED_TIME: usize = 12;
/// What the chapters popup takes, said when it can't go somewhere.
const TYPE_A_TIME: &str = "Type a time: 1:23, 90, +30 or 50%";
/// Videos asked for in a Mix: YouTube's goes on for over a thousand.
const MIX_SIZE: usize = 50;
/// Videos a playlist opened from a link lists, at most.
const PLAYLIST_SIZE: usize = 200;
/// Looks for the details of videos known only by their ids, per session:
/// 50 videos each.
const MAX_DETAIL_LOOKS: usize = 100;
/// Channels whose feed was missing that one round of feeds asks yt-dlp
/// about, at most. The feed server answering 404 for everyone is an outage,
/// not a hundred removed channels, and each ask is a request YouTube counts.
const GONE_CHECKS_PER_ROUND: usize = 5;
/// Feeds failing in a round, with none loading, that make it YouTube's
/// feeds being down (Dec 2025, May and Oct 2026) rather than channels gone:
/// one feed failing all its tries is rare, this many in a row never.
const OUTAGE_AFTER: usize = 6;
/// Feeds a round tries while they're down, to see when they're back.
const PROBE: usize = 2;
/// Channels whose Videos tab fills in for their feed, per round, while the
/// feeds are down: each is a request YouTube counts.
const FILL_IN_PER_ROUND: usize = 4;

/// YouTube's feeds have been failing for every channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedsDown {
    /// YouTube's feed server answers, with errors: channels' Videos tabs
    /// fill in, with yt-dlp.
    Broken,
    /// Nothing answers: no network, or YouTube can't be reached.
    Unreachable,
}

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
    Playlist {
        request: u64,
        id: PlaylistId,
        result: Result<(String, Vec<Video>)>,
    },
    ChannelInfo {
        id: ChannelId,
        tab: Tab,
        /// The videos' dates are rough, worked out from "3 days ago".
        dated: bool,
        result: Result<ChannelPage>,
    },
    Resolved {
        request: u64,
        result: Result<Streams>,
    },
    /// SponsorBlock's segments of the playback numbered `play`.
    Segments {
        play: u64,
        result: Result<Vec<Segment>>,
    },
    /// DeArrow's title for a video.
    Title {
        id: VideoId,
        result: Result<Option<String>>,
    },
    Player(PlayerEvent),
    Image(ImageEvent),
    /// yt-dlp's version, asked for when something it did failed.
    YtDlpVersion(Result<String>),
    /// What a Takeout file, or folder, held.
    Imported(Result<Vec<Takeout>>),
    /// What YouTube says of videos known only by their ids.
    Details(Result<Vec<Video>>),
    /// A video's comments.
    Comments {
        id: VideoId,
        result: Result<Vec<crate::ytdlp::Comment>>,
    },
    /// The terminal hung up (its window closed) or tuitube was asked to end:
    /// quit the normal way, so the database is closed and yt-dlp runs are
    /// ended with everything they started.
    #[cfg_attr(not(unix), allow(dead_code))]
    Quit,
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
    /// Videos you put in to play, in order: a queue.
    Jukebox,
    /// A video from a link pasted in, to play.
    Link(VideoId),
    /// A playlist from a link, and its title.
    Playlist(PlaylistId, String),
    /// One of your playlists (imported from Takeout), and its name.
    MyPlaylist(i64, String),
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
            View::Jukebox => "Jukebox".into(),
            View::Link(_) => "From a link".into(),
            View::Playlist(_, title) => format!("Playlist – {title}"),
            View::MyPlaylist(_, name) => name.clone(),
        }
    }
}

/// A row of the sidebar that can be selected.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Entry {
    Home,
    Shorts,
    Search,
    Jukebox,
    WatchLater,
    History,
    /// One of your playlists, by its place in [`App::my_playlists`].
    MyPlaylist(usize),
    /// A subscription, by its place in [`App::subscriptions`].
    Channel(usize),
}

pub const MENU: [Entry; 6] = [
    Entry::Home,
    Entry::Shorts,
    Entry::Search,
    Entry::Jukebox,
    Entry::WatchLater,
    Entry::History,
];

/// The sidebar's menu with Shorts turned off in the settings.
const MENU_WITHOUT_SHORTS: [Entry; 5] = [
    Entry::Home,
    Entry::Search,
    Entry::Jukebox,
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

/// The tabs of the `?` popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpTab {
    Keys,
    Settings,
}

/// The `?` popup: every key, and the settings (`O` opens it there). On the
/// settings tab, Enter or Space turns the setting under the cursor on or off,
/// or picks the theme under it, and saves it at once.
pub struct HelpMenu {
    pub tab: HelpTab,
    /// The first line shown on the keys tab. Drawing keeps it in range.
    pub scroll: usize,
    /// The row on the settings tab: one of the checkboxes, then the themes
    /// from [`HelpMenu::THEMES`] on.
    pub selected: usize,
    /// The first line shown on the settings tab. Drawing moves it to show
    /// the selected row.
    pub settings_scroll: usize,
}

impl HelpMenu {
    /// The "Shorts" row, the first one.
    pub const SHORTS: usize = 0;
    /// The "live streams and premieres" row.
    pub const LIVE: usize = Self::SHORTS + 1;
    /// The "start of the description" row.
    pub const DESCRIPTIONS: usize = Self::LIVE + 1;
    pub const AUTOPLAY: usize = Self::DESCRIPTIONS + 1;
    pub const SPONSORBLOCK: usize = Self::AUTOPLAY + 1;
    pub const DEARROW: usize = Self::SPONSORBLOCK + 1;
    pub const SUBTITLES: usize = Self::DEARROW + 1;
    /// The picture's rows, one per height in [`settings::HEIGHTS`],
    /// tallest first.
    pub const VIDEO_QUALITY: usize = Self::SUBTITLES + 1;
    /// The sound's rows, one per [`AudioQuality`], best first.
    pub const AUDIO_QUALITY: usize = Self::VIDEO_QUALITY + settings::HEIGHTS.len();
    /// The Jukebox's rows: as a list, and taking off what has played.
    pub const JUKEBOX_LIST: usize = Self::AUDIO_QUALITY + AudioQuality::ALL.len();
    pub const JUKEBOX_AUTOREMOVE: usize = Self::JUKEBOX_LIST + 1;
    pub const HISTORY: usize = Self::JUKEBOX_AUTOREMOVE + 1;
    /// The first theme's row. The themes come last, a long list.
    pub const THEMES: usize = Self::HISTORY + 1;
    /// The last row: the last theme.
    pub const LAST: usize = Self::THEMES + theme::BUILT_IN.len() - 1;

    /// Whether `row` picks one of several (a quality, a theme) rather than
    /// turning something on or off.
    pub fn is_choice(row: usize) -> bool {
        (Self::VIDEO_QUALITY..Self::JUKEBOX_LIST).contains(&row) || row >= Self::THEMES
    }

    pub fn new(tab: HelpTab) -> Self {
        Self {
            tab,
            scroll: 0,
            selected: 0,
            settings_scroll: 0,
        }
    }
}

/// The chapters popup (`t`): the chapters of the video playing, to go to
/// one, and a time typed to go to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChapterMenu {
    /// The chapter under the cursor.
    pub selected: usize,
    /// The first chapter shown. Drawing moves it to show the selected one.
    pub scroll: usize,
    /// A time being typed: `1:23`, `+30`, `50%`. Enter goes there instead
    /// of to the selected chapter.
    pub typed: String,
}

/// The `i` popup: a video's whole description, or its top comments.
#[derive(Clone, Debug, PartialEq)]
pub struct InfoPopup {
    pub video: Video,
    pub tab: InfoTab,
    /// The description's line the cursor is on: Enter goes to the time in
    /// it.
    pub line: usize,
    /// The first row shown of the comments, and the description's.
    pub scroll: usize,
    pub description_scroll: Cell<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoTab {
    Description,
    Comments,
}

/// A video's comments, as far as they've come.
#[derive(Clone, Debug, PartialEq)]
pub enum Comments {
    Loading,
    Loaded(Vec<crate::ytdlp::Comment>),
    Failed(String),
}

/// A question asked before doing what can't be undone: `y` does it, any
/// other key doesn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Empty the Jukebox.
    ClearJukebox,
    /// Delete one of your playlists.
    DeletePlaylist(i64),
}

/// How a video in the Jukebox is doing, for its mark there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JukeboxState {
    Loading,
    Playing,
    Paused,
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
    /// Played from the Jukebox, which goes on after it.
    from_jukebox: bool,
    /// SponsorBlock's segments of it, skipped as playback reaches them.
    skipper: Skipper,
    /// The last segment skipped, said in the player bar for a moment.
    skip: Option<Skip>,
    /// Its chapters, in order; none for most videos.
    pub chapters: Vec<Chapter>,
    /// mpv has its chapters, for its own window.
    chapters_sent: bool,
    /// The picture's lines (or the sound's bitrate) playing.
    pub quality: Option<String>,
    /// Its subtitles' name ("English"), if it has some in your languages:
    /// shown or not as `subtitles` says.
    pub subtitles: Option<String>,
}

/// A SponsorBlock segment skipped.
pub struct Skip {
    pub category: &'static str,
    /// How much was skipped, in seconds.
    pub seconds: f64,
    at: Instant,
}

/// A video being looked up to play.
pub struct Resolving {
    request: u64,
    pub video: Video,
    audio_only: bool,
    /// Played because the video before it ended, not asked for.
    auto: bool,
    /// Played from the Jukebox, which goes on after it.
    from_jukebox: bool,
    /// Played again at another quality: it carries on from where the one
    /// playing is when the new streams come.
    carry_on: bool,
}

impl Playing {
    pub fn controllable(&self) -> bool {
        self.player.controllable()
    }

    /// The segment just skipped, while the player bar says so.
    pub fn shown_skip(&self) -> Option<&Skip> {
        self.skip
            .as_ref()
            .filter(|skip| skip.at.elapsed() < SKIP_SHOWN_FOR)
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
    /// SponsorBlock's (and DeArrow's) own client: the same, but following
    /// no redirect.
    sponsor_http: reqwest::Client,
    tx: UnboundedSender<AppEvent>,
    pub images: Images,

    pub subscriptions: Vec<Channel>,
    /// Your playlists, imported from Takeout.
    pub my_playlists: Vec<store::Playlist>,
    /// Videos on screen known only by their ids, for [`App::fetch_details`],
    /// those asked about this session, and whether a look is going.
    details_wanted: Vec<VideoId>,
    details_asked: HashSet<VideoId>,
    details_looking: bool,
    detail_looks: usize,
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
    /// Background answers changed stored videos: reload after this batch.
    needs_reload: bool,
    /// DeArrow's titles, asked for as cards come on screen while it's on.
    titles: Titles,

    pub view: View,
    pub videos: Vec<Video>,
    search_results: Vec<Video>,
    search_count: usize,
    /// Mixes loaded this session, by the video each is of.
    mixes: HashMap<VideoId, Vec<Video>>,
    /// Playlists opened from links this session.
    playlists: HashMap<PlaylistId, Vec<Video>>,
    /// The video of the last link pasted in, as much as the link says,
    /// until it's played (and YouTube says the rest).
    link: Option<Video>,
    /// Where the next playing of a video starts, once: a link's `t=`, or a
    /// time in its description.
    start_once: Option<(VideoId, f64)>,
    /// The `i` popup: the selected video's description or comments.
    pub info_popup: Option<InfoPopup>,
    /// Comments loaded this session, by video.
    pub comments: HashMap<VideoId, Comments>,
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
    /// The `?` popup, while it's open.
    pub help: Option<HelpMenu>,
    /// A question waiting for `y`, in a popup.
    pub confirm: Option<Confirm>,
    /// The chapters popup, while it's open.
    pub chapter_menu: Option<ChapterMenu>,
    /// The command line (`:`), while it's open.
    pub command: Option<CommandLine>,

    /// What the grid is waiting for, said in its header.
    pub loading: Option<String>,
    /// Feeds fetched and to fetch, while they're being fetched.
    pub refreshing: Option<(usize, usize)>,
    /// When the last fetch of feeds ended, so failed ones wait for the next
    /// round instead of being retried at once.
    refreshed: Option<Instant>,
    feed_failures: usize,
    /// This round's feeds that loaded, that failed with an answer from
    /// YouTube, and that were missing (404, on every try).
    round_loaded: usize,
    round_answered: usize,
    round_missing: Vec<ChannelId>,
    /// This round's limit on feeds fetched at once: closing it calls off
    /// the rest of the round.
    round_limit: Option<Arc<Semaphore>>,
    /// The round only tries whether the feeds are back.
    round_probe: bool,
    probes: usize,
    /// YouTube's feeds are failing for every channel: each round tries only
    /// `PROBE` of them, and channels' Videos tabs fill in, a few a round.
    pub feeds_down: Option<FeedsDown>,
    /// When each channel's Videos tab last filled in for its feed.
    filled_in: HashMap<ChannelId, Instant>,
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
    /// from, or the rest of its Mix. The Jukebox plays first.
    pub up_next: VecDeque<Video>,
    /// The Jukebox, in the order it plays. It stays as it plays, unless
    /// `jukebox_autoremove` takes off each video that played to its end.
    pub jukebox: Vec<Video>,
    /// Where the Jukebox is: the video last picked to play from it. It goes
    /// on from the one after; from its start while this is `None`.
    jukebox_at: Option<VideoId>,
    /// Videos autoplay skipped in a row because they couldn't be played.
    skipped: u32,
    /// Muted (`M`), for this session.
    pub muted: bool,
    /// The volume changed since it was last saved: it's saved on the next
    /// tick, so a key held down writes settings.toml once.
    volume_unsaved: bool,
    /// yt-dlp's version, once asked for (after something it did failed).
    yt_dlp_version: Option<String>,
    version_asked: bool,
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
            sponsor_http: feed::client_without_redirects(),
            tx,
            images,
            subscriptions: Vec::new(),
            my_playlists: Vec::new(),
            details_wanted: Vec::new(),
            details_asked: HashSet::new(),
            details_looking: false,
            detail_looks: 0,
            avatars: HashMap::new(),
            channel_lookups: 0,
            checked: HashMap::new(),
            in_flight: HashSet::new(),
            lookups_per_channel: HashMap::new(),
            gone_checks: HashSet::new(),
            needs_reload: false,
            titles: Titles::default(),
            view: View::Home,
            videos: Vec::new(),
            search_results: Vec::new(),
            search_count: SEARCH_PAGE,
            mixes: HashMap::new(),
            playlists: HashMap::new(),
            link: None,
            start_once: None,
            info_popup: None,
            comments: HashMap::new(),
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
            help: None,
            confirm: None,
            chapter_menu: None,
            command: None,
            loading: None,
            refreshing: None,
            refreshed: None,
            feed_failures: 0,
            round_loaded: 0,
            round_answered: 0,
            round_missing: Vec::new(),
            round_limit: None,
            round_probe: false,
            probes: 0,
            feeds_down: None,
            filled_in: HashMap::new(),
            feeds_updated: None,
            feeds_paused: None,
            lookups_paused: None,
            status: None,
            playing: None,
            resolving: None,
            up_next: VecDeque::new(),
            jukebox: Vec::new(),
            jukebox_at: None,
            skipped: 0,
            muted: false,
            volume_unsaved: false,
            yt_dlp_version: None,
            version_asked: false,
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
        app.jukebox = app.store.jukebox().unwrap_or_default();
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
        #[cfg(unix)]
        quit_on_hangup(self.tx.clone())?;
        self.refresh_feeds(false);
        loop {
            terminal.draw(|frame| crate::ui::draw(frame, self))?;
            self.images.fetch();
            self.fetch_details();
            self.fetch_titles();
            if self.quit {
                break;
            }
            tokio::select! {
                event = keys.next() => match event {
                    Some(Ok(event)) => {
                        // Whatever else is already waiting is read with it:
                        // a paste that comes as keystrokes arrives at once.
                        let mut events = vec![event];
                        while events.len() < MAX_PROMPT
                            && crossterm::event::poll(Duration::ZERO).unwrap_or(false)
                        {
                            match crossterm::event::read() {
                                Ok(event) => events.push(event),
                                Err(_) => break,
                            }
                        }
                        self.on_terminal_events(events);
                    }
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
        self.save_volume();
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
        self.save_volume();
    }

    // Lists.

    fn reload_subscriptions(&mut self) {
        self.subscriptions = self.store.subscriptions().unwrap_or_default();
        self.my_playlists = self.store.playlists().unwrap_or_default();
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
            View::Jukebox => Ok(self.jukebox.clone()),
            View::Link(id) => Ok(self
                .store
                .video(id)
                .ok()
                .flatten()
                .or_else(|| self.link.clone().filter(|v| v.id == *id))
                .into_iter()
                .collect()),
            View::Playlist(id, _) => Ok(self.playlists.get(id).cloned().unwrap_or_default()),
            View::MyPlaylist(id, _) => self.store.playlist_videos(*id),
        };
        self.videos = videos.unwrap_or_default();
        // Your own lists show what you put there, whatever the settings say.
        if !matches!(
            self.view,
            View::WatchLater | View::History | View::Jukebox | View::MyPlaylist(..)
        ) {
            self.videos.retain(|v| !self.settings.hides(v));
        }
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
            View::Jukebox => Some(Entry::Jukebox),
            View::WatchLater => Some(Entry::WatchLater),
            View::History => Some(Entry::History),
            View::Channel(id, _) => self
                .subscriptions
                .iter()
                .position(|c| c.id == *id)
                .map(Entry::Channel),
            View::MyPlaylist(id, _) => self
                .my_playlists
                .iter()
                .position(|p| p.id == *id)
                .map(Entry::MyPlaylist),
            View::Mix(..) | View::Link(_) | View::Playlist(..) => None,
        };
        if let Some(i) = entry.and_then(|e| (0..self.sidebar_len()).find(|&i| self.entry(i) == e)) {
            self.sidebar_selected = i;
        }
    }

    /// The sidebar's menu: Shorts only if they're shown.
    pub fn menu(&self) -> &'static [Entry] {
        if self.settings.shorts {
            &MENU
        } else {
            &MENU_WITHOUT_SHORTS
        }
    }

    pub fn sidebar_len(&self) -> usize {
        self.menu().len() + self.my_playlists.len() + self.subscriptions.len()
    }

    /// The sidebar's `i`th row: the menu, your playlists, then your
    /// subscriptions.
    pub fn entry(&self, i: usize) -> Entry {
        let menu = self.menu();
        if let Some(entry) = menu.get(i) {
            return *entry;
        }
        let i = i - menu.len();
        if i < self.my_playlists.len() {
            Entry::MyPlaylist(i)
        } else {
            Entry::Channel(i - self.my_playlists.len())
        }
    }

    /// The view a sidebar row shows.
    fn entry_view(&self, entry: Entry) -> Option<View> {
        Some(match entry {
            Entry::Home => View::Home,
            Entry::Shorts => View::Shorts,
            Entry::Search => View::Search(self.last_query()),
            Entry::Jukebox => View::Jukebox,
            Entry::WatchLater => View::WatchLater,
            Entry::History => View::History,
            Entry::MyPlaylist(i) => {
                let p = self.my_playlists.get(i)?;
                View::MyPlaylist(p.id, p.name.clone())
            }
            Entry::Channel(i) => {
                let c = self.subscriptions.get(i)?;
                View::Channel(c.id.clone(), c.title.clone())
            }
        })
    }

    pub(crate) fn last_query(&self) -> String {
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

    /// A popup is open over the videos: the `?` one, the chapters, the
    /// command line's completions, or a question.
    pub fn popup_open(&self) -> bool {
        self.help.is_some()
            || self.info_popup.is_some()
            || self.confirm.is_some()
            || self.chapter_menu.is_some()
            || self.command.is_some()
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
        self.look_up_channel(id, Tab::Videos, false);
        None
    }

    /// Asks yt-dlp, in the background, for a tab of the channel's page: its
    /// photo and newest videos, with the lengths feeds don't give (and, if
    /// `dated`, rough dates). Whether it asked: not past the session's
    /// limits, or while paused.
    fn look_up_channel(&mut self, id: &ChannelId, tab: Tab, dated: bool) -> bool {
        let Some(yt) = self.yt.clone() else {
            return false;
        };
        // A pause that has run out is no pause: the next slow-down signal
        // starts a new one.
        if self
            .lookups_paused
            .is_some_and(|until| Instant::now() >= until)
        {
            self.lookups_paused = None;
        }
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
            let result = yt.channel(&id, tab, CHANNEL_LOOKUP, true, dated).await;
            let _ = tx.send(AppEvent::ChannelInfo {
                id,
                tab,
                dated,
                result,
            });
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

    /// A card on screen is known only by its id (from Takeout): it's asked
    /// about once the frame is drawn, with the others on screen. Not a
    /// link's: nothing is asked of YouTube about it until it's played.
    pub fn want_details(&mut self, video: &Video) {
        if video.needs_details()
            && !matches!(self.view, View::Link(_))
            && !self.details_asked.contains(&video.id)
            && !self.details_wanted.contains(&video.id)
        {
            self.details_wanted.push(video.id.clone());
        }
    }

    /// A card on screen: with DeArrow on, its title is asked for once the
    /// frame is drawn, once a session.
    pub fn want_title(&mut self, video: &Video) {
        if self.settings.dearrow && !self.demo && !video.needs_details() {
            self.titles.want(&video.id);
        }
    }

    /// Asks DeArrow for the titles of the cards on screen, a few at a time.
    fn fetch_titles(&mut self) {
        if !self.settings.dearrow {
            return;
        }
        for id in self.titles.take_wanted() {
            let (http, tx) = (self.sponsor_http.clone(), self.tx.clone());
            tokio::spawn(async move {
                let result = dearrow::title(&http, &id).await;
                let _ = tx.send(AppEvent::Title { id, result });
            });
        }
    }

    /// The cards DeArrow is to be asked about now.
    #[cfg(test)]
    pub fn titles_wanted(&mut self) -> Vec<VideoId> {
        self.titles.take_wanted()
    }

    /// The title shown for `video`: DeArrow's, while it's on and has one,
    /// else YouTube's.
    pub fn title<'a>(&'a self, video: &'a Video) -> &'a str {
        self.settings
            .dearrow
            .then(|| self.titles.get(&video.id))
            .flatten()
            .unwrap_or(&video.title)
    }

    /// Asks yt-dlp about the videos on screen known only by their ids, 50
    /// at a time, one look at a time, a few a session; not while YouTube
    /// asks tuitube to slow down.
    fn fetch_details(&mut self) {
        let wanted = std::mem::take(&mut self.details_wanted);
        let paused = self
            .lookups_paused
            .is_some_and(|until| Instant::now() < until);
        if wanted.is_empty()
            || self.details_looking
            || self.detail_looks >= MAX_DETAIL_LOOKS
            || paused
            || self.demo
        {
            return;
        }
        let Some(yt) = self.yt.clone() else {
            return;
        };
        let ids: Vec<VideoId> = wanted.into_iter().take(MAX_DETAILS).collect();
        self.details_asked.extend(ids.iter().cloned());
        self.details_looking = true;
        self.detail_looks += 1;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(AppEvent::Details(yt.details(&ids).await));
        });
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
                self.look_up_channel(&id, tab, false);
                return;
            }
        }
    }

    // Feeds.

    /// Fetches the feeds of subscriptions not fetched lately, or all of
    /// them if `all`; while they're down, only a few ([`App::round_of`]).
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
        let due = self.round_of(due);
        if due.is_empty() {
            self.refreshed = Some(Instant::now());
            if all {
                self.info("No subscriptions to update");
            }
            return;
        }
        let limit = self.start_round(due.len());
        for channel in due {
            let (tx, http, limit) = (self.tx.clone(), self.http.clone(), limit.clone());
            tokio::spawn(async move {
                let result = feed::fetch(&http, &limit, &channel).await;
                let _ = tx.send(AppEvent::Feed { channel, result });
            });
        }
    }

    /// The feeds a round fetches: every one due, or while they're down,
    /// `PROBE` of them, a different few each round (a gone channel's 404
    /// mustn't look like the outage going on).
    fn round_of(&mut self, mut due: Vec<ChannelId>) -> Vec<ChannelId> {
        self.round_probe = self.feeds_down.is_some() && !due.is_empty();
        if self.round_probe {
            let turn = self.probes * PROBE % due.len();
            due.rotate_left(turn);
            due.truncate(PROBE);
            self.probes += 1;
        }
        due
    }

    /// Starts counting a round of `feeds` feeds; its limit on how many are
    /// fetched at once.
    fn start_round(&mut self, feeds: usize) -> Arc<Semaphore> {
        self.refreshing = Some((0, feeds));
        self.feed_failures = 0;
        self.round_loaded = 0;
        self.round_answered = 0;
        self.round_missing.clear();
        let limit = Arc::new(Semaphore::new(feed::PARALLEL));
        self.round_limit = Some(limit.clone());
        limit
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
                self.round_loaded += 1;
                if self.feeds_down.take().is_some() {
                    self.filled_in.clear();
                    self.info("YouTube's feeds are back");
                }
            }
            // Not tried: says nothing about the channel.
            Err(e) if e.is::<feed::CalledOff>() => {}
            Err(e) if e.is::<feed::TooManyRequests>() => {
                if self.feeds_paused.is_none() {
                    self.feeds_paused = Some(Instant::now() + SLOW_DOWN);
                    self.error("YouTube asked tuitube to slow down: feeds wait an hour. Playing still works");
                }
            }
            Err(e) => {
                self.feed_failures += 1;
                if feed::answered(&e) {
                    self.round_answered += 1;
                }
                // Missing on every try: yt-dlp can tell, once the round is
                // over, whether the channel is gone.
                if e.is::<feed::NotFound>() {
                    self.round_missing.push(channel.clone());
                }
            }
        }
        // Nothing loads: the feeds are down, not each channel. The rest of
        // the round is called off, not tried four times each.
        if self.feeds_down.is_none() && self.round_loaded == 0 && self.feed_failures >= OUTAGE_AFTER
        {
            let down = if self.round_answered > 0 {
                FeedsDown::Broken
            } else {
                FeedsDown::Unreachable
            };
            self.feeds_down = Some(down);
            if let Some(limit) = &self.round_limit {
                limit.close();
            }
            self.error(match down {
                FeedsDown::Broken => {
                    "YouTube's feeds are down: Home keeps what it has, and channels fill in with yt-dlp, a few at a time"
                }
                FeedsDown::Unreachable => {
                    "YouTube's feeds can't be reached: is the network down? Home keeps what it has"
                }
            });
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
            self.round_limit = None;
            // Back after an outage: the rest are fetched now, not in
            // `CHECK_EVERY`.
            let back = self.round_probe && self.round_loaded > 0;
            self.refreshed = (!back).then(Instant::now);
            self.feeds_updated = self.store.feeds_updated().ok().flatten();
            // Still down: whether YouTube answers may have changed (the
            // network came back to feeds still broken).
            if self.round_probe && self.feeds_down.is_some() && self.feed_failures > 0 {
                self.feeds_down = Some(if self.round_answered > 0 {
                    FeedsDown::Broken
                } else {
                    FeedsDown::Unreachable
                });
            }
            match self.feeds_down {
                None => {
                    self.check_gone();
                    if self.feed_failures > 0 {
                        self.error(format!(
                            "{} of {total} channels didn't load: YouTube's feed server is flaky. R tries again",
                            self.feed_failures
                        ));
                    }
                }
                Some(FeedsDown::Broken) => self.fill_in(),
                Some(FeedsDown::Unreachable) => {}
            }
        }
    }

    /// Asks yt-dlp whether the channels whose feed was missing this round
    /// are gone: once a session per channel, a few per round.
    fn check_gone(&mut self) {
        let mut asked = 0;
        for channel in std::mem::take(&mut self.round_missing) {
            if asked == GONE_CHECKS_PER_ROUND {
                break;
            }
            if !self.gone_checks.contains(&channel)
                && self.look_up_channel(&channel, Tab::Videos, false)
            {
                self.gone_checks.insert(channel);
                asked += 1;
            }
        }
    }

    /// While YouTube's feeds are down, the Videos tabs of a few channels,
    /// the most active first, fill in for their feeds: Home still gets new
    /// videos, dated roughly ("3 days ago"). Each channel again once its
    /// feed would be due.
    fn fill_in(&mut self) {
        let every = Duration::from_secs(self.settings.refresh_every().max(0) as u64);
        let mut asked = 0;
        for id in self.store.most_active().unwrap_or_default() {
            if asked == FILL_IN_PER_ROUND {
                break;
            }
            let recent = self
                .filled_in
                .get(&id)
                .is_some_and(|at| at.elapsed() < every);
            if !recent && self.look_up_channel(&id, Tab::Videos, true) {
                self.filled_in.insert(id, Instant::now());
                asked += 1;
            }
        }
    }

    // What failed.

    /// A yt-dlp error as shown: in tuitube's words when YouTube refused on
    /// purpose (a sign-in, a bot check), else as yt-dlp said it, with a word
    /// about updating yt-dlp if it's old.
    fn explain(&mut self, error: &anyhow::Error) -> String {
        let error = format!("{error:#}");
        if let Some(reason) = refusal(&error) {
            return reason.into();
        }
        let hint = self.update_hint(&error);
        error + &hint
    }

    /// For a failure YouTube didn't explain: " · yt-dlp came out 3 months
    /// ago…" if it's old enough that updating it may fix this. The first
    /// time, its version is asked for, and added to the message when it
    /// comes.
    fn update_hint(&mut self, error: &str) -> String {
        if youtube_said(error) {
            return String::new();
        }
        match self.yt_dlp_version.clone() {
            Some(version) => self
                .old_yt_dlp(&version)
                .map(|notice| format!(" · {notice}"))
                .unwrap_or_default(),
            None => {
                self.ask_version();
                String::new()
            }
        }
    }

    fn ask_version(&mut self) {
        let Some(yt) = self.yt.clone().filter(|_| !self.version_asked) else {
            return;
        };
        self.version_asked = true;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(AppEvent::YtDlpVersion(yt.version().await));
        });
    }

    /// What to say about yt-dlp `version` if it's old.
    fn old_yt_dlp(&self, version: &str) -> Option<String> {
        let path = self.tools.yt_dlp.as_deref()?;
        crate::ytdlp::old_notice(version, path, chrono::Local::now().date_naive())
    }

    // Searching and channels.

    fn search(&mut self, query: &str, count: usize) {
        // A YouTube link typed or pasted in is opened, not searched for.
        if count == SEARCH_PAGE
            && let Some(link) = links::parse(query)
        {
            return self.open_link(link);
        }
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

    /// Opens what a link is to: a video, as a card to play (nothing is
    /// asked of YouTube until it's played), a playlist or a channel.
    fn open_link(&mut self, link: Link) {
        if self.demo {
            return self.info(DEMO);
        }
        match link {
            Link::Video { id, start, short } => {
                let mut video = self
                    .store
                    .video(&id)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| Video::placeholder(id.clone(), short));
                video.short |= short;
                self.start_once = start.map(|at| (id.clone(), f64::from(at)));
                let from = start.map_or(String::new(), |at| {
                    format!(" from {}", video::duration(at))
                });
                self.link = Some(video);
                self.show(View::Link(id), false);
                self.focus = Focus::Grid;
                self.info(format!("Enter plays it{from}, a plays its sound"));
            }
            Link::Playlist(id) => {
                self.show(View::Playlist(id.clone(), "Loading…".into()), false);
                self.focus = Focus::Grid;
                self.fetch_playlist(id);
            }
            Link::Channel(id) => {
                let name = self
                    .store
                    .channel(&id)
                    .ok()
                    .flatten()
                    .map_or_else(|| "Channel".into(), |c| c.title);
                self.show(View::Channel(id, name), true);
                self.focus = Focus::Grid;
                self.sync_sidebar();
            }
            Link::Named(name) => self.fetch_named_channel(name),
            Link::AccountList => self.error(
                "That list is your YouTube account's: tuitube keeps its own, Watch later (w) and History",
            ),
        }
    }

    fn fetch_playlist(&mut self, id: PlaylistId) {
        let Some(yt) = self.yt.clone() else {
            return self.error(self.tools.missing().unwrap_or_default());
        };
        self.request += 1;
        let request = self.request;
        self.loading = Some("Loading the playlist…".into());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = yt.playlist(&id, PLAYLIST_SIZE).await;
            let _ = tx.send(AppEvent::Playlist {
                request,
                id,
                result,
            });
        });
    }

    /// A channel a link names by handle: yt-dlp says which it is, and its
    /// page opens when it answers.
    fn fetch_named_channel(&mut self, name: ChannelName) {
        let Some(yt) = self.yt.clone() else {
            return self.error(self.tools.missing().unwrap_or_default());
        };
        self.request += 1;
        let request = self.request;
        self.loading = Some(format!("Opening {name}…"));
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = yt.named_channel(&name, 60).await;
            let _ = tx.send(AppEvent::Channel { request, result });
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
            let result = yt.channel(&id, Tab::Videos, 60, false, false).await;
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
                    self.up_next = playable(
                        videos
                            .iter()
                            .filter(|v| v.id != seed && !self.settings.hides(v)),
                    );
                    self.info(if self.jukebox_next().is_some() {
                        "Up next: the Jukebox, then the rest of the Mix"
                    } else {
                        "Up next: the rest of the Mix"
                    });
                }
                self.mixes.insert(seed, videos);
                self.reload();
            }
            Err(e) => {
                if matches!(&self.view, View::Mix(id, _) if *id == seed) {
                    self.go_back();
                }
                let shown = self.explain(&e);
                self.error(format!("No Mix: {shown}"));
            }
        }
    }

    /// Near the end of the search results: ask for more.
    fn load_more(&mut self) {
        let View::Search(query) = &self.view else {
            return;
        };
        let near_end = self.selected + self.grid.get().cols * 2 >= self.videos.len();
        // What came back, not what's shown: the settings can hide some.
        let full = self.search_results.len() >= self.search_count;
        if near_end && full && self.loading.is_none() && self.search_count < MAX_SEARCH {
            let query = query.clone();
            self.search(&query, self.search_count + SEARCH_PAGE);
        }
    }

    // Playing.

    /// Plays the selected video, then the cards after it, one by one. In
    /// the Jukebox, the Jukebox plays on after it.
    fn play_selected(&mut self, audio_only: bool) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        let from_jukebox = self.view == View::Jukebox;
        self.up_next = if from_jukebox {
            VecDeque::new()
        } else {
            playable(self.videos.iter().skip(self.selected + 1))
        };
        self.play(video, audio_only, false, from_jukebox);
    }

    /// The place in the Jukebox of the video last picked from it, if it's
    /// still there.
    fn jukebox_place(&self) -> Option<usize> {
        let at = self.jukebox_at.as_ref()?;
        self.jukebox.iter().position(|v| v.id == *at)
    }

    /// The Jukebox's next video: the first after the one last picked from
    /// it that can play (not a premiere or live stream that hasn't
    /// started).
    fn jukebox_next(&self) -> Option<&Video> {
        let from = self.jukebox_place().map_or(0, |i| i + 1);
        self.jukebox[from..].iter().find(|v| !v.upcoming)
    }

    /// The Jukebox goes back one: the video it was at plays next.
    fn jukebox_step_back(&mut self) {
        let before = self.jukebox_place().and_then(|i| i.checked_sub(1));
        self.jukebox_at = before.map(|i| self.jukebox[i].id.clone());
    }

    /// Whether `id` is playing or loading from the Jukebox, for its mark.
    pub fn jukebox_state(&self, id: &VideoId) -> Option<JukeboxState> {
        if self
            .resolving
            .as_ref()
            .is_some_and(|r| r.from_jukebox && r.video.id == *id)
        {
            return Some(JukeboxState::Loading);
        }
        let playing = self
            .playing
            .as_ref()
            .filter(|p| p.from_jukebox && p.video.id == *id)?;
        Some(if playing.paused {
            JukeboxState::Paused
        } else {
            JukeboxState::Playing
        })
    }

    /// What N plays: the Jukebox's next video, else the first up next.
    pub fn next_video(&self) -> Option<&Video> {
        self.jukebox_next().or(self.up_next.front())
    }

    /// What plays when the video playing ends: the Jukebox's next video
    /// whatever autoplay says (you put it there), else the first up next
    /// with autoplay on.
    pub fn plays_next(&self) -> Option<&Video> {
        self.jukebox_next()
            .or(self.up_next.front().filter(|_| self.settings.autoplay))
    }

    /// Plays the Jukebox's next video, else the first up next (only with
    /// autoplay on, if `auto`: the video before it ended). Whether there was
    /// one.
    fn play_next(&mut self, audio_only: bool, auto: bool) -> bool {
        let (video, from_jukebox) = match self.jukebox_next() {
            Some(video) => (video.clone(), true),
            None if auto && !self.settings.autoplay => return false,
            None => match self.up_next.pop_front() {
                Some(video) => (video, false),
                None => return false,
            },
        };
        self.play(video, audio_only, auto, from_jukebox);
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

    fn play(&mut self, video: Video, audio_only: bool, auto: bool, from_jukebox: bool) {
        if self.demo {
            return self.info(DEMO);
        }
        let (Some(yt), Some(_)) = (self.yt.clone(), self.tools.mpv.as_ref()) else {
            return self.error(self.tools.missing().unwrap_or_default());
        };
        // The Jukebox goes on from here, played or not: autoplay skips one
        // that can't be played.
        if from_jukebox {
            self.jukebox_at = Some(video.id.clone());
        }
        // It doesn't play again from up next.
        self.up_next.retain(|v| v.id != video.id);
        self.resolves += 1;
        let request = self.resolves;
        let id = video.id.clone();
        self.resolving = Some(Resolving {
            request,
            video,
            audio_only,
            auto,
            from_jukebox,
            carry_on: false,
        });
        let sound = self.settings.audio_quality;
        let quality = if audio_only {
            Quality::AudioOnly(sound)
        } else {
            Quality::Video(self.settings.max_height(), sound)
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = yt.resolve(&id, quality).await;
            let _ = tx.send(AppEvent::Resolved { request, result });
        });
    }

    fn on_play_failed(&mut self, resolving: Resolving, error: anyhow::Error) {
        let shown = self.explain(&error);
        let error = format!("{error:#}");
        self.error(format!("Can't play “{}”: {shown}", resolving.video.title));
        // One in the list that can't be played (removed, private) is
        // skipped, a few in a row at most; a bot check stops autoplay.
        if resolving.auto && self.skipped < MAX_SKIPS && !slowed_down(&error) {
            self.skipped += 1;
            self.play_next(resolving.audio_only, true);
        }
    }

    fn on_resolved(&mut self, resolving: Resolving, streams: Streams) {
        let Resolving {
            video,
            audio_only,
            from_jukebox,
            carry_on,
            ..
        } = resolving;
        // What YouTube says about the video now fills in the card (a link's
        // has only its id), merged as every source is.
        let video = match &streams.info {
            Some(info) => {
                let _ = self.store.save_videos(std::slice::from_ref(&video));
                let _ = self.store.save_videos(std::slice::from_ref(&**info));
                if matches!(self.view, View::Link(_)) {
                    self.reload();
                }
                self.store.video(&video.id).ok().flatten().unwrap_or(video)
            }
            None => video,
        };
        // A link's start, the first time it's played.
        let start_once = self
            .start_once
            .take_if(|(id, _)| *id == video.id && !carry_on)
            .map(|(_, at)| at);
        // Played again at another quality: from where it is now. A live
        // stream goes on from where it's live.
        let here = self
            .playing
            .as_ref()
            .filter(|p| carry_on && p.video.id == video.id && !video.live && p.position > 0.0)
            .map(|p| p.position);
        self.stop_playing();
        let Some(mpv) = self.tools.mpv.clone() else {
            return;
        };
        let start_at = here.or(start_once).or_else(|| {
            self.store
                .progress(&video.id)
                .ok()
                .flatten()
                .filter(|&(at, length)| at > RESUME_AFTER && at < length - RESUME_AFTER)
                .map(|(at, _)| at)
        });
        self.plays += 1;
        // Subtitles in the first of your languages the video has them in;
        // none for sound alone.
        let languages = self.settings.subtitle_languages();
        let caption = crate::ytdlp::pick_caption(&streams.captions, &languages)
            .filter(|_| !audio_only)
            .cloned();
        let title = self.title(&video).to_string();
        let start = Start {
            mpv: &mpv,
            tools: &self.tools,
            streams: &streams,
            title: &title,
            start_at,
            audio_only,
            subtitles: caption.as_ref().map(|c| c.url.as_str()),
            subtitles_shown: self.settings.subtitles,
            volume: self.settings.volume(),
            muted: self.muted,
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
                self.info(format!("{what} “{title}”{from}"));
                self.playing = Some(Playing {
                    player,
                    video,
                    position: start_at.unwrap_or(0.0),
                    duration: streams.duration,
                    paused: false,
                    audio_only,
                    saved: Instant::now(),
                    ended: false,
                    from_jukebox,
                    skipper: Skipper::default(),
                    skip: None,
                    chapters: streams.chapters,
                    chapters_sent: false,
                    quality: streams.quality,
                    subtitles: caption.map(|c| c.name),
                });
                self.look_up_segments();
            }
            Err(e) => self.error(format!("mpv didn't start: {e}")),
        }
    }

    /// Asks SponsorBlock what to skip in the video playing, if it's on and
    /// tuitube can seek in mpv (not on Windows yet). A live stream has
    /// nothing marked yet.
    fn look_up_segments(&mut self) {
        let Some(playing) = &self.playing else {
            return;
        };
        let categories = self.settings.skip_categories();
        if !self.settings.sponsorblock
            || categories.is_empty()
            || !playing.player.has_mpv()
            || !playing.controllable()
            || playing.video.live
        {
            return;
        }
        let (play, id) = (playing.player.play, playing.video.id.clone());
        let (http, tx) = (self.sponsor_http.clone(), self.tx.clone());
        tokio::spawn(async move {
            let result = sponsorblock::segments(&http, &id, &categories).await;
            let _ = tx.send(AppEvent::Segments { play, result });
        });
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
                if let Some(segment) = playing.skipper.due(at, playing.duration) {
                    playing.player.seek_to(segment.end);
                    playing.skip = Some(Skip {
                        category: segment.category,
                        seconds: segment.end - at,
                        at: Instant::now(),
                    });
                }
                if playing.saved.elapsed() > SAVE_EVERY {
                    playing.saved = Instant::now();
                    self.save_progress();
                }
            }
            PlayerEventKind::Duration(length) => {
                playing.duration = Some(length);
                // mpv keeps only the chapters before the length it knows.
                if !playing.chapters_sent
                    && playing.chapters.last().is_some_and(|c| c.start < length)
                {
                    playing.chapters_sent = true;
                    playing.player.set_chapters(&playing.chapters);
                }
            }
            PlayerEventKind::Paused(paused) => playing.paused = paused,
            // Changed in mpv's window too: the next video plays as loud.
            PlayerEventKind::Volume(volume) => {
                let volume = volume.round().clamp(0.0, 100.0) as u32;
                if volume != self.settings.volume() {
                    self.settings.volume = volume;
                    self.volume_unsaved = true;
                }
            }
            PlayerEventKind::Muted(muted) => self.muted = muted,
            PlayerEventKind::Ended => playing.ended = true,
            PlayerEventKind::Exited => {
                // Played to its end, not closed or cut off midway.
                let finished = playing.ended
                    && playing
                        .duration
                        .is_none_or(|length| playing.position > length - ENDED_WITHIN);
                let audio_only = playing.audio_only;
                let jukebox_ended = finished && playing.from_jukebox;
                let id = playing.video.id.clone();
                self.stop_playing();
                // Played from the Jukebox to its end: taken off it, if the
                // settings say so. The Jukebox goes on all the same.
                if jukebox_ended && self.settings.jukebox_autoremove {
                    self.take_off_jukebox(&id);
                }
                // A video you chose while this one played is still loading:
                // it plays, not the next. The Jukebox plays on whatever
                // autoplay says; the rest only with it on (`play_next`).
                if finished && self.resolving.is_none() {
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
        // Only a video still in History: one taken off it while it plays
        // (x) stays off. Starting to play is what adds it (on_resolved).
        if self
            .store
            .update_watch(&id, now(), at, length)
            .unwrap_or(false)
            && let Some(length) = length
        {
            self.progress.insert(id, (at, length));
        }
    }

    /// Shows `video` in the player bar, `position` seconds in, with no
    /// mpv behind it: the demo's. From the Jukebox if it's in it.
    pub fn show_playing(&mut self, video: Video, position: f64, duration: f64, audio_only: bool) {
        self.plays += 1;
        let from_jukebox = self.jukebox.iter().any(|v| v.id == video.id);
        if from_jukebox {
            self.jukebox_at = Some(video.id.clone());
        }
        self.playing = Some(Playing {
            player: Player::detached(self.plays),
            video,
            position,
            duration: Some(duration),
            paused: false,
            audio_only,
            saved: Instant::now(),
            ended: false,
            from_jukebox,
            skipper: Skipper::default(),
            skip: None,
            chapters: Vec::new(),
            chapters_sent: false,
            quality: None,
            subtitles: None,
        });
    }

    fn stop_playing(&mut self) {
        self.chapter_menu = None;
        self.save_progress();
        if let Some(mut playing) = self.playing.take() {
            playing.player.stop();
        }
        if self.view == View::History {
            self.reload();
        }
    }

    // Chapters, and going to a time.

    /// `t`: the chapters popup, on the chapter playing.
    fn open_chapters(&mut self) {
        let Some(playing) = &self.playing else {
            return;
        };
        let selected = chapters::current(&playing.chapters, playing.position).unwrap_or(0);
        self.chapter_menu = Some(ChapterMenu {
            selected,
            ..ChapterMenu::default()
        });
    }

    /// `[` and `]`: the start of this chapter (or the one before, if this
    /// one just started), and the next chapter.
    fn step_chapter(&mut self, ahead: bool) {
        let Some(playing) = &self.playing else {
            return;
        };
        let (list, at) = (&playing.chapters, playing.position);
        if list.is_empty() {
            return self.info("This video has no chapters. To go to a time: t");
        }
        let target = if ahead {
            chapters::next(list, at)
        } else {
            chapters::previous(list, at)
        };
        match target {
            Some(start) => self.go_to(start),
            None if ahead => self.info("This is the last chapter"),
            // Before the first chapter: the start.
            None => self.go_to(0.0),
        }
    }

    /// Enter in the chapters popup: the time typed, else the chapter
    /// selected.
    fn go_to_chosen(&mut self) {
        let (Some(menu), Some(playing)) = (&self.chapter_menu, &self.playing) else {
            return;
        };
        let typed = menu.typed.trim().to_string();
        let at = if typed.is_empty() {
            match playing.chapters.get(menu.selected) {
                Some(chapter) => chapter.start,
                None => return self.error(format!("This video has no chapters. {TYPE_A_TIME}")),
            }
        } else {
            let Some(at) = chapters::parse_time(&typed, playing.position, playing.duration) else {
                return self.error(format!("“{typed}” isn't a time. {TYPE_A_TIME}"));
            };
            if let Some(length) = playing.duration.filter(|&length| at >= length) {
                return self.error(format!(
                    "The video is only {} long",
                    video::duration(length as u32)
                ));
            }
            at
        };
        self.chapter_menu = None;
        self.go_to(at);
    }

    /// Goes to `at` seconds into the video playing, and says where that
    /// is.
    fn go_to(&mut self, at: f64) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        if !playing.controllable() {
            return self.error("tuitube can't move mpv on Windows yet");
        }
        playing.player.seek_to(at);
        // Shown at once, before mpv says: a second `]` goes on from here.
        playing.position = at;
        let chapter = chapters::current(&playing.chapters, at)
            .map(|i| playing.chapters[i].title.clone())
            .filter(|title| !title.is_empty());
        let when = video::duration(at as u32);
        self.info(match chapter {
            Some(title) => format!("{when} · {title}"),
            None => format!("Went to {when}"),
        });
    }

    fn on_chapters_key(&mut self, key: KeyEvent, ctrl: bool) {
        let count = self.playing.as_ref().map_or(0, |p| p.chapters.len());
        let Some(menu) = self.chapter_menu.as_mut() else {
            return;
        };
        let last = count.saturating_sub(1);
        let page = HELP_PAGE.unsigned_abs();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 't') if !ctrl => self.chapter_menu = None,
            KeyCode::Enter => self.go_to_chosen(),
            KeyCode::Backspace => {
                menu.typed.pop();
            }
            KeyCode::Char(c)
                if !ctrl
                    && (c.is_ascii_digit() || ":.+-%".contains(c))
                    && menu.typed.len() < MAX_TYPED_TIME =>
            {
                menu.typed.push(c);
            }
            KeyCode::Char('j') | KeyCode::Down => menu.selected = (menu.selected + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Char('d') if ctrl => menu.selected = (menu.selected + page).min(last),
            KeyCode::Char('u') if ctrl => menu.selected = menu.selected.saturating_sub(page),
            KeyCode::PageDown => menu.selected = (menu.selected + 2 * page).min(last),
            KeyCode::PageUp => menu.selected = menu.selected.saturating_sub(2 * page),
            KeyCode::Char('g') | KeyCode::Home => menu.selected = 0,
            KeyCode::Char('G') | KeyCode::End => menu.selected = last,
            _ => {}
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
            View::Jukebox => {
                self.take_off_jukebox(&video.id);
                self.info(format!("Taken off the Jukebox: “{}”", video.title));
            }
            View::History => {
                let _ = self.store.forget_watch(&video.id);
                self.progress.remove(&video.id);
                self.info(format!("Removed from History: “{}”", video.title));
                self.reload();
            }
            View::MyPlaylist(id, ref name) => {
                let name = name.clone();
                if let Err(e) = self.store.remove_from_playlist(id, &video.id) {
                    return self.error(format!("Couldn't save: {e}"));
                }
                self.info(format!("Taken off “{name}”: “{}”", video.title));
                self.reload_subscriptions();
                self.reload();
            }
            _ => {}
        }
    }

    fn ask_to_delete_playlist(&mut self) {
        if let View::MyPlaylist(id, _) = self.view {
            self.confirm = Some(Confirm::DeletePlaylist(id));
        }
    }

    fn delete_playlist(&mut self, id: i64) {
        let name = self
            .my_playlists
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        if let Err(e) = self.store.delete_playlist(id) {
            return self.error(format!("Couldn't save: {e}"));
        }
        self.reload_subscriptions();
        if self.view == View::MyPlaylist(id, name.clone()) {
            self.go_back();
        }
        self.info(format!("Deleted the playlist “{name}”"));
    }

    fn reload_jukebox(&mut self) {
        self.jukebox = self.store.jukebox().unwrap_or_default();
        if self.view == View::Jukebox {
            self.reload();
        }
    }

    /// Takes the video off the Jukebox. If the Jukebox is at it, it goes on
    /// from the video after it all the same.
    fn take_off_jukebox(&mut self, id: &VideoId) {
        if self.jukebox_at.as_ref() == Some(id) {
            self.jukebox_step_back();
        }
        if let Err(e) = self.store.remove_from_jukebox(id) {
            self.error(format!("Couldn't save: {e}"));
        }
        self.reload_jukebox();
    }

    fn order_jukebox(&mut self, order: &[VideoId]) {
        if let Err(e) = self.store.order_jukebox(order) {
            self.error(format!("Couldn't save: {e}"));
        }
        // The selection goes with the video (`reload` keeps it).
        self.reload_jukebox();
    }

    /// `e`: the selected video at the end of the Jukebox. `E`: next in it,
    /// right after the one playing from it (first, before it has played);
    /// moved there if it's in already.
    fn add_to_jukebox(&mut self, next: bool) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        let _ = self.store.save_videos(std::slice::from_ref(&video));
        let added = match self.store.add_to_jukebox(&video.id) {
            Ok(added) => added,
            Err(e) => return self.error(format!("Couldn't save: {e}")),
        };
        self.reload_jukebox();
        if next {
            if self.jukebox_at.as_ref() == Some(&video.id) {
                if self.jukebox_state(&video.id).is_some() {
                    return self.info(format!("Playing now: “{}”", video.title));
                }
                // The Jukebox was at it: it plays again.
                self.jukebox_step_back();
            } else {
                let mut order: Vec<VideoId> = self
                    .jukebox
                    .iter()
                    .map(|v| v.id.clone())
                    .filter(|id| *id != video.id)
                    .collect();
                let after = self
                    .jukebox_at
                    .as_ref()
                    .and_then(|at| order.iter().position(|id| id == at))
                    .map_or(0, |i| i + 1);
                order.insert(after, video.id.clone());
                self.order_jukebox(&order);
            }
            return self.info(format!("Next in the Jukebox: “{}”", video.title));
        }
        let place = self
            .jukebox
            .iter()
            .position(|v| v.id == video.id)
            .map_or(0, |i| i + 1);
        self.info(if added {
            format!("In the Jukebox, #{place}: “{}”", video.title)
        } else {
            format!(
                "In the Jukebox already, #{place}: “{}”. E plays it next",
                video.title
            )
        });
    }

    /// `J` or `K` in the Jukebox: the selected video one later or earlier.
    fn move_in_jukebox(&mut self, later: bool) {
        if self.view != View::Jukebox {
            return;
        }
        let other = if later {
            self.selected + 1
        } else if let Some(before) = self.selected.checked_sub(1) {
            before
        } else {
            return;
        };
        let mut order: Vec<VideoId> = self.jukebox.iter().map(|v| v.id.clone()).collect();
        if self.selected >= order.len() || other >= order.len() {
            return;
        }
        order.swap(self.selected, other);
        self.order_jukebox(&order);
    }

    /// `C` in the Jukebox (or on it in the sidebar): asks first.
    fn ask_to_clear_jukebox(&mut self) {
        if self.jukebox.is_empty() {
            return self.info("The Jukebox is empty");
        }
        self.confirm = Some(Confirm::ClearJukebox);
    }

    fn clear_jukebox(&mut self) {
        match self.store.clear_jukebox() {
            Ok(n) => {
                self.jukebox_at = None;
                self.reload_jukebox();
                let videos = if n == 1 { "video" } else { "videos" };
                self.info(format!("Cleared the Jukebox: {n} {videos} taken off"));
            }
            Err(e) => self.error(format!("Couldn't clear the Jukebox: {e}")),
        }
    }

    /// `v` in the Jukebox, or the settings tab: a list, or cards with
    /// thumbnails.
    fn toggle_jukebox_layout(&mut self) {
        self.settings.jukebox_list = !self.settings.jukebox_list;
        self.save_setting("jukebox_list", self.settings.jukebox_list.into());
    }

    fn toggle_jukebox_autoremove(&mut self) {
        self.settings.jukebox_autoremove = !self.settings.jukebox_autoremove;
        self.save_setting(
            "jukebox_autoremove",
            self.settings.jukebox_autoremove.into(),
        );
        self.info(if self.settings.jukebox_autoremove {
            "Jukebox: each video is taken off once it has played to its end"
        } else {
            "Jukebox: videos stay in it after they play"
        });
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
    /// Imports a Takeout file, or every one in a Takeout folder: read on a
    /// thread of its own (years of history is big), saved when it's read.
    pub fn import(&mut self, path: &str) {
        if self.demo {
            return self.info(DEMO);
        }
        let path = PathBuf::from(unquote(path));
        self.info("Importing…");
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(AppEvent::Imported(takeout::find(&path)));
        });
    }

    fn on_imported(&mut self, result: Result<Vec<Takeout>>) {
        let found = match result {
            Ok(found) => found,
            Err(e) => return self.error(text::clean(&format!("{e:#}"))),
        };
        let channels = found.iter().any(|t| matches!(t, Takeout::Subscriptions(_)));
        let imported = match takeout::save(&mut self.store, found, self.settings.history) {
            Ok(imported) => imported,
            Err(e) => return self.error(format!("Couldn't save: {e:#}")),
        };
        self.reload_subscriptions();
        if channels {
            self.info(format!("{}. Fetching their videos…", imported.summary()));
            self.show(View::Home, false);
            self.sync_sidebar();
            self.refresh_feeds(false);
        } else {
            self.info(imported.summary());
            self.reload();
        }
    }

    fn open_in_browser(&mut self) {
        if self.demo {
            return self.info(DEMO);
        }
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
        self.save_setting("autoplay", self.settings.autoplay.into());
        self.info(if self.settings.autoplay {
            "Autoplay on: when a video ends, the next one plays"
        } else {
            "Autoplay off"
        });
    }

    /// Subtitles on or off, for the video playing (if mpv can be told) and
    /// the ones after it.
    fn toggle_subtitles(&mut self) {
        self.settings.subtitles = !self.settings.subtitles;
        self.save_setting("subtitles", self.settings.subtitles.into());
        let on = self.settings.subtitles;
        let playing = self.playing.as_ref();
        match playing.map(|p| (p.subtitles.clone(), p.controllable())) {
            Some((Some(name), true)) => {
                if let Some(playing) = &self.playing {
                    playing.player.show_subtitles(on);
                }
                self.info(if on {
                    format!("Subtitles on: {name}")
                } else {
                    "Subtitles off".into()
                });
            }
            Some((Some(_), false)) => self.info(if on {
                "Subtitles on, from the next video"
            } else {
                "Subtitles off, from the next video"
            }),
            _ if on => {
                let languages = self.settings.subtitle_languages().join(", ");
                self.info(format!(
                    "Subtitles on, for videos that have them in {languages} (subtitle_languages in settings.toml)"
                ));
            }
            _ => self.info("Subtitles off"),
        }
    }

    fn toggle_sponsorblock(&mut self) {
        self.settings.sponsorblock = !self.settings.sponsorblock;
        self.save_setting("sponsorblock", self.settings.sponsorblock.into());
        if !self.settings.sponsorblock {
            if let Some(playing) = self.playing.as_mut() {
                playing.skipper = Skipper::default();
            }
            return self.info("SponsorBlock off");
        }
        let categories = self.settings.skip_categories();
        if categories.is_empty() {
            return self.error(
                "SponsorBlock on, but sponsorblock_categories in settings.toml names none tuitube knows",
            );
        }
        let names: Vec<&str> = categories.iter().map(|c| sponsorblock::shown(c)).collect();
        self.info(format!(
            "SponsorBlock on: skips {}. sponsor.ajay.app gets a hash of each video's id",
            names.join(", ").to_lowercase()
        ));
        self.look_up_segments();
    }

    fn toggle_dearrow(&mut self) {
        self.settings.dearrow = !self.settings.dearrow;
        self.save_setting("dearrow", self.settings.dearrow.into());
        if self.settings.dearrow {
            self.titles.retry();
            self.info("DeArrow on: better titles, where its users wrote one. sponsor.ajay.app gets a hash of each video's id");
        } else {
            self.info("DeArrow off: YouTube's titles");
        }
    }

    /// Sets the volume, in percent, for the video playing (if mpv can be
    /// told) and the ones after it, and unmutes, as YouTube's player does.
    fn set_volume(&mut self, volume: u32) {
        let volume = volume.min(100);
        self.settings.volume = volume;
        self.volume_unsaved = true;
        let unmute = std::mem::take(&mut self.muted);
        match self.playing.as_ref().map(|p| (p, p.controllable())) {
            Some((playing, true)) => {
                playing.player.set_volume(volume);
                if unmute {
                    playing.player.set_muted(false);
                }
                self.info(format!("Volume {volume}%"));
            }
            Some((_, false)) => self.info(format!("Volume {volume}%, from the next video")),
            None => self.info(format!("Volume {volume}%")),
        }
    }

    /// `M`: sound off or on again, for this session.
    fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        let muted = self.muted;
        let later = match self.playing.as_ref().map(|p| (p, p.controllable())) {
            Some((playing, true)) => {
                playing.player.set_muted(muted);
                ""
            }
            Some((_, false)) => ", from the next video",
            None => "",
        };
        let volume = self.settings.volume();
        self.info(if muted {
            format!("Muted{later}: M turns the sound on again")
        } else {
            format!("Sound on{later}: volume {volume}%")
        });
    }

    /// Writes the volume to settings.toml, if it changed since it was last
    /// written.
    fn save_volume(&mut self) {
        if std::mem::take(&mut self.volume_unsaved) {
            self.save_setting("volume", i64::from(self.settings.volume()).into());
        }
    }

    fn pick_theme(&mut self, id: &str) {
        self.settings.theme = id.to_string();
        self.colors = Colors::named(id);
        self.save_setting("theme", id.into());
        self.info(format!("Theme: {}", self.colors.name));
    }

    fn toggle_shorts(&mut self) {
        // The sidebar's rows move: the one selected stays selected.
        let entry = self.entry(self.sidebar_selected);
        self.settings.shorts = !self.settings.shorts;
        self.save_setting("shorts", self.settings.shorts.into());
        if !self.settings.shorts {
            if self.view == View::Shorts {
                self.show(View::Home, false);
            }
            // Backspace can't go back to them either.
            self.back.retain(|(view, _)| *view != View::Shorts);
        }
        let entry = match entry {
            Entry::Shorts => Entry::Home,
            entry => entry,
        };
        if let Some(i) = (0..self.sidebar_len()).find(|&i| self.entry(i) == entry) {
            self.sidebar_selected = i;
        }
        self.reload();
        self.info(if self.settings.shorts {
            "Shorts shown"
        } else {
            "Shorts hidden, but in Watch later and History"
        });
    }

    fn toggle_live(&mut self) {
        self.settings.live = !self.settings.live;
        self.save_setting("live", self.settings.live.into());
        self.reload();
        self.info(if self.settings.live {
            "Live streams and premieres shown"
        } else {
            "Live streams and premieres hidden until they've ended, but in Watch later and History"
        });
    }

    fn toggle_descriptions(&mut self) {
        self.settings.descriptions = !self.settings.descriptions;
        self.save_setting("descriptions", self.settings.descriptions.into());
    }

    fn toggle_history(&mut self) {
        self.settings.history = !self.settings.history;
        self.save_setting("history", self.settings.history.into());
        self.info(if self.settings.history {
            "History on: what you watch here, and where you stopped, is kept on this computer"
        } else {
            "History off: nothing more is kept. What's in History stays until you remove it (x)"
        });
    }

    /// Picks the best picture to play, and plays the video playing again
    /// at it.
    fn pick_height(&mut self, height: u32) {
        self.settings.max_height = height;
        self.save_setting("max_height", i64::from(height).into());
        self.play_again(format!("Video quality: up to {height}p"), true);
    }

    /// Picks the sound's quality, and plays the video playing again at it.
    fn pick_audio_quality(&mut self, quality: AudioQuality) {
        self.settings.audio_quality = quality;
        self.save_setting("audio_quality", quality.name().into());
        let what = match quality {
            AudioQuality::High => "Sound quality: high, about 130 kbps",
            AudioQuality::Low => "Sound quality: low, about 70 kbps",
        };
        self.play_again(what.into(), false);
    }

    /// After a quality changed (`what`): the video playing plays again at
    /// it, from where it is, as YouTube's quality menu does. Not when the
    /// change can't show (a picture quality while only the sound plays),
    /// while another video is loading, or where tuitube can't tell where it
    /// is (Windows).
    fn play_again(&mut self, what: String, picture: bool) {
        let again = self
            .playing
            .as_ref()
            .filter(|p| p.player.has_mpv() && p.controllable() && !(picture && p.audio_only))
            .filter(|_| self.resolving.is_none() && !self.demo)
            .map(|p| (p.video.clone(), p.audio_only, p.from_jukebox));
        let Some((video, audio_only, from_jukebox)) = again else {
            return self.info(what);
        };
        let id = video.id.clone();
        self.play(video, audio_only, false, from_jukebox);
        if let Some(resolving) = self.resolving.as_mut().filter(|r| r.video.id == id) {
            resolving.carry_on = true;
            self.info(format!("{what}. Loading it again from here…"));
        }
    }

    /// Writes one setting into settings.toml, keeping what the file says
    /// about the others, and applies opt-outs written there since start
    /// (`history = false`, `sponsorblock = false`, `dearrow = false`) to
    /// this session too.
    fn save_setting(&mut self, key: &str, value: toml::Value) {
        let Some(path) = &self.settings_path else {
            return;
        };
        match Settings::save_one(path, key, value) {
            Ok(on_disk) => {
                self.settings.history &= on_disk.history;
                if self.settings.sponsorblock && !on_disk.sponsorblock {
                    self.settings.sponsorblock = false;
                    if let Some(playing) = self.playing.as_mut() {
                        playing.skipper = Skipper::default();
                    }
                }
                self.settings.dearrow &= on_disk.dearrow;
            }
            Err(e) => self.error(format!("{e:#}")),
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
                    Err(e) => {
                        let shown = self.explain(&e);
                        self.error(format!("Search failed: {shown}"));
                    }
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
                        // A channel a link named opens now that it's known.
                        if !matches!(&self.view, View::Channel(id, _) if *id == page.id) {
                            self.show(View::Channel(page.id.clone(), page.title.clone()), false);
                            self.focus = Focus::Grid;
                            self.sync_sidebar();
                        }
                        self.avatars.insert(page.id.clone(), page.avatar);
                        self.reload();
                    }
                    Err(e) => {
                        let shown = self.explain(&e);
                        self.error(format!("The channel didn't load: {shown}"));
                    }
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
            AppEvent::Playlist {
                request,
                id,
                result,
            } => {
                if request != self.request {
                    return;
                }
                self.loading = None;
                match result {
                    Ok((title, videos)) => {
                        let _ = self.store.save_videos(&videos);
                        self.playlists.insert(id.clone(), videos);
                        if matches!(&self.view, View::Playlist(shown, _) if *shown == id) {
                            let title = if title.is_empty() {
                                "Playlist".into()
                            } else {
                                title
                            };
                            self.view = View::Playlist(id, title);
                        }
                        self.reload();
                    }
                    Err(e) => {
                        let shown = self.explain(&e);
                        self.error(format!("The playlist didn't load: {shown}"));
                    }
                }
            }
            AppEvent::ChannelInfo {
                id,
                tab,
                dated,
                result,
            } => {
                self.in_flight.remove(&id);
                // Tried, found or not: a failure (a bot check, no network)
                // isn't retried until there's a newer video to look for.
                self.set_tab_checked(&id, tab);
                // A bot check or a 429 pauses the background looks: they're
                // what YouTube counts, and playing needs the same address.
                if let Err(e) = &result
                    && slowed_down(&format!("{e:#}"))
                    && !self
                        .lookups_paused
                        .is_some_and(|until| Instant::now() < until)
                {
                    self.lookups_paused = Some(Instant::now() + SLOW_DOWN);
                }
                if let Err(e) = &result
                    && channel_gone(&format!("{e:#}"))
                {
                    self.mark_gone(&id);
                }
                if let Ok(page) = result {
                    let _ = if dated {
                        self.store.save_roughly_dated(&page.videos)
                    } else {
                        self.store.save_videos(&page.videos)
                    };
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
                    Ok(streams) => self.on_resolved(resolving, streams),
                    Err(e) => self.on_play_failed(resolving, e),
                }
            }
            AppEvent::Segments { play, result } => {
                // Turned off while SponsorBlock was asked: nothing skipped.
                let Some(playing) = self
                    .playing
                    .as_mut()
                    .filter(|p| p.player.play == play && self.settings.sponsorblock)
                else {
                    return;
                };
                match result {
                    Ok(segments) => playing.skipper = Skipper::new(segments),
                    Err(e) => self.error(format!(
                        "SponsorBlock didn't answer: {}",
                        text::clean(&format!("{e:#}"))
                    )),
                }
            }
            AppEvent::Title { id, result } => {
                if self.titles.answered(id, &result)
                    && self.settings.dearrow
                    && let Err(e) = result
                {
                    self.error(format!(
                        "DeArrow didn't answer: {}. Titles are YouTube's until it's turned on again",
                        text::clean(&format!("{e:#}"))
                    ));
                }
            }
            AppEvent::Player(event) => self.on_player(event),
            AppEvent::Image(event) => self.images.on_built(event),
            AppEvent::YtDlpVersion(Ok(version)) => {
                let notice = self.old_yt_dlp(&version);
                self.yt_dlp_version = Some(version);
                // Said with the failure that asked, while it's shown.
                if let (Some(notice), Some(status)) = (notice, self.status.as_mut())
                    && status.error
                {
                    status.text = format!("{} · {notice}", status.text);
                    status.at = Instant::now();
                }
            }
            AppEvent::YtDlpVersion(Err(_)) => {}
            AppEvent::Imported(result) => self.on_imported(result),
            AppEvent::Comments { id, result } => {
                let comments = match result {
                    Ok(comments) => Comments::Loaded(comments),
                    Err(e) => Comments::Failed(self.explain(&e)),
                };
                self.comments.insert(id, comments);
            }
            AppEvent::Details(result) => {
                self.details_looking = false;
                match result {
                    Ok(videos) => {
                        let _ = self.store.save_videos(&videos);
                        self.needs_reload = true;
                    }
                    Err(e) if slowed_down(&format!("{e:#}")) => {
                        self.lookups_paused = Some(Instant::now() + SLOW_DOWN);
                    }
                    Err(_) => {}
                }
            }
            AppEvent::Quit => self.quit = true,
        }
    }

    /// Terminal events that were waiting together. A paste is meant to come
    /// as one `Event::Paste` (bracketed paste), but on Windows, and from a
    /// terminal that doesn't bracket pastes, it comes as keystrokes, all at
    /// once: a run of different keys read together is pasted text, never
    /// commands. In the search or import box it's added as text (and
    /// submitted only if it's plain text ending in one Enter); with no box
    /// open, a YouTube link is opened (not played) and anything else is
    /// ignored. A key held down (one key, repeated) acts as usual.
    fn on_terminal_events(&mut self, events: Vec<Event>) {
        let presses: Vec<KeyEvent> = events
            .iter()
            .filter_map(|event| match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => Some(*key),
                _ => None,
            })
            .collect();
        let repeated = presses.windows(2).all(|w| w[0].code == w[1].code);
        if presses.len() < 2 || repeated {
            return events.into_iter().for_each(|event| self.on_terminal(event));
        }
        for event in events.into_iter().filter(|e| !matches!(e, Event::Key(_))) {
            self.on_terminal(event);
        }
        let typed = |key: &KeyEvent| {
            matches!(key.code, KeyCode::Char(_))
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        };
        if self.prompt.is_none() && self.command.is_none() {
            if presses.len() >= PASTED_KEYS {
                let text: String = presses
                    .iter()
                    .filter_map(|key| match key.code {
                        KeyCode::Char(c) if typed(key) => Some(c),
                        _ => None,
                    })
                    .collect();
                return match links::parse(&text) {
                    Some(link) if !self.popup_open() => self.open_link(link),
                    _ => self
                        .info("Pasted text goes in the search or import box: press / or I first"),
                };
            }
            return presses.into_iter().for_each(|key| self.on_key(key));
        }
        let (last, before) = presses.split_last().expect("two or more");
        let submit = last.code == KeyCode::Enter && before.iter().all(typed);
        let text: String = presses
            .iter()
            .filter_map(|key| match key.code {
                KeyCode::Char(c) if typed(key) => Some(c),
                KeyCode::Enter | KeyCode::Tab => Some(' '),
                _ => None,
            })
            .collect();
        self.on_terminal(Event::Paste(text));
        // A command pasted isn't run: Enter is yours to press.
        if submit && self.command.is_none() {
            self.on_key(*last);
        }
    }

    fn on_terminal(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Paste(text) => {
                // Cut before cleaning, so a huge paste costs nothing, and
                // the box never holds more than `MAX_PROMPT`.
                let paste = |into: &mut String| {
                    let room = MAX_PROMPT.saturating_sub(into.chars().count());
                    let line = video::one_line(text::first_chars(&text, 2 * MAX_PROMPT), room);
                    into.push_str(&line);
                };
                if let Some(prompt) = self.prompt.as_mut() {
                    paste(&mut prompt.text);
                } else if let Some(command) = self.command.as_mut() {
                    paste(&mut command.text);
                    command.edited();
                    self.complete_command();
                } else if !self.popup_open()
                    && let Some(link) = links::parse(text::first_chars(&text, 2 * MAX_PROMPT))
                {
                    self.open_link(link);
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
        if self.command.is_some() {
            return self.on_command_key(key);
        }
        // `y` answers yes; any other key, no.
        if let Some(confirm) = self.confirm.take() {
            if key.code == KeyCode::Char('y') && !ctrl {
                match confirm {
                    Confirm::ClearJukebox => self.clear_jukebox(),
                    Confirm::DeletePlaylist(id) => self.delete_playlist(id),
                }
            }
            return;
        }
        if self.help.is_some() {
            return self.on_help_key(key, ctrl);
        }
        if self.info_popup.is_some() {
            return self.on_info_key(key, ctrl);
        }
        if self.chapter_menu.is_some() {
            return self.on_chapters_key(key, ctrl);
        }
        if std::mem::take(&mut self.pending_g) && key.code == KeyCode::Char('g') {
            return self.jump_to(0);
        }
        // Letters act here only without Ctrl: Ctrl-u and Ctrl-d move half a
        // page in the grid and the sidebar.
        match key.code {
            KeyCode::Char('q') if !ctrl => self.quit = true,
            KeyCode::Char('?') if !ctrl => self.help = Some(HelpMenu::new(HelpTab::Keys)),
            KeyCode::Char('O') if !ctrl => self.help = Some(HelpMenu::new(HelpTab::Settings)),
            KeyCode::Char(':') if !ctrl => {
                self.command = Some(CommandLine::default());
                self.complete_command();
            }
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
            KeyCode::Char('A') if !ctrl => self.toggle_autoplay(),
            KeyCode::Char('T') if !ctrl => self.toggle_subtitles(),
            KeyCode::Char('i') if !ctrl => self.open_info(),
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
            KeyCode::Char('-') if !ctrl => {
                self.set_volume(self.settings.volume().saturating_sub(VOLUME_STEP));
            }
            KeyCode::Char('+' | '=') if !ctrl => {
                self.set_volume(self.settings.volume() + VOLUME_STEP);
            }
            KeyCode::Char('M') if !ctrl => self.toggle_mute(),
            KeyCode::Char('t') if !ctrl && self.playing.is_some() => self.open_chapters(),
            KeyCode::Char(c @ ('[' | ']')) if !ctrl && self.playing.is_some() => {
                self.step_chapter(c == ']');
            }
            KeyCode::Char('X') if !ctrl && (self.playing.is_some() || self.resolving.is_some()) => {
                self.stop();
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

    /// X: stops playing, and forgets what was up next.
    fn stop(&mut self) {
        self.stop_playing();
        self.resolving = None;
        self.up_next.clear();
        self.info("Stopped");
    }

    /// Opens a sidebar row: its list, or the search box.
    fn open_entry(&mut self, entry: Entry) {
        if entry == Entry::Search {
            self.prompt = Some(Prompt {
                kind: PromptKind::Search,
                text: self.last_query(),
            });
            return;
        }
        if let Some(view) = self.entry_view(entry) {
            self.show(view, matches!(entry, Entry::Channel(_)));
            self.sync_sidebar();
        }
        self.focus = Focus::Grid;
    }

    /// c: the selected video's channel.
    fn open_selected_channel(&mut self) {
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

    // The command line.

    /// Works out the completions of the command line as typed.
    fn complete_command(&mut self) {
        if let Some(mut line) = self.command.take() {
            line.complete(self);
            self.command = Some(line);
        }
    }

    fn on_command_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(line) = self.command.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.command = None,
            KeyCode::Backspace if line.text.is_empty() => self.command = None,
            KeyCode::Enter => self.submit_command(),
            KeyCode::Tab | KeyCode::Down => line.step(false),
            KeyCode::BackTab | KeyCode::Up => line.step(true),
            KeyCode::Char('n') if ctrl => line.step(false),
            KeyCode::Char('p') if ctrl => line.step(true),
            KeyCode::Backspace => {
                line.text.pop();
                line.edited();
                self.complete_command();
            }
            KeyCode::Char('u') if ctrl => {
                line.text.clear();
                line.edited();
                self.complete_command();
            }
            KeyCode::Char('w') if ctrl => {
                let trimmed = line.text.trim_end().len();
                line.text.truncate(trimmed);
                let cut = line.text.rfind(' ').map_or(0, |i| i + 1);
                line.text.truncate(cut);
                line.edited();
                self.complete_command();
            }
            KeyCode::Char(c)
                if !ctrl && !text::is_hidden(c) && line.text.chars().count() < MAX_PROMPT =>
            {
                line.text.push(c);
                line.edited();
                self.complete_command();
            }
            _ => {}
        }
    }

    /// Enter on the command line: runs it, if it's a whole command. Only
    /// Tab completes: a line typed in part says so and waits, so nothing
    /// runs that isn't on the line (`:sp` doesn't turn SponsorBlock on).
    /// A completion Tab put there is taken, and a command waiting for its
    /// argument then lists what it takes.
    fn submit_command(&mut self) {
        let Some(line) = self.command.as_ref() else {
            return;
        };
        let text = line.text.clone();
        if text.trim().is_empty() {
            self.command = None;
            return;
        }
        let tabbed = line.source() != text;
        let error = match commands::parse(&text, self) {
            Ok(command) => {
                self.command = None;
                return self.run_command(command);
            }
            Err(error) => error,
        };
        if tabbed {
            if let Some(line) = self.command.as_mut() {
                line.edited();
            }
            self.complete_command();
        }
        let Some(line) = self.command.as_mut() else {
            return;
        };
        if tabbed && !line.completion.candidates.is_empty() {
            return;
        }
        // Tab would change the line: it's typed in part.
        let partial = line.with(0).is_some_and(|completed| completed != text);
        line.error = Some(if partial {
            format!("{error}. Tab completes it")
        } else {
            error
        });
    }

    fn run_command(&mut self, command: Command) {
        match command {
            Command::Quality(height) => self.pick_height(height),
            Command::Sound(quality) => self.pick_audio_quality(quality),
            Command::Theme(id) => self.pick_theme(id),
            Command::Set(toggle, on) => self.set_toggle(toggle, on),
            Command::Volume(volume) => self.set_volume(volume),
            Command::Mute => self.toggle_mute(),
            Command::Play => self.play_selected(false),
            Command::Listen => self.play_selected(true),
            Command::Pause => {
                if let Some(playing) = &self.playing {
                    playing.player.toggle_pause();
                }
            }
            Command::Stop => self.stop(),
            Command::Next => self.skip_to_next(),
            Command::GoTo(at) => self.go_to(at),
            Command::Chapters => self.open_chapters(),
            Command::Chapter(i) => {
                let start = self.playing.as_ref().and_then(|p| p.chapters.get(i));
                if let Some(start) = start.map(|c| c.start) {
                    self.go_to(start);
                }
            }
            Command::PreviousChapter => self.step_chapter(false),
            Command::NextChapter => self.step_chapter(true),
            Command::Mix => self.open_mix(),
            Command::Jukebox(JukeboxAction::Add) => self.add_to_jukebox(false),
            Command::Jukebox(JukeboxAction::Next) => self.add_to_jukebox(true),
            Command::Jukebox(JukeboxAction::Clear) => self.ask_to_clear_jukebox(),
            Command::WatchLater => self.toggle_watch_later(),
            Command::Remove => self.remove_selected(),
            Command::Channel(None) => self.open_selected_channel(),
            Command::Channel(Some(i)) => self.open_entry(Entry::Channel(i)),
            Command::Subscribe => self.toggle_subscription(),
            Command::Undo => self.undo_unsubscribe(),
            Command::Browser => self.open_in_browser(),
            Command::Copy => self.copy_link(),
            Command::Go(entry) => self.open_entry(entry),
            Command::Search(query) if query.trim().is_empty() => self.open_entry(Entry::Search),
            Command::Search(query) => self.search(&query, SEARCH_PAGE),
            Command::Import(path) if path.trim().is_empty() => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Import,
                    text: String::new(),
                });
            }
            Command::Import(path) => self.import(&path),
            Command::Back => self.go_back(),
            Command::Refresh => self.refresh(),
            Command::Keys => self.help = Some(HelpMenu::new(HelpTab::Keys)),
            Command::Info => self.open_info(),
            Command::Settings => self.help = Some(HelpMenu::new(HelpTab::Settings)),
            Command::Quit => self.quit = true,
        }
    }

    /// Turns a setting on, off, or (`None`) over.
    fn set_toggle(&mut self, toggle: Toggle, on: Option<bool>) {
        if let Some(on) = on.filter(|&on| on == toggle.is_on(&self.settings)) {
            let state = if on { "on" } else { "off" };
            return self.info(format!("That's {state} already"));
        }
        match toggle {
            Toggle::Autoplay => self.toggle_autoplay(),
            Toggle::Subtitles => self.toggle_subtitles(),
            Toggle::SponsorBlock => self.toggle_sponsorblock(),
            Toggle::DeArrow => self.toggle_dearrow(),
            Toggle::Shorts => self.toggle_shorts(),
            Toggle::Live => self.toggle_live(),
            Toggle::History => self.toggle_history(),
            Toggle::JukeboxAutoremove => self.toggle_jukebox_autoremove(),
            Toggle::Descriptions => {
                self.toggle_descriptions();
                self.info(if self.settings.descriptions {
                    "Descriptions shown"
                } else {
                    "Descriptions hidden"
                });
            }
            Toggle::JukeboxList => {
                self.toggle_jukebox_layout();
                self.info(if self.settings.jukebox_list {
                    "Jukebox: a list"
                } else {
                    "Jukebox: cards"
                });
            }
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
            View::Playlist(id, _) => self.fetch_playlist(id),
            View::Link(_) => self.reload(),
            _ => {
                self.refresh_feeds(true);
                if self.refreshing.is_some() {
                    self.info(if self.feeds_down.is_some() {
                        "Seeing whether YouTube's feeds are back…"
                    } else {
                        "Updating your subscriptions…"
                    });
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

    /// Opens the `i` popup on the selected video (or the one playing, with
    /// none selected): its description, as stored.
    fn open_info(&mut self) {
        let video = self
            .selected_video()
            .or(self.playing.as_ref().map(|p| &p.video))
            .cloned();
        let Some(video) = video else {
            return;
        };
        let video = self.store.video(&video.id).ok().flatten().unwrap_or(video);
        self.info_popup = Some(InfoPopup {
            video,
            tab: InfoTab::Description,
            line: 0,
            scroll: 0,
            description_scroll: Cell::new(0),
        });
    }

    /// The comments of the video in the `i` popup, asked for the first time
    /// its Comments tab opens: yt-dlp reads its page, as for playing.
    fn load_comments(&mut self) {
        let Some(id) = self.info_popup.as_ref().map(|p| p.video.id.clone()) else {
            return;
        };
        let retry = matches!(self.comments.get(&id), Some(Comments::Failed(_)));
        if self.comments.contains_key(&id) && !retry {
            return;
        }
        if self.demo {
            self.comments.insert(
                id,
                Comments::Failed("This is the demo: no comments to load".into()),
            );
            return;
        }
        let Some(yt) = self.yt.clone() else {
            let missing = self.tools.missing().unwrap_or_default();
            self.comments.insert(id, Comments::Failed(missing));
            return;
        };
        self.comments.insert(id.clone(), Comments::Loading);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = yt.comments(&id).await;
            let _ = tx.send(AppEvent::Comments { id, result });
        });
    }

    fn on_info_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(popup) = self.info_popup.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'i') => {
                self.info_popup = None;
                return;
            }
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Char('h' | 'l')
            | KeyCode::Left
            | KeyCode::Right => {
                popup.tab = match popup.tab {
                    InfoTab::Description => InfoTab::Comments,
                    InfoTab::Comments => InfoTab::Description,
                };
                if popup.tab == InfoTab::Comments {
                    self.load_comments();
                }
                return;
            }
            KeyCode::Enter if popup.tab == InfoTab::Description => {
                let line = popup.video.description.lines().nth(popup.line);
                let Some(at) = line.and_then(time_in) else {
                    return;
                };
                let video = popup.video.clone();
                self.info_popup = None;
                return self.go_to_in(video, at);
            }
            _ => {}
        }
        let delta: isize = match key.code {
            KeyCode::Char('j') | KeyCode::Down => 1,
            KeyCode::Char('k') | KeyCode::Up => -1,
            KeyCode::Char('d') if ctrl => HELP_PAGE,
            KeyCode::Char('u') if ctrl => -HELP_PAGE,
            KeyCode::PageDown => 2 * HELP_PAGE,
            KeyCode::PageUp => -2 * HELP_PAGE,
            KeyCode::Char('g') | KeyCode::Home => isize::MIN,
            KeyCode::Char('G') | KeyCode::End => isize::MAX,
            _ => return,
        };
        match popup.tab {
            InfoTab::Description => {
                let last = popup.video.description.lines().count().saturating_sub(1);
                popup.line = popup.line.saturating_add_signed(delta).min(last);
            }
            // Drawing stops it at the end.
            InfoTab::Comments => popup.scroll = popup.scroll.saturating_add_signed(delta),
        }
    }

    /// Goes to `at` in `video`: there at once if it's playing, else it
    /// plays from there.
    fn go_to_in(&mut self, video: Video, at: f64) {
        if self
            .playing
            .as_ref()
            .is_some_and(|p| p.video.id == video.id)
        {
            return self.go_to(at);
        }
        self.start_once = Some((video.id.clone(), at));
        self.play(video, false, false, false);
    }

    fn on_help_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(menu) = self.help.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Char('h' | 'l')
            | KeyCode::Left
            | KeyCode::Right => {
                menu.tab = match menu.tab {
                    HelpTab::Keys => HelpTab::Settings,
                    HelpTab::Settings => HelpTab::Keys,
                };
                return;
            }
            KeyCode::Enter | KeyCode::Char(' ') if menu.tab == HelpTab::Settings => {
                let row = menu.selected;
                return self.change_setting(row);
            }
            KeyCode::Esc | KeyCode::Char('q' | '?' | 'O') => {
                self.help = None;
                return;
            }
            _ => {}
        }
        let delta: isize = match key.code {
            KeyCode::Char('j') | KeyCode::Down => 1,
            KeyCode::Char('k') | KeyCode::Up => -1,
            KeyCode::Char('d') if ctrl => HELP_PAGE,
            KeyCode::Char('u') if ctrl => -HELP_PAGE,
            KeyCode::PageDown => 2 * HELP_PAGE,
            KeyCode::PageUp => -2 * HELP_PAGE,
            KeyCode::Char('g') | KeyCode::Home => isize::MIN,
            KeyCode::Char('G') | KeyCode::End => isize::MAX,
            _ => return,
        };
        match menu.tab {
            // Drawing stops it at the end of the list.
            HelpTab::Keys => menu.scroll = menu.scroll.saturating_add_signed(delta),
            HelpTab::Settings => {
                menu.selected = menu
                    .selected
                    .saturating_add_signed(delta)
                    .min(HelpMenu::LAST);
            }
        }
    }

    /// Enter or Space on the settings tab: turns the setting on `row` on or
    /// off, or picks the quality or theme on it. Saved at once.
    fn change_setting(&mut self, row: usize) {
        match row {
            HelpMenu::SHORTS => self.toggle_shorts(),
            HelpMenu::LIVE => self.toggle_live(),
            HelpMenu::DESCRIPTIONS => self.toggle_descriptions(),
            HelpMenu::JUKEBOX_LIST => self.toggle_jukebox_layout(),
            HelpMenu::JUKEBOX_AUTOREMOVE => self.toggle_jukebox_autoremove(),
            HelpMenu::AUTOPLAY => self.toggle_autoplay(),
            HelpMenu::SUBTITLES => self.toggle_subtitles(),
            HelpMenu::SPONSORBLOCK => self.toggle_sponsorblock(),
            HelpMenu::DEARROW => self.toggle_dearrow(),
            HelpMenu::HISTORY => self.toggle_history(),
            row if (HelpMenu::VIDEO_QUALITY..HelpMenu::AUDIO_QUALITY).contains(&row) => {
                self.pick_height(settings::HEIGHTS[row - HelpMenu::VIDEO_QUALITY]);
            }
            row if (HelpMenu::AUDIO_QUALITY..HelpMenu::JUKEBOX_LIST).contains(&row) => {
                self.pick_audio_quality(AudioQuality::ALL[row - HelpMenu::AUDIO_QUALITY]);
            }
            row => {
                if let Some((id, _)) = theme::names().get(row - HelpMenu::THEMES) {
                    self.pick_theme(id);
                }
            }
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
                self.open_entry(self.entry(self.sidebar_selected));
                None
            }
            KeyCode::Char('C') if self.entry(self.sidebar_selected) == Entry::Jukebox => {
                self.ask_to_clear_jukebox();
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
            KeyCode::Char('e') => self.add_to_jukebox(false),
            KeyCode::Char('E') => self.add_to_jukebox(true),
            KeyCode::Char('J') => self.move_in_jukebox(true),
            KeyCode::Char('K') => self.move_in_jukebox(false),
            KeyCode::Char('v') if self.view == View::Jukebox => self.toggle_jukebox_layout(),
            KeyCode::Char('C') if self.view == View::Jukebox => self.ask_to_clear_jukebox(),
            KeyCode::Char('C') if matches!(self.view, View::MyPlaylist(..)) => {
                self.ask_to_delete_playlist();
            }
            KeyCode::Char('w') => self.toggle_watch_later(),
            KeyCode::Char('x') | KeyCode::Delete => self.remove_selected(),
            KeyCode::Char('c') => self.open_selected_channel(),
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

/// The time a line of a description names: `1:23` or `1:02:03`, the first
/// one in it (`0:00 Intro`, `Intro - 0:00`, `(12:34)`).
pub fn time_in(line: &str) -> Option<f64> {
    line.split(|c: char| !c.is_ascii_digit() && c != ':')
        .find_map(|word| {
            let parts: Vec<&str> = word.split(':').collect();
            if !(2..=3).contains(&parts.len())
                || parts.iter().any(|p| p.is_empty() || p.len() > 2)
                || parts[1..].iter().any(|p| p.len() != 2)
            {
                return None;
            }
            let n: Vec<u32> = parts
                .iter()
                .map(|p| p.parse().ok())
                .collect::<Option<_>>()?;
            if n[1..].iter().any(|&v| v >= 60) {
                return None;
            }
            Some(n.iter().fold(0, |total, v| total * 60 + v))
        })
        .map(f64::from)
}

/// Whether yt-dlp's error says YouTube wants tuitube to slow down.
fn slowed_down(error: &str) -> bool {
    // Whole phrases: a bare "429" also turns up in channel ids, and "Sign in
    // to confirm" also starts YouTube's age check.
    [
        "HTTP Error 429",
        "Too Many Requests",
        "not a bot",
        // An hour's limit on this address.
        "content isn't available, try again later",
        "captcha",
    ]
    .iter()
    .any(|sign| error.contains(sign))
}

/// What YouTube refusing means, in tuitube's words, when it's on purpose: a
/// bot check, or a sign-in (which tuitube never does; your browser can).
fn refusal(error: &str) -> Option<&'static str> {
    if slowed_down(error) {
        return Some(
            "YouTube is limiting this network (a bot check): it usually clears within an hour",
        );
    }
    let error = error.to_lowercase();
    let says = |signs: &[&str]| signs.iter().any(|sign| error.contains(sign));
    if says(&[
        "confirm your age",
        "age-restricted",
        "inappropriate for some users",
    ]) {
        Some("YouTube shows it only to signed-in adults: o on it opens it in your browser")
    } else if says(&["members-only", "join this channel", "channel's members"]) {
        Some("it's for the channel's members: o on it opens it in your browser")
    } else if says(&["youtube premium", "music premium", "premium members"]) {
        Some("it needs YouTube Premium: o on it opens it in your browser")
    } else if says(&["private video", "video is private"]) {
        Some("it's private")
    } else if says(&["sign in"]) {
        Some("YouTube wants a sign-in for it: o on it opens it in your browser")
    } else {
        None
    }
}

/// Whether yt-dlp's error only passes on what YouTube said (the video is
/// gone, private, not out yet, not here), which an update wouldn't change.
fn youtube_said(error: &str) -> bool {
    refusal(error).is_some()
        || channel_gone(error)
        || [
            "unavailable",
            "has been removed",
            "not available",
            "Premieres in",
            "will begin in",
            "only for music videos",
            "nothing to search for",
            "took too long",
            "cannot start yt-dlp",
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
    // xdg-open runs more programs by name (xprop, gio, x-www-browser…). They
    // come from PATH's absolute folders only (main keeps no other), and it
    // runs in the home folder, not the one tuitube was started from. The
    // rest of the environment stays: it decides which browser opens.
    let mut command = std::process::Command::new(opener);
    command
        .arg(url)
        .current_dir(dirs::home_dir().unwrap_or_else(|| "/".into()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group, so the terminal's signals don't reach the
    // browser it starts.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Turns SIGHUP (the terminal window closed) and SIGTERM into a normal quit,
/// which closes the database (writing it out in full) and ends yt-dlp runs
/// with everything they started.
#[cfg(unix)]
fn quit_on_hangup(tx: UnboundedSender<AppEvent>) -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::spawn(async move {
        tokio::select! {
            _ = hangup.recv() => {}
            _ = terminate.recv() => {}
        }
        let _ = tx.send(AppEvent::Quit);
    });
    Ok(())
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

    /// Opens the settings (`O`), goes down to `row` and presses Enter.
    fn change_setting(app: &mut App, row: usize) {
        app.on_key(key(KeyCode::Char('O')));
        assert_eq!(app.help.as_ref().unwrap().tab, HelpTab::Settings);
        for _ in 0..row {
            app.on_key(key(KeyCode::Char('j')));
        }
        app.on_key(key(KeyCode::Enter));
        assert!(app.help.is_some(), "the popup stays open");
        app.on_key(key(KeyCode::Esc));
        assert!(app.help.is_none());
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
    async fn the_settings_tab_turns_things_on_and_off_and_saves_them() {
        let dir = std::env::temp_dir().join(format!("tuitube-app-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = crate::settings::path(&dir);
        let _ = std::fs::remove_file(&file);
        let mut app = app();
        app.settings_path = Some(file.clone());
        with_feed(&mut app, 3);

        // ? opens on the keys, Tab goes to the settings and back.
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.help.as_ref().unwrap().tab, HelpTab::Keys);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.help.as_ref().unwrap().tab, HelpTab::Settings);
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.help.as_ref().unwrap().selected, HelpMenu::LAST);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.help.as_ref().unwrap().selected,
            HelpMenu::LAST,
            "no row below"
        );
        app.on_key(key(KeyCode::Enter));
        let last = theme::BUILT_IN[theme::BUILT_IN.len() - 1].0;
        assert_eq!(app.settings.theme, last, "Enter on a theme uses it");
        app.on_key(key(KeyCode::Char('?')));
        assert!(app.help.is_none());
        // T and B no longer do anything.
        app.on_key(key(KeyCode::Char('B')));
        assert!(!app.settings.sponsorblock);

        // Shorts off, from Shorts: back Home, and no Shorts in the sidebar.
        app.focus = Focus::Sidebar;
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.view, View::Shorts);
        change_setting(&mut app, HelpMenu::SHORTS);
        assert!(!app.settings.shorts);
        assert_eq!(app.view, View::Home);
        assert_eq!(app.entry(app.sidebar_selected), Entry::Home);
        assert_eq!(app.sidebar_len(), MENU_WITHOUT_SHORTS.len() + 1);
        assert!((0..app.sidebar_len()).all(|i| app.entry(i) != Entry::Shorts));
        app.on_key(key(KeyCode::Backspace));
        assert_ne!(app.view, View::Shorts, "not back to Shorts");
        // A channel stays selected as the rows above it move.
        app.on_key(key(KeyCode::Char('G')));
        change_setting(&mut app, HelpMenu::SHORTS);
        assert!(app.settings.shorts);
        assert_eq!(app.entry(app.sidebar_selected), Entry::Channel(0));

        change_setting(&mut app, HelpMenu::HISTORY);
        change_setting(&mut app, HelpMenu::LIVE);
        assert!(!app.settings.history && !app.settings.live);
        let saved = Settings::load(&file).unwrap();
        assert!(!saved.history && !saved.live && saved.shorts);
        assert_eq!(saved.theme, last);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn shorts_and_live_streams_turned_off_are_hidden_but_in_your_lists() {
        let mut app = app();
        with_feed(&mut app, 2);
        let mut short = video("vidshort001", CH, Some(2000));
        short.short = true;
        let mut live = video("vidlive0001", CH, Some(2001));
        live.live = true;
        let mut premiere = video("vidprem0001", CH, Some(2002));
        premiere.upcoming = true;
        app.store
            .save_videos(&[short.clone(), live.clone(), premiere.clone()])
            .unwrap();
        for v in [&short, &live] {
            app.store.toggle_watch_later(&v.id, 1).unwrap();
        }
        let ch = ChannelId::parse(CH).unwrap();
        app.show(View::Channel(ch, "BekBrace".into()), false);
        assert_eq!(app.videos.len(), 5);

        app.settings.shorts = false;
        app.reload();
        assert_eq!(app.videos.len(), 4);
        assert!(app.videos.iter().all(|v| !v.short));
        app.settings.live = false;
        app.reload();
        assert_eq!(app.videos.len(), 2, "neither live now nor to come");
        app.show(View::WatchLater, false);
        assert_eq!(app.videos.len(), 2, "what you put there stays");
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
        assert!(!slowed_down(
            "UCabcdefgh429ijklmnopqrs: This channel does not have a streams tab"
        ));
        assert!(!slowed_down("Sign in to confirm your age"));
    }

    /// `text` as the key events a terminal sends for it without bracketed
    /// paste: a carriage return is Enter, a tab is Tab.
    fn as_keys(text: &str) -> Vec<Event> {
        text.chars()
            .map(|c| {
                let (code, modifiers) = match c {
                    '\r' => (KeyCode::Enter, KeyModifiers::NONE),
                    '\t' => (KeyCode::Tab, KeyModifiers::NONE),
                    c if c.is_uppercase() => (KeyCode::Char(c), KeyModifiers::SHIFT),
                    c => (KeyCode::Char(c), KeyModifiers::NONE),
                };
                Event::Key(KeyEvent::new(code, modifiers))
            })
            .collect()
    }

    #[tokio::test]
    async fn a_paste_that_comes_as_keystrokes_never_runs_commands() {
        let mut app = app();
        with_feed(&mut app, 3);
        let subscribed = app.subscriptions.len();
        // Into the import box: all text, the box stays open.
        app.on_key(key(KeyCode::Char('I')));
        app.on_terminal_events(as_keys("/home/me/x.csv\r\tGxxxB"));
        assert_eq!(
            app.prompt.as_ref().map(|p| p.text.as_str()),
            Some("/home/me/x.csv GxxxB")
        );
        assert_eq!(app.subscriptions.len(), subscribed);
        assert!(!app.settings.sponsorblock);
        // With no box open: ignored, not run.
        app.prompt = None;
        let (focus, selected) = (app.focus, app.selected);
        app.on_terminal_events(as_keys("\tGxxxB"));
        assert_eq!((app.focus, app.selected), (focus, selected));
        assert_eq!(app.subscriptions.len(), subscribed);
        assert!(!app.settings.sponsorblock);
        // A key held down still moves; plain text ending in Enter submits.
        app.grid.set(GridShape { cols: 1, rows: 3 });
        app.on_terminal_events(as_keys("jj"));
        assert_eq!(app.selected, 2);
        app.on_key(key(KeyCode::Char('/')));
        app.prompt.as_mut().unwrap().text.clear();
        app.on_terminal_events(as_keys("lofi\r"));
        assert!(app.prompt.is_none(), "submitted");
    }

    #[tokio::test]
    async fn every_slow_down_pauses_background_looks_not_just_the_first() {
        let mut app = app();
        let ch = ChannelId::parse(CH).unwrap();
        let bot_check = || AppEvent::ChannelInfo {
            id: ChannelId::parse(CH).unwrap(),
            tab: Tab::Videos,
            dated: false,
            result: Err(anyhow::anyhow!("Sign in to confirm you're not a bot")),
        };
        app.on_event(bot_check());
        assert!(app.lookups_paused.is_some_and(|t| t > Instant::now()));
        // The hour has run out; looks may start again.
        app.lookups_paused = Some(Instant::now() - Duration::from_secs(1));
        let dir = std::env::temp_dir().join(format!("tuitube-pause-{}", std::process::id()));
        let tools = Tools {
            yt_dlp: Some("/dev/null/no-yt-dlp".into()),
            ..Tools::default()
        };
        app.yt = YtDlp::new(&tools, &dir).unwrap();
        assert!(
            app.look_up_channel(&ch, Tab::Videos, false),
            "the pause ran out"
        );
        app.in_flight.clear();
        // A second bot check pauses them again.
        app.on_event(bot_check());
        assert!(
            app.lookups_paused.is_some_and(|t| t > Instant::now()),
            "paused again"
        );
        assert!(!app.look_up_channel(&ch, Tab::Streams, false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn taking_the_playing_video_off_history_isnt_undone_by_saving_progress() {
        let mut app = app();
        with_feed(&mut app, 1);
        let video = app.videos[0].clone();
        app.store
            .record_watch(&video.id, 1000, None, Some(600.0))
            .unwrap();
        app.show_playing(video.clone(), 120.0, 600.0, false);
        mpv_control(&mut app, true);
        app.show(View::History, false);
        assert_eq!(app.videos.len(), 1);
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.store.history(10).unwrap().is_empty());
        // The next save while it plays, and the one when it stops.
        app.playing.as_mut().unwrap().saved = Instant::now() - SAVE_EVERY * 2;
        mpv_says(&mut app, [PlayerEventKind::Position(131.0)]);
        app.on_key(key(KeyCode::Char('X')));
        assert!(app.store.history(10).unwrap().is_empty(), "still off");
        assert_eq!(app.store.progress(&video.id).unwrap(), None);
        assert!(!app.progress.contains_key(&video.id));
        // A video still in History gets its place saved as before, where
        // tuitube can follow mpv; Windows can't yet, and saves none.
        let other = crate::store::tests::video("vidwindows1", CH, Some(2));
        app.store.save_videos(std::slice::from_ref(&other)).unwrap();
        for (video, controllable, saved) in
            [(&video, true, Some((200.0, 600.0))), (&other, false, None)]
        {
            app.store
                .record_watch(&video.id, 2000, None, Some(600.0))
                .unwrap();
            app.show_playing(video.clone(), 200.0, 600.0, false);
            mpv_control(&mut app, controllable);
            app.on_key(key(KeyCode::Char('X')));
            assert_eq!(app.store.progress(&video.id).unwrap(), saved);
        }
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
            dated: false,
            result: Err(anyhow::anyhow!("Sign in to confirm you're not a bot")),
        });
        assert!(!app.is_gone(&ch));
        app.on_event(AppEvent::ChannelInfo {
            id: ch.clone(),
            tab: Tab::Videos,
            dated: false,
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

    /// `n` subscriptions, the last one the most active, and a yt-dlp that
    /// fails at once, offline. Not in the temporary folder, which other
    /// users can write to on Linux.
    fn subscribed(n: usize) -> (App, Vec<ChannelId>, PathBuf) {
        let mut app = app();
        let dir = std::env::temp_dir().join(format!(
            "tuitube-feeds-{}-{n}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let tools = Tools {
            yt_dlp: Some("/dev/null/no-yt-dlp".into()),
            ..Tools::default()
        };
        app.yt = YtDlp::new(&tools, &dir).unwrap();
        let channels: Vec<ChannelId> = (0..n)
            .map(|i| ChannelId::parse(&format!("UC{i:0>22}")).unwrap())
            .collect();
        for (i, channel) in channels.iter().enumerate() {
            app.store.subscribe(channel, "A channel").unwrap();
            let id = format!("vid{i:08}");
            let v = video(&id, channel.as_str(), Some(1000 + i as i64));
            app.store.save_videos(&[v]).unwrap();
        }
        app.reload_subscriptions();
        (app, channels, dir)
    }

    fn feed_says(app: &mut App, channel: &ChannelId, result: Result<Feed>) {
        app.on_event(AppEvent::Feed {
            channel: channel.clone(),
            result,
        });
    }

    #[tokio::test]
    async fn channels_missing_from_a_round_that_loads_are_asked_about_a_few_at_a_time() {
        let (mut app, channels, dir) = subscribed(20);
        for _ in 0..2 {
            app.start_round(channels.len());
            let loaded = Feed {
                channel: "A channel".into(),
                videos: Vec::new(),
            };
            feed_says(&mut app, &channels[0], Ok(loaded));
            for channel in &channels[1..] {
                feed_says(&mut app, channel, Err(feed::NotFound.into()));
            }
            assert_eq!(app.feeds_down, None, "one loaded: not an outage");
        }
        // Each round asks about the next few, never the same twice.
        assert_eq!(app.channel_lookups, 2 * GONE_CHECKS_PER_ROUND);
        assert_eq!(app.gone_checks.len(), 2 * GONE_CHECKS_PER_ROUND);
        assert!(app.status.as_ref().unwrap().text.contains("19 of 20"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn feeds_down_call_the_round_off_and_channels_fill_in() {
        let (mut app, channels, dir) = subscribed(20);
        let due = app.round_of(channels.clone());
        assert_eq!(due.len(), 20, "a whole round while they're up");
        let limit = app.start_round(due.len());
        for (i, channel) in channels.iter().enumerate().take(OUTAGE_AFTER) {
            assert_eq!(app.feeds_down, None, "not yet after {i}");
            feed_says(&mut app, channel, Err(feed::NotFound.into()));
        }
        assert_eq!(app.feeds_down, Some(FeedsDown::Broken));
        assert!(limit.is_closed(), "the rest is called off");
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .text
                .contains("YouTube's feeds are down")
        );
        for channel in &channels[OUTAGE_AFTER..] {
            feed_says(&mut app, channel, Err(feed::CalledOff.into()));
        }
        assert_eq!(app.refreshing, None, "the round is over");
        assert_eq!(app.feed_failures, OUTAGE_AFTER, "the rest wasn't tried");
        // No channel is asked whether it's gone; the most active fill in.
        assert!(app.gone_checks.is_empty());
        assert_eq!(app.channel_lookups, FILL_IN_PER_ROUND);
        let mut filled: Vec<&ChannelId> = app.filled_in.keys().collect();
        filled.sort();
        let newest: Vec<&ChannelId> = channels[20 - FILL_IN_PER_ROUND..].iter().collect();
        assert_eq!(filled, newest);

        // While they're down, a round tries two, different ones each time.
        let first = app.round_of(channels.clone());
        let second = app.round_of(channels.clone());
        assert_eq!((first.len(), second.len()), (PROBE, PROBE));
        assert!(first.iter().all(|c| !second.contains(c)));
        // The next fills in the next most active.
        app.start_round(PROBE);
        for channel in &second {
            feed_says(&mut app, channel, Err(feed::NotFound.into()));
        }
        assert_eq!(app.feeds_down, Some(FeedsDown::Broken));
        assert_eq!(app.channel_lookups, 2 * FILL_IN_PER_ROUND);

        // One loads: they're back, and the rest are fetched at once.
        app.round_of(channels.clone());
        app.start_round(PROBE);
        let loaded = Feed {
            channel: "A channel".into(),
            videos: Vec::new(),
        };
        feed_says(&mut app, &channels[0], Ok(loaded));
        assert_eq!(app.feeds_down, None);
        assert_eq!(
            app.status.as_ref().unwrap().text,
            "YouTube's feeds are back"
        );
        feed_says(&mut app, &channels[1], Err(feed::NotFound.into()));
        assert_eq!(app.refreshed, None, "the next round starts now");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn feeds_that_cant_be_reached_fill_nothing_in() {
        let (mut app, channels, dir) = subscribed(10);
        app.start_round(channels.len());
        for channel in &channels {
            // As no network fails: a reqwest error.
            let no_answer = feed::client().get("not a url").build().unwrap_err();
            feed_says(&mut app, channel, Err(no_answer.into()));
        }
        assert_eq!(app.feeds_down, Some(FeedsDown::Unreachable));
        assert!(app.status.as_ref().unwrap().text.contains("network"));
        assert_eq!(app.channel_lookups, 0, "yt-dlp couldn't reach it either");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn youtube_refusing_on_purpose_is_said_in_tuitubes_words() {
        let age = "Sign in to confirm your age. This video may be inappropriate for some users";
        assert!(refusal(age).unwrap().contains("signed-in adults"));
        let members = "Join this channel to get access to members-only content like this video, and other exclusive perks.";
        assert!(refusal(members).unwrap().contains("members"));
        assert_eq!(
            refusal("Private video. Sign in if you've been granted access to this video"),
            Some("it's private")
        );
        let bot = "Sign in to confirm you're not a bot";
        assert!(refusal(bot).unwrap().contains("bot check"));
        let limited = "This content isn't available, try again later";
        assert!(slowed_down(limited));
        assert_eq!(refusal("Unable to extract nsig"), None);
        assert!(youtube_said(
            "Video unavailable. This video has been removed by the uploader"
        ));
        assert!(!youtube_said("Unable to extract nsig"));
    }

    #[tokio::test]
    async fn a_failure_nothing_explains_says_to_update_an_old_yt_dlp() {
        let mut app = app();
        app.tools.yt_dlp = Some("/opt/homebrew/bin/yt-dlp".into());
        app.yt_dlp_version = Some("2026.01.01".into());
        let broken = anyhow::anyhow!("Unable to extract nsig");
        let shown = app.explain(&broken);
        assert!(
            shown.starts_with("Unable to extract nsig · yt-dlp came out"),
            "{shown}"
        );
        assert!(shown.ends_with(": brew upgrade yt-dlp"), "{shown}");
        let gone = anyhow::anyhow!("Video unavailable");
        assert_eq!(
            app.explain(&gone),
            "Video unavailable",
            "an update wouldn't help"
        );
        let today = chrono::Local::now().format("%Y.%m.%d").to_string();
        app.yt_dlp_version = Some(today);
        assert_eq!(app.explain(&broken), "Unable to extract nsig", "it's new");

        // Asked for after a failure: added to the message still shown.
        app.yt_dlp_version = None;
        app.error("Can't play “A video”: Unable to extract nsig");
        app.on_event(AppEvent::YtDlpVersion(Ok("2026.01.01".into())));
        let status = &app.status.as_ref().unwrap().text;
        assert!(
            status.contains("Unable to extract nsig · yt-dlp came out"),
            "{status}"
        );
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

    /// tuitube can control the mpv playing (`true`, as on Unix) or not
    /// (as on Windows), whichever system runs the test. A test of anything
    /// that differs (the player bar's keys, seeking, saving where you are,
    /// SponsorBlock) sets it and checks both: Windows CI runs the tests too.
    pub fn mpv_control(app: &mut App, controllable: bool) {
        let playing = app.playing.as_mut().expect("a video playing");
        playing.player.set_controllable(controllable);
    }

    /// SponsorBlock marked `start..end` of the video playing as a sponsor,
    /// and mpv is `at` seconds in.
    pub fn sponsor_at(app: &mut App, start: f64, end: f64, at: f64) {
        let play = app.plays;
        app.on_event(AppEvent::Segments {
            play,
            result: Ok(vec![Segment::new(start, end, "sponsor")]),
        });
        mpv_says(app, [PlayerEventKind::Position(at)]);
    }

    /// DeArrow answered `title` for `id`.
    pub fn dearrow_says(app: &mut App, id: &VideoId, title: &str) {
        app.on_event(AppEvent::Title {
            id: id.clone(),
            result: Ok(Some(title.into())),
        });
    }

    fn shown_skip(app: &App) -> Option<(&'static str, f64)> {
        let skip = app.playing.as_ref()?.shown_skip()?;
        Some((skip.category, skip.seconds))
    }

    #[tokio::test]
    async fn minus_and_plus_set_the_volume_and_m_mutes() {
        let mut app = app();
        with_feed(&mut app, 2);
        assert_eq!(app.settings.volume(), 100);
        // Nothing playing: for the next video.
        app.on_key(key(KeyCode::Char('-')));
        assert_eq!(app.settings.volume(), 95);
        assert!(app.volume_unsaved, "saved on the next tick");
        app.show_playing(app.videos[0].clone(), 0.0, 600.0, false);
        for controllable in [true, false] {
            mpv_control(&mut app, controllable);
            app.settings.volume = 50;
            app.on_key(key(KeyCode::Char('+')));
            app.on_key(key(KeyCode::Char('=')));
            assert_eq!(app.settings.volume(), 60);
            let said = &app.status.as_ref().unwrap().text;
            assert_eq!(
                said.contains("from the next video"),
                !controllable,
                "{said}"
            );
        }
        mpv_control(&mut app, true);
        app.settings.volume = 3;
        app.on_key(key(KeyCode::Char('-')));
        assert_eq!(app.settings.volume(), 0, "not below 0");
        app.settings.volume = 98;
        app.on_key(key(KeyCode::Char('+')));
        assert_eq!(app.settings.volume(), 100, "not above 100");

        app.on_key(key(KeyCode::Char('M')));
        assert!(app.muted);
        assert!(app.status.as_ref().unwrap().text.starts_with("Muted"));
        // Changing the volume turns the sound on again, as on YouTube.
        app.on_key(key(KeyCode::Char('-')));
        assert!(!app.muted);
        app.on_key(key(KeyCode::Char('M')));
        app.on_key(key(KeyCode::Char('M')));
        assert!(!app.muted);

        // Changed in mpv's window: the next video plays as loud.
        app.volume_unsaved = false;
        mpv_says(&mut app, [PlayerEventKind::Volume(95.0)]);
        assert!(
            !app.volume_unsaved,
            "mpv saying tuitube's volume changes nothing"
        );
        mpv_says(
            &mut app,
            [PlayerEventKind::Volume(42.4), PlayerEventKind::Muted(true)],
        );
        assert_eq!(app.settings.volume(), 42);
        assert!(app.muted && app.volume_unsaved);
        app.save_volume();
        assert!(!app.volume_unsaved);

        // The command line does the same.
        app.on_key(key(KeyCode::Char(':')));
        for c in "volume 30".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.settings.volume(), 30);
        assert!(!app.muted);
    }

    #[tokio::test]
    async fn dearrow_is_opt_in_and_only_changes_the_title_shown() {
        let mut app = app();
        with_feed(&mut app, 2);
        let video = app.videos[0].clone();
        assert!(!app.settings.dearrow);
        app.want_title(&video);
        assert!(app.titles.take_wanted().is_empty(), "off: nothing is asked");

        change_setting(&mut app, HelpMenu::DEARROW);
        assert!(app.settings.dearrow);
        assert!(app.status.as_ref().unwrap().text.contains("hash"));
        app.want_title(&video);
        assert_eq!(app.titles.take_wanted(), std::slice::from_ref(&video.id));
        app.on_event(AppEvent::Title {
            id: video.id.clone(),
            result: Ok(Some("What it's really about".into())),
        });
        assert_eq!(app.title(&video), "What it's really about");
        assert_eq!(app.title(&app.videos[1]), app.videos[1].title, "none known");
        let stored = app.store.video(&video.id).unwrap().unwrap();
        assert_eq!(stored.title, video.title, "YouTube's is what's kept");

        change_setting(&mut app, HelpMenu::DEARROW);
        assert_eq!(app.title(&video), video.title, "off: YouTube's again");

        // The demo asks nothing.
        app.settings.dearrow = true;
        app.demo = true;
        app.want_title(&app.videos[1].clone());
        assert!(app.titles.take_wanted().is_empty());
    }

    #[tokio::test]
    async fn opt_outs_written_meanwhile_apply_when_any_setting_is_saved() {
        let dir = std::env::temp_dir().join(format!("tuitube-opt-outs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = settings::path(&dir);
        let mut app = app();
        app.settings_path = Some(path.clone());
        app.settings.history = true;
        app.settings.sponsorblock = true;
        app.settings.dearrow = true;
        // Written by hand, or by another tuitube, after this one started.
        std::fs::write(
            &path,
            "history = false\nsponsorblock = false\ndearrow = false\n",
        )
        .unwrap();
        app.on_key(key(KeyCode::Char('-')));
        app.save_volume();
        assert!(!app.settings.history && !app.settings.sponsorblock && !app.settings.dearrow);
        let on_disk = Settings::load(&path).unwrap();
        assert_eq!(on_disk.volume(), 95);
        assert!(!on_disk.dearrow, "never turned back on");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn dearrow_failing_is_said_once() {
        let mut app = app();
        with_feed(&mut app, 4);
        app.settings.dearrow = true;
        for video in app.videos.clone() {
            app.want_title(&video);
        }
        app.status = None;
        for id in app.titles.take_wanted() {
            app.on_event(AppEvent::Title {
                id,
                result: Err(anyhow::anyhow!("HTTP 503")),
            });
        }
        let said = app.status.take().unwrap();
        assert!(
            said.error && said.text.contains("HTTP 503"),
            "{}",
            said.text
        );
        app.on_event(AppEvent::Title {
            id: app.videos[0].id.clone(),
            result: Err(anyhow::anyhow!("HTTP 503")),
        });
        assert!(app.status.is_none(), "once");
    }

    #[tokio::test]
    async fn blocked_videos_hide_everywhere_but_your_own_lists() {
        let mut app = app();
        with_feed(&mut app, 3);
        let blocked = app
            .videos
            .iter()
            .find(|v| v.id.as_str() == "vid00000001")
            .unwrap()
            .clone();
        app.store.toggle_watch_later(&blocked.id, 1).unwrap();
        app.settings.blocked_words = vec!["VID00000001".into()];
        app.reload();
        assert_eq!(app.videos.len(), 2);
        assert!(app.videos.iter().all(|v| v.id != blocked.id));
        app.show(View::WatchLater, false);
        assert_eq!(app.videos.len(), 1, "Watch later keeps it");
        app.show(View::Home, false);
        app.settings.blocked_words.clear();
        app.settings.blocked_channels = vec![CH.into()];
        app.reload();
        assert!(app.videos.is_empty(), "by the channel's id");
    }

    #[tokio::test]
    async fn sponsorblock_is_opt_in_and_skips_each_segment_once() {
        let mut app = app();
        with_feed(&mut app, 2);
        assert!(!app.settings.sponsorblock);
        app.show_playing(app.videos[0].clone(), 0.0, 600.0, false);
        // Off: an answer still on its way is dropped.
        sponsor_at(&mut app, 10.0, 40.0, 12.0);
        assert_eq!(shown_skip(&app), None);

        // Settings turn it on. The demo's player has no mpv: nothing is
        // asked.
        change_setting(&mut app, HelpMenu::SPONSORBLOCK);
        assert!(app.settings.sponsorblock);
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .text
                .contains("sponsor, intro, outro")
        );
        // An answer for an earlier playback is dropped.
        let play = app.plays;
        app.on_event(AppEvent::Segments {
            play: play - 1,
            result: Ok(vec![Segment::new(10.0, 40.0, "sponsor")]),
        });
        mpv_says(&mut app, [PlayerEventKind::Position(12.0)]);
        assert_eq!(shown_skip(&app), None);

        sponsor_at(&mut app, 10.0, 40.0, 12.0);
        assert_eq!(shown_skip(&app), Some(("sponsor", 28.0)));
        // Seeking back into it plays it: each segment is skipped once.
        app.playing.as_mut().unwrap().skip = None;
        mpv_says(
            &mut app,
            [
                PlayerEventKind::Position(5.0),
                PlayerEventKind::Position(15.0),
            ],
        );
        assert_eq!(shown_skip(&app), None);

        // Off again, mid-video: the rest isn't skipped.
        sponsor_at(&mut app, 100.0, 140.0, 50.0);
        change_setting(&mut app, HelpMenu::SPONSORBLOCK);
        assert!(!app.settings.sponsorblock);
        mpv_says(&mut app, [PlayerEventKind::Position(101.0)]);
        assert_eq!(shown_skip(&app), None);
    }

    #[tokio::test]
    async fn chapters_and_typed_times_move_the_video_playing() {
        let mut app = app();
        with_feed(&mut app, 2);
        app.show_playing(app.videos[0].clone(), 75.0, 600.0, false);
        let chapter = |start: f64, title: &str| Chapter {
            start,
            title: title.into(),
        };
        app.playing.as_mut().unwrap().chapters = vec![
            chapter(0.0, "Intro"),
            chapter(60.0, "Middle"),
            chapter(180.0, "Outro"),
        ];
        let at = |app: &App| app.playing.as_ref().unwrap().position;
        let said = |app: &App| {
            let status = app.status.as_ref().unwrap();
            (status.text.clone(), status.error)
        };
        let typed = |app: &mut App, text: &str| {
            for c in text.chars() {
                app.on_key(key(KeyCode::Char(c)));
            }
        };

        // mpv gets the chapters once it knows a length that holds them.
        mpv_says(&mut app, [PlayerEventKind::Duration(1.0)]);
        assert!(!app.playing.as_ref().unwrap().chapters_sent);
        mpv_says(&mut app, [PlayerEventKind::Duration(600.0)]);
        assert!(app.playing.as_ref().unwrap().chapters_sent);

        // Where tuitube can't move mpv (Windows, for now), it says so.
        mpv_control(&mut app, false);
        app.on_key(key(KeyCode::Char(']')));
        assert!(said(&app).1, "Windows can't move mpv yet");
        assert_eq!(at(&app), 75.0);
        mpv_control(&mut app, true);
        // ] the next; [ this one's start, or the one before if it just
        // started.
        app.on_key(key(KeyCode::Char(']')));
        assert_eq!(at(&app), 180.0);
        assert_eq!(said(&app), ("3:00 · Outro".into(), false));
        app.on_key(key(KeyCode::Char(']')));
        assert_eq!(at(&app), 180.0, "the last one");
        app.on_key(key(KeyCode::Char('[')));
        assert_eq!(at(&app), 60.0, "just started: the one before");
        mpv_says(&mut app, [PlayerEventKind::Position(100.0)]);
        app.on_key(key(KeyCode::Char('[')));
        assert_eq!(at(&app), 60.0, "well into it: its start");

        // t: the chapters, on the one playing; Enter goes to the one chosen.
        app.on_key(key(KeyCode::Char('t')));
        assert_eq!(app.chapter_menu.as_ref().unwrap().selected, 1);
        app.on_key(key(KeyCode::Char('k')));
        app.on_key(key(KeyCode::Enter));
        assert!(app.chapter_menu.is_none());
        assert_eq!(at(&app), 0.0);

        // A time typed: Enter goes there. Other keys aren't typed.
        app.on_key(key(KeyCode::Char('t')));
        typed(&mut app, "1:3x0");
        assert_eq!(app.chapter_menu.as_ref().unwrap().typed, "1:30");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(at(&app), 90.0);
        assert_eq!(said(&app), ("1:30 · Middle".into(), false));

        // Past the end, or not a time: said, and the popup stays to fix it.
        app.on_key(key(KeyCode::Char('t')));
        typed(&mut app, "20:00");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(said(&app), ("The video is only 10:00 long".into(), true));
        assert!(app.chapter_menu.is_some());
        for _ in 0..5 {
            app.on_key(key(KeyCode::Backspace));
        }
        typed(&mut app, "::");
        app.on_key(key(KeyCode::Enter));
        assert!(said(&app).0.starts_with("“::” isn't a time"));
        app.on_key(key(KeyCode::Esc));
        assert!(app.chapter_menu.is_none());
        assert_eq!(at(&app), 90.0, "nowhere");
        app.on_key(key(KeyCode::Char('t')));
        typed(&mut app, "+30");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(at(&app), 120.0);

        // No chapters: [ and ] say so; t still goes to a time.
        app.playing.as_mut().unwrap().chapters.clear();
        app.on_key(key(KeyCode::Char(']')));
        assert!(said(&app).0.contains("no chapters"));
        app.on_key(key(KeyCode::Char('t')));
        app.on_key(key(KeyCode::Enter));
        assert!(said(&app).1, "nothing chosen");
        typed(&mut app, "50%");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(at(&app), 300.0);

        // The popup goes with the video.
        app.on_key(key(KeyCode::Char('t')));
        app.stop_playing();
        assert!(app.chapter_menu.is_none());
        app.on_key(key(KeyCode::Char('t')));
        assert!(app.chapter_menu.is_none(), "nothing playing");
    }

    #[tokio::test]
    async fn the_quality_picked_is_saved_and_plays_from_the_next_video() {
        let dir = std::env::temp_dir().join(format!("tuitube-app-quality-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = crate::settings::path(&dir);
        let _ = std::fs::remove_file(&file);
        let mut app = app();
        app.settings_path = Some(file.clone());
        with_feed(&mut app, 2);

        change_setting(&mut app, HelpMenu::VIDEO_QUALITY + 3);
        assert_eq!(app.settings.max_height, 720);
        change_setting(&mut app, HelpMenu::AUDIO_QUALITY + 1);
        assert_eq!(app.settings.audio_quality, AudioQuality::Low);
        let saved = Settings::load(&file).unwrap();
        assert_eq!(
            (saved.max_height, saved.audio_quality),
            (720, AudioQuality::Low)
        );

        // With no mpv behind the player (the demo's), nothing plays again.
        app.show_playing(app.videos[0].clone(), 30.0, 600.0, false);
        change_setting(&mut app, HelpMenu::VIDEO_QUALITY);
        assert!(app.resolving.is_none());
        assert_eq!(
            app.status.as_ref().unwrap().text,
            "Video quality: up to 2160p"
        );
        assert_eq!(Settings::load(&file).unwrap().max_height, 2160);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[tokio::test]
    async fn the_command_line_runs_settings_and_actions_by_name() {
        let dir = std::env::temp_dir().join(format!("tuitube-app-command-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = crate::settings::path(&dir);
        let _ = std::fs::remove_file(&file);
        let mut app = app();
        app.settings_path = Some(file.clone());
        with_feed(&mut app, 3);
        let error = |app: &App| app.command.as_ref().and_then(|l| l.error.clone());

        // Typed whole.
        typed(&mut app, ":quality 720");
        app.on_key(key(KeyCode::Enter));
        assert!(app.command.is_none(), "closed once run");
        assert_eq!(app.settings.max_height, 720);
        assert_eq!(Settings::load(&file).unwrap().max_height, 720, "saved");

        // Half typed: Enter runs nothing, Tab completes. Enter on a command
        // Tab put there that needs more lists what it takes.
        typed(&mut app, ":qual");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            error(&app),
            Some("No command “qual”. Tab completes it".into())
        );
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Enter));
        let line = app.command.as_ref().unwrap();
        assert_eq!(line.text, "quality ");
        assert_eq!(line.completion.candidates.len(), 6, "the heights");
        assert_eq!(error(&app), None);
        typed(&mut app, "4");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.settings.max_height, 720, "not run");
        assert!(error(&app).unwrap().ends_with("Tab completes it"));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.command.as_ref().unwrap().text, "quality 480");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.settings.max_height, 480);
        // vim's :sp isn't SponsorBlock.
        typed(&mut app, ":sp");
        app.on_key(key(KeyCode::Enter));
        assert!(!app.settings.sponsorblock);
        assert!(error(&app).unwrap().ends_with("Tab completes it"));
        app.on_key(key(KeyCode::Esc));

        // Tab goes through the values, on the line.
        typed(&mut app, ":sound ");
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.command.as_ref().unwrap().text, "sound high");
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.command.as_ref().unwrap().text, "sound low");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.settings.audio_quality, AudioQuality::Low);

        // Tab onto a command, then Enter: it runs, if it can without more
        // (autoplay turns over); else it lists what it takes.
        typed(&mut app, ":aut");
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.command.as_ref().unwrap().text, "autoplay ");
        app.on_key(key(KeyCode::Enter));
        assert!(app.command.is_none());
        assert!(!app.settings.autoplay);
        typed(&mut app, ":the");
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Enter));
        let line = app.command.as_ref().unwrap();
        assert_eq!(line.text, "theme ");
        assert_eq!(line.completion.candidates.len(), theme::BUILT_IN.len());
        assert_eq!(error(&app), None, "the list says what it takes");
        app.on_key(key(KeyCode::Esc));
        typed(&mut app, ":set autoplay off");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.status.as_ref().unwrap().text, "That's off already");

        // What can't run says why, and stays to be fixed.
        typed(&mut app, ":pause");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            error(&app),
            Some("Nothing is playing".into()),
            "whole, so Tab wouldn't help"
        );
        app.on_key(key(KeyCode::Char('d')));
        assert_eq!(error(&app), None, "typing clears it");
        app.on_key(key(KeyCode::Esc));
        assert!(app.command.is_none());
        typed(&mut app, ":x");
        app.on_key(key(KeyCode::Backspace));
        app.on_key(key(KeyCode::Backspace));
        assert!(
            app.command.is_none(),
            "Backspace on an empty line closes it"
        );

        // Actions, and the lists in the sidebar.
        typed(&mut app, ":go later");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.view, View::WatchLater);
        assert_eq!(app.entry(app.sidebar_selected), Entry::WatchLater);
        typed(&mut app, ":channel BekB");
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.view, View::Channel(..)));
        typed(&mut app, ":go home");
        app.on_key(key(KeyCode::Enter));
        typed(&mut app, ":jukebox add");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.jukebox.len(), 1);
        typed(&mut app, ":theme nord");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.settings.theme, "nord");
        typed(&mut app, ":keys");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.help.as_ref().unwrap().tab, HelpTab::Keys);
        app.on_key(key(KeyCode::Esc));

        // A time alone, while a video plays.
        app.show_playing(app.videos[0].clone(), 10.0, 600.0, false);
        mpv_control(&mut app, true);
        typed(&mut app, ":1:23");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.playing.as_ref().unwrap().position, 83.0);
        typed(&mut app, ":q");
        app.on_key(key(KeyCode::Enter));
        assert!(app.quit, "vim's :q");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_command_pasted_goes_on_the_line_but_isnt_run() {
        let mut app = app();
        with_feed(&mut app, 2);
        app.on_key(key(KeyCode::Char(':')));
        // Keystrokes from a terminal that doesn't bracket pastes.
        let mut events = as_keys("autoplay off");
        events.push(Event::Key(key(KeyCode::Enter)));
        app.on_terminal_events(events);
        assert!(app.settings.autoplay, "not run");
        let line = app.command.as_ref().unwrap();
        assert_eq!(line.text, "autoplay off");
        assert_eq!(line.completion.candidates[0].text, "off", "completed");
        // A bracketed paste: one line, cleaned.
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char(':')));
        app.on_terminal(Event::Paste("theme\n\u{1b}[2Jdracula".into()));
        assert_eq!(app.command.as_ref().unwrap().text, "theme [2Jdracula");
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
            from_jukebox: false,
            carry_on: false,
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

    /// An app that gets as far as asking yt-dlp to play, with programs that
    /// can't be there: each ask fails at once, offline. Not in the temporary
    /// folder, which other users can write to on Linux.
    fn app_that_plays(name: &str) -> (App, PathBuf) {
        let mut app = app();
        let dir = std::env::temp_dir().join(format!("tuitube-{name}-{}", std::process::id()));
        app.tools = Tools {
            yt_dlp: Some("/dev/null/no-yt-dlp".into()),
            mpv: Some("/dev/null/no-mpv".into()),
            ..Tools::default()
        };
        app.yt = YtDlp::new(&app.tools, &dir).unwrap();
        (app, dir)
    }

    fn jukebox(app: &App) -> Vec<&str> {
        app.jukebox.iter().map(|v| v.id.as_str()).collect()
    }

    fn loading(app: &App) -> Option<&str> {
        app.resolving.as_ref().map(|r| r.video.id.as_str())
    }

    /// yt-dlp's answer to the video loading.
    fn resolved(app: &mut App, result: Result<Streams>) {
        let request = app.resolves;
        app.on_event(AppEvent::Resolved { request, result });
    }

    const LINKED: &str = "dQw4w9WgXcQ";

    fn takeout_found() -> Vec<Takeout> {
        let id = |s: &str| VideoId::parse(s).unwrap();
        let mut watched = video("vid00000009", CH, None);
        watched.title = "Rust in 100 Seconds".into();
        vec![
            Takeout::Playlist {
                name: "Watch later".into(),
                watch_later: true,
                videos: vec![(id("vid00000001"), Some(100)), (id("vid00000002"), None)],
            },
            Takeout::Playlist {
                name: "Road trip".into(),
                watch_later: false,
                videos: vec![(id("vid00000003"), None), (id("vid00000004"), None)],
            },
            Takeout::History(vec![(watched, 5000)]),
        ]
    }

    #[test]
    fn times_in_a_descriptions_lines_are_found() {
        assert_eq!(time_in("0:00 Intro"), Some(0.0));
        assert_eq!(time_in("Setup - 12:34"), Some(754.0));
        assert_eq!(time_in("(1:02:03) The end"), Some(3723.0));
        assert_eq!(time_in("Ratio 16:9 and 4:3"), None, "9 isn't seconds");
        assert_eq!(time_in("at 1:75"), None);
        assert_eq!(time_in("Released 2024"), None);
        assert_eq!(time_in("http://x.example:8080/"), None);
    }

    #[tokio::test]
    async fn i_shows_the_description_and_enter_goes_to_its_times() {
        let (mut app, dir) = app_that_plays("info");
        with_feed(&mut app, 2);
        let mut v = app.videos[0].clone();
        v.description = "My video\n\n0:00 Intro\n1:23 The good part".into();
        app.store.save_videos(std::slice::from_ref(&v)).unwrap();
        app.on_key(key(KeyCode::Char('i')));
        let popup = app.info_popup.as_ref().unwrap();
        assert_eq!(popup.video.description, v.description, "the stored one");
        assert!(app.popup_open());
        for _ in 0..3 {
            app.on_key(key(KeyCode::Char('j')));
        }
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.info_popup.as_ref().unwrap().line,
            3,
            "stops at the last"
        );
        // Not playing: it plays from there.
        app.on_key(key(KeyCode::Enter));
        assert!(app.info_popup.is_none());
        assert_eq!(loading(&app), Some(v.id.as_str()));
        assert_eq!(app.start_once, Some((v.id.clone(), 83.0)));
        // Playing: it goes there at once.
        app.resolving = None;
        app.show_playing(v.clone(), 5.0, 600.0, false);
        app.on_key(key(KeyCode::Char('i')));
        app.on_key(key(KeyCode::Char('G')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.playing.as_ref().unwrap().position, 83.0);

        // Comments load when their tab opens, once.
        app.on_key(key(KeyCode::Char('i')));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.comments.get(&v.id), Some(&Comments::Loading));
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Tab));
        let comment = crate::ytdlp::Comment {
            author: "@someone".into(),
            text: "Great video".into(),
            likes: Some(1200),
            at: Some(100),
            pinned: false,
            by_uploader: false,
        };
        app.on_event(AppEvent::Comments {
            id: v.id.clone(),
            result: Ok(vec![comment.clone()]),
        });
        assert_eq!(
            app.comments.get(&v.id),
            Some(&Comments::Loaded(vec![comment]))
        );
        app.on_key(key(KeyCode::Esc));
        assert!(app.info_popup.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn t_turns_subtitles_on_and_off_for_the_video_playing_and_the_next() {
        let mut app = app();
        with_feed(&mut app, 2);
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, false);
        app.on_key(key(KeyCode::Char('T')));
        assert!(app.settings.subtitles);
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .text
                .starts_with("Subtitles on, for videos")
        );
        app.playing.as_mut().unwrap().subtitles = Some("English".into());
        app.on_key(key(KeyCode::Char('T')));
        assert!(!app.settings.subtitles);
        assert_eq!(app.status.as_ref().unwrap().text, "Subtitles off");
        app.on_key(key(KeyCode::Char('T')));
        assert_eq!(app.status.as_ref().unwrap().text, "Subtitles on: English");
        // Windows' mpv can't be told while it plays.
        mpv_control(&mut app, false);
        app.on_key(key(KeyCode::Char('T')));
        assert_eq!(
            app.status.as_ref().unwrap().text,
            "Subtitles off, from the next video"
        );
    }

    #[tokio::test]
    async fn takeouts_lists_and_history_are_imported_and_playlists_kept_in_the_sidebar() {
        let (mut app, dir) = app_that_plays("import");
        app.on_imported(Ok(takeout_found()));
        assert_eq!(
            app.status.as_ref().unwrap().text,
            "Imported 2 videos to Watch later, the playlist “Road trip” (2 videos) and 1 video of history"
        );
        assert_eq!(app.watch_later.len(), 2);
        assert_eq!(app.my_playlists.len(), 1);
        let row = app.menu().len();
        assert_eq!(app.entry(row), Entry::MyPlaylist(0));
        app.focus = Focus::Sidebar;
        app.jump_to(row);
        app.on_key(key(KeyCode::Enter));
        let road_trip = View::MyPlaylist(app.my_playlists[0].id, "Road trip".into());
        assert_eq!(app.view, road_trip);
        assert_eq!(app.videos.len(), 2);
        assert!(app.videos[0].needs_details(), "only their ids are known");

        // On screen, they're asked about in one look, once.
        app.want_details(&app.videos[0].clone());
        app.want_details(&app.videos[1].clone());
        app.fetch_details();
        assert!(app.details_looking);
        app.want_details(&app.videos[0].clone());
        assert!(app.details_wanted.is_empty(), "asked already");
        let mut found = video("vid00000003", CH, None);
        found.title = "The coast road".into();
        app.on_event(AppEvent::Details(Ok(vec![found])));
        app.reload();
        assert_eq!(app.videos[0].title, "The coast road");
        assert!(app.videos[1].needs_details(), "private or gone: as it was");

        // x takes one off; C, then y, deletes the playlist.
        app.focus = Focus::Grid;
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.videos.len(), 1);
        assert_eq!(app.my_playlists[0].count, 1);
        app.on_key(key(KeyCode::Char('C')));
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.my_playlists.is_empty());
        assert_ne!(app.view, road_trip);

        app.show(View::History, false);
        assert_eq!(app.videos[0].title, "Rust in 100 Seconds");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn history_kept_off_isnt_imported() {
        let mut app = app();
        app.settings.history = false;
        app.on_imported(Ok(takeout_found()));
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .text
                .ends_with("History is off, so it wasn't imported")
        );
        assert!(app.store.history(10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_link_pasted_in_shows_its_video_to_play_from_its_time() {
        let (mut app, dir) = app_that_plays("link");
        app.on_terminal(Event::Paste("https://youtu.be/dQw4w9WgXcQ?t=42".into()));
        assert_eq!(app.view, View::Link(VideoId::parse(LINKED).unwrap()));
        assert_eq!(ids(&app.videos.iter().cloned().collect()), [LINKED]);
        assert_eq!(app.videos[0].title, "youtu.be/dQw4w9WgXcQ");
        assert!(app.resolving.is_none(), "a paste plays nothing");
        app.want_details(&app.videos[0].clone());
        assert!(app.details_wanted.is_empty(), "nor asks about it");
        let status = &app.status.as_ref().unwrap().text;
        assert_eq!(status, "Enter plays it from 0:42, a plays its sound");

        app.on_key(key(KeyCode::Enter));
        assert_eq!(loading(&app), Some(LINKED));
        let mut info = video(LINKED, CH, Some(500));
        info.title = "Never Gonna Give You Up".into();
        let streams = Streams {
            video: "https://rr1---sn-abc.googlevideo.com/videoplayback".into(),
            audio: None,
            user_agent: None,
            duration: Some(212.0),
            chapters: Vec::new(),
            quality: None,
            info: Some(Box::new(info)),
            captions: Vec::new(),
        };
        resolved(&mut app, Ok(streams));
        assert_eq!(app.videos[0].title, "Never Gonna Give You Up", "filled in");
        assert_eq!(
            app.videos[0].channel_id.as_ref().map(ChannelId::as_str),
            Some(CH)
        );
        assert_eq!(app.start_once, None, "its time is used once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn links_open_from_the_search_box_or_as_keystrokes_and_nothing_else_does() {
        let (mut app, dir) = app_that_plays("links");
        // A handle: yt-dlp says which channel, and it opens then.
        app.on_key(key(KeyCode::Char('/')));
        app.on_terminal(Event::Paste("youtube.com/@BekBrace".into()));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.view, View::Home, "not a search");
        assert_eq!(app.loading.as_deref(), Some("Opening @BekBrace…"));
        let page = ChannelPage {
            id: ChannelId::parse(CH).unwrap(),
            title: "Bek Brace".into(),
            avatar: None,
            videos: vec![video("vid00000001", CH, Some(100))],
        };
        let request = app.request;
        app.on_event(AppEvent::Channel {
            request,
            result: Ok(page),
        });
        assert_eq!(
            app.view,
            View::Channel(ChannelId::parse(CH).unwrap(), "Bek Brace".into())
        );
        assert_eq!(app.videos.len(), 1);

        // A playlist, pasted as keystrokes with no box open.
        let list = "PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf";
        let link = format!("https://www.youtube.com/playlist?list={list}");
        app.on_terminal_events(as_keys(&link));
        assert!(matches!(&app.view, View::Playlist(_, title) if title == "Loading…"));
        let request = app.request;
        app.on_event(AppEvent::Playlist {
            request,
            id: PlaylistId::parse(list).unwrap(),
            result: Ok((
                "Rust talks".into(),
                vec![
                    video("vid00000002", CH, None),
                    video("vid00000003", CH, None),
                ],
            )),
        });
        assert_eq!(app.view.title(), "Playlist – Rust talks");
        assert_eq!(app.videos.len(), 2);

        // Your account's lists, and text that isn't a link, open nothing.
        let before = app.view.clone();
        app.on_terminal(Event::Paste(
            "https://www.youtube.com/playlist?list=WL".into(),
        ));
        assert!(app.status.as_ref().unwrap().text.contains("account's"));
        app.on_terminal(Event::Paste("never gonna".into()));
        app.on_terminal(Event::Paste(
            "https://evil.example/watch?v=dQw4w9WgXcQ".into(),
        ));
        assert_eq!(app.view, before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn e_fills_the_jukebox_and_j_and_k_reorder_it() {
        let mut app = app();
        with_feed(&mut app, 5);
        // Home lists the newest first: vid00000004 … vid00000000.
        app.on_key(key(KeyCode::Char('e')));
        app.select(2);
        app.on_key(key(KeyCode::Char('e')));
        assert!(app.status.as_ref().unwrap().text.contains("#2"));
        app.on_key(key(KeyCode::Char('e')));
        assert!(app.status.as_ref().unwrap().text.contains("already"));
        app.select(3);
        app.on_key(key(KeyCode::Char('E')));
        assert_eq!(jukebox(&app), ["vid00000001", "vid00000004", "vid00000002"]);
        // Outside the Jukebox, J, K and v do nothing.
        app.on_key(key(KeyCode::Char('J')));
        app.on_key(key(KeyCode::Char('v')));
        assert_eq!(jukebox(&app).len(), 3);
        assert!(!app.settings.jukebox_list);

        app.focus = Focus::Sidebar;
        app.on_key(key(KeyCode::Char('G')));
        while app.view != View::Jukebox {
            app.on_key(key(KeyCode::Char('k')));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Grid);
        assert_eq!(app.videos.len(), 3);
        app.on_key(key(KeyCode::Char('J')));
        assert_eq!(jukebox(&app), ["vid00000004", "vid00000001", "vid00000002"]);
        assert_eq!(app.selected, 1, "the selection goes with it");
        app.on_key(key(KeyCode::Char('J')));
        app.on_key(key(KeyCode::Char('J')));
        assert_eq!(jukebox(&app), ["vid00000004", "vid00000002", "vid00000001"]);
        app.on_key(key(KeyCode::Char('K')));
        app.on_key(key(KeyCode::Char('K')));
        app.on_key(key(KeyCode::Char('K')));
        assert_eq!(jukebox(&app), ["vid00000001", "vid00000004", "vid00000002"]);
        assert_eq!(app.selected, 0);
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(jukebox(&app), ["vid00000004", "vid00000002"]);
        assert_eq!(app.videos.len(), 2);
        // Kept, as Watch later is.
        assert_eq!(app.store.jukebox().unwrap().len(), 2);
        app.on_key(key(KeyCode::Char('v')));
        assert!(app.settings.jukebox_list);
        change_setting(&mut app, HelpMenu::JUKEBOX_LIST);
        assert!(!app.settings.jukebox_list);
        assert!(!app.settings.jukebox_autoremove, "off unless turned on");
        change_setting(&mut app, HelpMenu::JUKEBOX_AUTOREMOVE);
        assert!(app.settings.jukebox_autoremove);
    }

    fn ended(app: &mut App, video: &Video) {
        app.show_playing(video.clone(), 599.0, 600.0, false);
        mpv_says(app, [PlayerEventKind::Ended, PlayerEventKind::Exited]);
    }

    #[tokio::test]
    async fn the_jukebox_plays_on_from_where_it_is_whatever_autoplay_says() {
        let (mut app, dir) = app_that_plays("jukebox");
        with_feed(&mut app, 6);
        // Home lists the newest first: vid00000005 … vid00000000.
        let listed = app.videos.clone();
        let video = |n: usize| listed[5 - n].clone();
        for n in [1, 3, 5] {
            app.select(5 - n);
            app.on_key(key(KeyCode::Char('e')));
        }
        assert_eq!(jukebox(&app), ["vid00000001", "vid00000003", "vid00000005"]);

        // Played from Home: the Jukebox comes before the cards after it.
        app.select(1);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.up_next.len(), 4);
        assert_eq!(app.plays_next().unwrap().id, video(1).id);
        app.resolving = None;
        ended(&mut app, &video(4));
        assert_eq!(loading(&app), Some("vid00000001"));
        assert_eq!(app.jukebox_state(&video(1).id), Some(JukeboxState::Loading));
        assert_eq!(jukebox(&app).len(), 3, "it stays in the Jukebox");
        assert_eq!(app.up_next.len(), 3, "but leaves up next");

        // Can't be played: autoplay skips it. A bot check stops autoplay.
        resolved(&mut app, Err(anyhow::anyhow!("Video unavailable")));
        assert_eq!(loading(&app), Some("vid00000003"));
        resolved(
            &mut app,
            Err(anyhow::anyhow!("Sign in to confirm you're not a bot")),
        );
        assert_eq!(loading(&app), None);
        assert_eq!(jukebox(&app).len(), 3);

        // With autoplay off, the Jukebox still plays on, from where it is,
        // to its end.
        app.settings.autoplay = false;
        app.show_playing(video(3), 10.0, 600.0, false);
        assert_eq!(app.jukebox_state(&video(3).id), Some(JukeboxState::Playing));
        assert_eq!(app.jukebox_state(&video(1).id), None);
        assert_eq!(app.plays_next().unwrap().id, video(5).id);
        ended(&mut app, &video(3));
        assert_eq!(loading(&app), Some("vid00000005"));
        app.resolving = None;
        ended(&mut app, &video(5));
        assert_eq!(loading(&app), None, "after its last, nothing");
        assert_eq!(jukebox(&app).len(), 3, "kept: autoremove is off");

        // Autoremove on: one played to its end is taken off, and the Jukebox
        // goes on after it. One closed before its end stays.
        app.settings.jukebox_autoremove = true;
        app.show_playing(video(1), 100.0, 600.0, false);
        mpv_says(&mut app, [PlayerEventKind::Exited]);
        assert_eq!(jukebox(&app).len(), 3);
        ended(&mut app, &video(1));
        assert_eq!(jukebox(&app), ["vid00000003", "vid00000005"]);
        assert_eq!(loading(&app), Some("vid00000003"));

        // Enter in the Jukebox plays from there, and only the Jukebox after
        // it; E puts a video right after the one playing.
        app.show(View::Jukebox, false);
        app.select(0);
        app.on_key(key(KeyCode::Enter));
        assert!(app.up_next.is_empty());
        app.show(View::Home, false);
        app.select(3);
        app.on_key(key(KeyCode::Char('E')));
        assert_eq!(jukebox(&app), ["vid00000003", "vid00000002", "vid00000005"]);
        assert_eq!(app.plays_next().unwrap().id, video(2).id);

        // x on the one it's at: it goes on from the one after all the same.
        app.show(View::Jukebox, false);
        app.select(0);
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(jukebox(&app), ["vid00000002", "vid00000005"]);
        assert_eq!(app.plays_next().unwrap().id, video(2).id);

        // C asks first, and only y clears it.
        app.on_key(key(KeyCode::Char('C')));
        assert_eq!(app.confirm, Some(Confirm::ClearJukebox));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.confirm, None);
        assert_eq!(jukebox(&app).len(), 2, "kept");
        // On it in the sidebar, too.
        app.sync_sidebar();
        app.focus = Focus::Sidebar;
        app.on_key(key(KeyCode::Char('C')));
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.jukebox.is_empty() && app.store.jukebox().unwrap().is_empty());
        assert!(app.status.as_ref().unwrap().text.contains("2 videos"));
        app.on_key(key(KeyCode::Char('C')));
        assert_eq!(app.confirm, None, "nothing to ask about");
        let _ = std::fs::remove_dir_all(&dir);
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
