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
use crate::mpris::Ask;
use crate::pepper::{self, Placed};
use crate::player::{Pepper, Player, PlayerEvent, PlayerEventKind, Remote, Start};
use crate::settings::{self, Repeat, Settings};
use crate::shuffle::Rng;
use crate::sponsorblock::{self, Segment, Skipper};
use crate::store::{self, Channel, MAX_GROUP_NAME, Store};
use crate::takeout::{self, Takeout};
use crate::text;
use crate::theme::{self, Colors};
use crate::tools::Tools;
use crate::video::{self, Video};
use crate::visualizer::{self, Spectrum};
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
/// Repeat one: mpv going back to the start is a lap only from this close to
/// the end, so a seek back from midway doesn't skip SponsorBlock's segments
/// again.
const LAP_FROM_END: f64 = 3.0;
/// ...and they're skipped again only after a lap this long: one that was a
/// single skip (a segment over the whole video) plays the next lap, rather
/// than skipping to the end over and over.
const MIN_LAP: Duration = Duration::from_secs(5);
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
    /// The Now Playing picture of the playback numbered `play`: a file.
    Cover {
        play: u64,
        path: PathBuf,
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
    /// What the desktop asks through Now Playing on Linux.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Mpris(crate::mpris::Ask),
    /// tuitube is on the session bus again, Now Playing turned back on.
    MprisStarted(crate::mpris::Mpris),
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
    /// One of your groups of subscriptions: its channels' newest videos,
    /// as Home's. Its id and name.
    Group(i64, String),
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
            View::MyPlaylist(_, name) | View::Group(_, name) => name.clone(),
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
    /// One of your groups of subscriptions, by its place in
    /// [`App::groups`].
    Group(usize),
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

/// The pane the keys move in: the sidebar or the videos. The search box
/// is the third, open as `App.prompt`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Focus {
    Sidebar,
    Grid,
}

/// The way Ctrl and an arrow or `h j k l` go: to the pane that way. Without
/// Ctrl they move only in the pane that has the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Toward {
    Left,
    Down,
    Up,
    Right,
}

fn toward(key: &KeyEvent) -> Option<Toward> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    Some(match key.code {
        KeyCode::Char('h') | KeyCode::Left => Toward::Left,
        KeyCode::Char('j') | KeyCode::Down => Toward::Down,
        KeyCode::Char('k') | KeyCode::Up => Toward::Up,
        KeyCode::Char('l') | KeyCode::Right => Toward::Right,
        _ => return None,
    })
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
    /// The "every view as a list" row.
    pub const LIST: usize = Self::DESCRIPTIONS + 1;
    pub const AUTOPLAY: usize = Self::LIST + 1;
    pub const SHUFFLE: usize = Self::AUTOPLAY + 1;
    pub const SPONSORBLOCK: usize = Self::SHUFFLE + 1;
    pub const DEARROW: usize = Self::SPONSORBLOCK + 1;
    pub const SUBTITLES: usize = Self::DEARROW + 1;
    /// The picture's rows, one per height in [`settings::HEIGHTS`],
    /// tallest first.
    pub const VIDEO_QUALITY: usize = Self::SUBTITLES + 1;
    /// The sound's rows, one per [`AudioQuality`], best first.
    pub const AUDIO_QUALITY: usize = Self::VIDEO_QUALITY + settings::HEIGHTS.len();
    /// Listening: the sound alone, and the visualizer.
    pub const AUDIO_ONLY: usize = Self::AUDIO_QUALITY + AudioQuality::ALL.len();
    pub const VISUALIZER: usize = Self::AUDIO_ONLY + 1;
    /// The visualizer's bars in white, not the rainbow.
    pub const VISUALIZER_WHITE: usize = Self::VISUALIZER + 1;
    /// The Jukebox's rows: as a list, and taking off what has played.
    pub const JUKEBOX_LIST: usize = Self::VISUALIZER_WHITE + 1;
    pub const JUKEBOX_AUTOREMOVE: usize = Self::JUKEBOX_LIST + 1;
    pub const HISTORY: usize = Self::JUKEBOX_AUTOREMOVE + 1;
    pub const NOW_PLAYING: usize = Self::HISTORY + 1;
    /// In Pepper: the picture in the terminal.
    pub const INLINE_VIDEO: usize = Self::NOW_PLAYING + 1;
    /// The first theme's row. The themes come last, a long list.
    pub const THEMES: usize = Self::INLINE_VIDEO + 1;
    /// The last row: the last theme.
    pub const LAST: usize = Self::THEMES + theme::BUILT_IN.len() - 1;

    /// Whether `row` picks one of several (a quality, a theme) rather than
    /// turning something on or off.
    pub fn is_choice(row: usize) -> bool {
        (Self::VIDEO_QUALITY..Self::AUDIO_ONLY).contains(&row) || row >= Self::THEMES
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

/// The groups popup (`F`): the groups of your subscriptions, those a channel
/// is in ticked, to put it in or take it out of each, and a group's name
/// typed.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupMenu {
    pub channel: ChannelId,
    /// The channel's name, as shown.
    pub name: String,
    /// The groups it's in.
    pub inside: HashSet<i64>,
    /// The row under the cursor: a group, by its place in [`App::groups`],
    /// or past the last, "New group".
    pub selected: usize,
    /// The first group shown. Drawing moves it to show the selected one.
    pub scroll: usize,
    /// A name being typed, Enter to keep it.
    pub naming: Option<Naming>,
}

/// A group's name being typed: a new group's, or a new name for one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Naming {
    /// The group it renames; none for a new one.
    pub group: Option<i64>,
    pub text: String,
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

/// What a key did in a video's info: the `i` popup's, or the watch page's.
enum InfoKey {
    Handled,
    /// The Comments tab opened: they're asked for the first time.
    Comments,
    /// Enter on a description's line with a time: go there.
    GoTo(Video, f64),
    Ignored,
}

/// The watch page (Enter on a video, `p`): one video, laid out as YouTube's
/// watch page. Its picture is at the top left: inside Pepper, its thumbnail
/// elsewhere. Under it are its title and channel, then its description or
/// comments, and on the right what plays after it.
#[derive(Debug)]
pub struct Watch {
    /// The video's title, channel, description and comments, kept as the
    /// `i` popup's.
    pub info: InfoPopup,
    /// Where the keys go: the video's description or comments, or up next.
    pub pane: WatchPane,
    /// The video selected in up next, and its first row shown.
    pub next_selected: usize,
    pub next_scroll: Cell<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchPane {
    Info,
    UpNext,
}

/// A video's comments, as far as they've come.
#[derive(Clone, Debug, PartialEq)]
pub enum Comments {
    Loading,
    Loaded(Vec<crate::ytdlp::Comment>),
    Failed(String),
}

/// A question asked before anything is taken off a list: `y` does it, any
/// other key doesn't.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Empty the Jukebox.
    ClearJukebox,
    /// Delete one of your playlists.
    DeletePlaylist(i64),
    /// Delete one of your groups of subscriptions.
    DeleteGroup(i64),
    /// Take a video off a list, with its title as shown.
    TakeOff(Kept, VideoId, String),
    /// Unsubscribe from a channel, and its name.
    Unsubscribe(ChannelId, String),
}

/// A list of yours a video can be taken off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    WatchLater,
    Jukebox,
    History,
    /// One of your playlists, and its name.
    Playlist(i64, String),
}

impl Kept {
    /// The list the view shows, if it's one of yours.
    fn of(view: &View) -> Option<Kept> {
        match view {
            View::WatchLater => Some(Kept::WatchLater),
            View::Jukebox => Some(Kept::Jukebox),
            View::History => Some(Kept::History),
            View::MyPlaylist(id, name) => Some(Kept::Playlist(*id, name.clone())),
            _ => None,
        }
    }
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
    /// When it last started from its start (repeat one plays it in laps).
    lap: Instant,
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
    /// The visualizer's bars.
    pub spectrum: Spectrum,
    /// Whether the visualizer's filter is in mpv's sound.
    filter: Filter,
    /// mpv has started playing it: its sound is set up for a filter.
    started: bool,
    /// Its Now Playing picture, once made (`Images::cover`).
    cover: Option<PathBuf>,
    /// mpv has the picture.
    cover_sent: bool,
    /// The picture plays inside Pepper, through pepper-play: the token its
    /// box is placed with (`crate::pepper`).
    pub pepper: Option<String>,
}

/// The visualizer's filter in the sound of the mpv playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    Out,
    /// Asked for: mpv hasn't said yet whether it went in.
    Asked,
    In,
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

    /// SponsorBlock's segments of it that are skipped, for the seek bar.
    pub fn segments(&self) -> impl Iterator<Item = &Segment> {
        self.skipper.segments(self.duration)
    }

    /// Gives mpv the Now Playing picture once it's made and mpv plays.
    fn give_cover(&mut self) {
        if let Some(path) = self
            .cover
            .as_ref()
            .filter(|_| self.started && !self.cover_sent)
        {
            if crate::player::NOW_PLAYING {
                self.player.add_cover(path);
            }
            self.cover_sent = true;
        }
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
    /// tuitube on Linux's session bus, while `now_playing` is on.
    pub mpris: Option<crate::mpris::Mpris>,

    pub subscriptions: Vec<Channel>,
    /// Your playlists, imported from Takeout.
    pub my_playlists: Vec<store::Playlist>,
    /// Your groups of subscriptions, by name.
    pub groups: Vec<store::Group>,
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
    /// What was typed in the search box when Ctrl went to another pane: in
    /// it again when it's back.
    pub search_typed: Option<String>,
    /// The `?` popup, while it's open.
    pub help: Option<HelpMenu>,
    /// A question waiting for `y`, in a popup.
    pub confirm: Option<Confirm>,
    /// The chapters popup, while it's open.
    pub chapter_menu: Option<ChapterMenu>,
    /// The groups popup, while it's open.
    pub group_menu: Option<GroupMenu>,
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
    /// The whole list up next was taken from, in order (the view's cards,
    /// or the Mix): repeat all plays it again from its first once up next
    /// runs out.
    round: VecDeque<Video>,
    /// The Jukebox, in the order it plays. It stays as it plays, unless
    /// `jukebox_autoremove` takes off each video that played to its end.
    pub jukebox: Vec<Video>,
    /// Where the Jukebox is: the video last picked to play from it. It goes
    /// on from the one after; from its start while this is `None`.
    jukebox_at: Option<VideoId>,
    /// The video picked to play last came from the Jukebox: repeat all
    /// starts the Jukebox again after its last video, not the list up next.
    last_from_jukebox: bool,
    /// Up next came from a list shuffle plays in an order of its own (a
    /// playlist, Watch later or a Mix: `shuffles`).
    round_shuffles: bool,
    /// With shuffle on, the order the Jukebox plays in: each video once a
    /// round, and one put in somewhere after the one playing.
    jukebox_order: Vec<VideoId>,
    /// The video picked to play last: a new shuffled round doesn't start
    /// with it again.
    last_played: Option<VideoId>,
    /// Where shuffled orders come from.
    rng: Rng,
    /// Videos autoplay skipped in a row because they couldn't be played.
    skipped: u32,
    /// Muted (`m`), for this session.
    pub muted: bool,
    /// mpv's FFmpeg couldn't make the visualizer's filter: it isn't asked
    /// again this session.
    visualizer_failed: bool,
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
    /// Pepper's port (`PEPPER_PORT`), when tuitube runs in Pepper: the
    /// picture can play inside the terminal (`crate::pepper`). Set by
    /// `main`, never read here, so no test takes it from the terminal it
    /// runs in.
    pub pepper: Option<std::ffi::OsString>,
    /// Where the picture playing in Pepper goes, in cells, as the last frame
    /// laid it out: the watch page's video, or the picture-in-picture box.
    /// Set by `ui::draw`; none while nothing shows it.
    pub video_box: Cell<Option<ratatui::layout::Rect>>,
    /// The box Pepper was last told about.
    placed: Option<Placed>,
    /// The picture-in-picture box is hidden (`P`), for this session.
    pub pip_hidden: bool,
    /// The picture playing inside Pepper fills the whole tab (`f`), until
    /// `f` or Esc, or nothing plays or loads (`ui::draw`).
    pub fullscreen: bool,
    /// The video asked for again after its streams failed before anything
    /// played (`Failed`): once, not again.
    retried: Option<VideoId>,
    /// The videos that couldn't be played this session, with why (as
    /// shown): marked on their cards until they play (`could_not_play`).
    pub failed: HashMap<VideoId, String>,
    /// The watch page, while it's open.
    pub watch: Option<Watch>,
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
            mpris: None,
            subscriptions: Vec::new(),
            my_playlists: Vec::new(),
            groups: Vec::new(),
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
            focus: Focus::Sidebar,
            prompt: None,
            search_typed: None,
            help: None,
            confirm: None,
            chapter_menu: None,
            group_menu: None,
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
            round: VecDeque::new(),
            jukebox: Vec::new(),
            jukebox_at: None,
            last_from_jukebox: false,
            round_shuffles: false,
            jukebox_order: Vec::new(),
            last_played: None,
            rng: Rng::default(),
            skipped: 0,
            muted: false,
            visualizer_failed: false,
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
            pepper: None,
            video_box: Cell::new(None),
            placed: None,
            pip_hidden: false,
            fullscreen: false,
            retried: None,
            failed: HashMap::new(),
            watch: None,
            clipboard: None,
        };
        let _ = app.store.prune(now().saturating_sub(KEEP_VIDEOS));
        app.feeds_updated = app.store.feeds_updated().ok().flatten();
        app.reload_subscriptions();
        app.jukebox = app.store.jukebox().unwrap_or_default();
        app.sync_jukebox_order();
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
        // The demo's visualizer moves on these; a real one on mpv's answers.
        let mut frames = tokio::time::interval(visualizer::EVERY);
        frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        #[cfg(unix)]
        quit_on_hangup(self.tx.clone())?;
        self.refresh_feeds(false);
        loop {
            terminal.draw(|frame| crate::ui::draw(frame, self))?;
            write_to_terminal(&self.video_placement());
            self.tell_mpris();
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
                _ = frames.tick(), if self.demo && self.visualizing() => self.demo_frame(),
            }
        }
        self.save_volume();
        self.stop_playing();
        write_to_terminal(&self.video_placement());
        Ok(())
    }

    /// What to tell Pepper after a frame: where the picture playing goes now
    /// (`OSC 777;pepper`), or that it's gone, if that changed. Pepper tells
    /// pepper-play the box's size in pixels itself. Under a popup, the
    /// picture is taken away: it would cover it. Returns the escape
    /// sequences, written after the frame; empty when nothing changed.
    pub fn video_placement(&mut self) -> String {
        let token = self.playing.as_ref().and_then(|p| p.pepper.clone());
        let area = self.video_box.get().filter(|_| !self.popup_open());
        let wanted = token.zip(area).map(|(token, area)| Placed { token, area });
        if wanted == self.placed {
            return String::new();
        }
        let mut out = String::new();
        if let Some(old) = &self.placed
            && wanted.as_ref().is_none_or(|new| new.token != old.token)
        {
            out.push_str(&pepper::remove(&old.token));
        }
        if let Some(new) = &wanted {
            out.push_str(&pepper::place(&new.token, new.area));
        }
        self.placed = wanted;
        out
    }

    /// pepper-play and Pepper's port, when the picture plays inside the
    /// terminal: in Pepper, with `inline_video` on and pepper-play found.
    /// Never for sound alone, which has no picture.
    fn inline_player(&self, audio_only: bool) -> Option<(PathBuf, std::ffi::OsString)> {
        if audio_only || !self.settings.inline_video {
            return None;
        }
        Some((self.tools.pepper_play.clone()?, self.pepper.clone()?))
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

    pub(crate) fn reload_subscriptions(&mut self) {
        // The row selected stays selected when rows above it come or go: a
        // group made, a channel subscribed to.
        let selected = self.entry_view(self.entry(self.sidebar_selected));
        self.subscriptions = self.store.subscriptions().unwrap_or_default();
        self.my_playlists = self.store.playlists().unwrap_or_default();
        self.groups = self.store.groups().unwrap_or_default();
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
        self.sidebar_selected = selected
            .and_then(|view| {
                (0..entries).find(|&i| self.entry_view(self.entry(i)).as_ref() == Some(&view))
            })
            .unwrap_or(self.sidebar_selected)
            .min(entries.saturating_sub(1));
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
            View::Group(id, _) => self.store.group_feed(*id, MAX_LISTED),
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
        self.watch = None;
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
        self.watch = None;
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
            View::Group(id, _) => self
                .groups
                .iter()
                .position(|g| g.id == *id)
                .map(Entry::Group),
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
        self.menu().len() + self.groups.len() + self.my_playlists.len() + self.subscriptions.len()
    }

    /// The sidebar's `i`th row: the menu, your playlists, your groups, then
    /// your subscriptions.
    pub fn entry(&self, i: usize) -> Entry {
        let menu = self.menu();
        if let Some(entry) = menu.get(i) {
            return *entry;
        }
        let i = i - menu.len();
        if i < self.my_playlists.len() {
            return Entry::MyPlaylist(i);
        }
        let i = i - self.my_playlists.len();
        if i < self.groups.len() {
            Entry::Group(i)
        } else {
            Entry::Channel(i - self.groups.len())
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
            Entry::Group(i) => {
                let g = self.groups.get(i)?;
                View::Group(g.id, g.name.clone())
            }
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
            || self.group_menu.is_some()
            || self.command.is_some()
    }

    /// The video actions act on: the watch page's while it's open, else
    /// the selected card's.
    pub fn selected_video(&self) -> Option<&Video> {
        match &self.watch {
            Some(watch) => Some(&watch.info.video),
            None => self.videos.get(self.selected),
        }
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
        let shows_feeds = matches!(self.view, View::Home | View::Shorts | View::Group(..))
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
        if count == SEARCH_PAGE {
            // What was left typed in the search box is done with.
            self.search_typed = None;
            // A YouTube link typed or pasted in is opened, not searched for.
            if let Some(link) = links::parse(query) {
                return self.open_link(link);
            }
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
                    self.round = playable(videos.iter().filter(|v| !self.settings.hides(v)));
                    self.round_shuffles = true;
                    let mut rest = once_each(self.round.iter().filter(|v| v.id != seed));
                    if self.settings.shuffle {
                        self.rng.shuffle(&mut rest);
                    }
                    self.up_next = rest.into();
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
    /// the Jukebox, the Jukebox plays on after it. With shuffle on, a
    /// playlist, Watch later or a Mix plays every other video of it after
    /// it, each once, in an order of its own; the Jukebox starts a new
    /// shuffled round with it.
    fn play_selected(&mut self, audio_only: bool) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        let from_jukebox = self.view == View::Jukebox;
        // A picture plays on the watch page; sound alone, from the list.
        if !audio_only && !self.settings.audio_only {
            self.open_watch(video.clone());
        }
        self.round_shuffles = shuffles(&self.view);
        (self.round, self.up_next) = if from_jukebox {
            if self.settings.shuffle {
                self.shuffle_jukebox(Some(&video.id));
            }
            (VecDeque::new(), VecDeque::new())
        } else if self.shuffling() {
            let round = playable(self.videos.iter());
            let mut others = once_each(round.iter().filter(|v| v.id != video.id));
            self.rng.shuffle(&mut others);
            (round, others.into())
        } else {
            (
                playable(self.videos.iter()),
                playable(self.videos.iter().skip(self.selected + 1)),
            )
        };
        self.play(video, audio_only, false, from_jukebox);
    }

    /// `0`: back to the start of what plays, as YouTube's `0`, whatever is
    /// selected (N doesn't move the selection): the video loading if one
    /// is, which then starts there, else the one playing, as it plays (sound
    /// alone stays sound alone). With nothing playing, the selected video
    /// plays as Enter plays it, from its start rather than where it was left.
    fn back_to_start(&mut self) {
        if let Some(resolving) = self.resolving.as_mut() {
            // Not from where the video playing is, either.
            resolving.carry_on = false;
            let video = resolving.video.clone();
            self.start_once = Some((video.id.clone(), 0.0));
            let title = self.title(&video).to_string();
            return self.info(format!("“{title}” plays from its start"));
        }
        if self.playing.is_some() {
            return self.go_to(0.0);
        }
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        self.start_once = Some((video.id, 0.0));
        self.play_selected(false);
    }

    /// Whether shuffle plays what's up next in an order of its own.
    fn shuffling(&self) -> bool {
        self.settings.shuffle && self.round_shuffles
    }

    /// Whether what plays came from somewhere shuffle changes: the Jukebox,
    /// or a playlist, Watch later or a Mix. The player bar shows `z` then.
    pub fn shuffle_applies(&self) -> bool {
        self.last_from_jukebox || self.round_shuffles
    }

    /// The Jukebox in the order it plays: as it's kept, or with shuffle on,
    /// `jukebox_order`'s (any video it doesn't know yet after).
    fn jukebox_queue(&self) -> Vec<&Video> {
        if !self.settings.shuffle {
            return self.jukebox.iter().collect();
        }
        let by_id: HashMap<&VideoId, &Video> = self.jukebox.iter().map(|v| (&v.id, v)).collect();
        let mut queue: Vec<&Video> = self
            .jukebox_order
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        if queue.len() < self.jukebox.len() {
            let known: HashSet<&VideoId> = queue.iter().map(|v| &v.id).collect();
            let unknown: Vec<&Video> = self
                .jukebox
                .iter()
                .filter(|v| !known.contains(&v.id))
                .collect();
            queue.extend(unknown);
        }
        queue
    }

    /// A new shuffled round of the Jukebox: `first` (about to play), then
    /// every other video in an order of its own. Without `first`, from
    /// where it is: the videos up to the one it's at stay before it, played,
    /// and the rest are shuffled after it.
    fn shuffle_jukebox(&mut self, first: Option<&VideoId>) {
        let mut ids: Vec<VideoId> = self.jukebox.iter().map(|v| v.id.clone()).collect();
        let mut rest = match first {
            Some(first) => {
                ids.retain(|id| id != first);
                std::mem::replace(&mut ids, vec![first.clone()])
            }
            None => {
                let from = self
                    .jukebox_at
                    .as_ref()
                    .and_then(|at| ids.iter().position(|id| id == at))
                    .map_or(0, |i| i + 1);
                ids.split_off(from)
            }
        };
        self.rng.shuffle(&mut rest);
        ids.append(&mut rest);
        self.jukebox_order = ids;
    }

    /// Keeps the shuffled order to the Jukebox as it is now: a video taken
    /// off leaves it, and one put in goes somewhere after the one playing,
    /// so it plays this round. With shuffle off there's none.
    fn sync_jukebox_order(&mut self) {
        if !self.settings.shuffle {
            self.jukebox_order.clear();
            return;
        }
        let kept: HashSet<&VideoId> = self.jukebox.iter().map(|v| &v.id).collect();
        self.jukebox_order.retain(|id| kept.contains(id));
        let known: HashSet<VideoId> = self.jukebox_order.iter().cloned().collect();
        for video in &self.jukebox {
            if known.contains(&video.id) {
                continue;
            }
            let from = self
                .jukebox_at
                .as_ref()
                .and_then(|at| self.jukebox_order.iter().position(|id| id == at))
                .map_or(0, |i| i + 1);
            let at = from + self.rng.below(self.jukebox_order.len() - from + 1);
            self.jukebox_order.insert(at, video.id.clone());
        }
    }

    /// The place in the Jukebox's order of the video last picked from it,
    /// if it's still there.
    fn jukebox_place(&self) -> Option<usize> {
        let at = self.jukebox_at.as_ref()?;
        self.jukebox_queue().iter().position(|v| v.id == *at)
    }

    /// The Jukebox's next video this round: the first after the one last
    /// picked from it that can play (not a premiere or live stream that
    /// hasn't started).
    fn jukebox_after(&self) -> Option<&Video> {
        let queue = self.jukebox_queue();
        let from = self.jukebox_place().map_or(0, |i| i + 1);
        queue[from.min(queue.len())..]
            .iter()
            .find(|v| !v.upcoming)
            .copied()
    }

    /// The Jukebox's next video: the next this round. After its last, with
    /// repeat all while it's what plays, its first again, or shuffled, the
    /// first of a new round.
    fn jukebox_next(&self) -> Option<&Video> {
        let again =
            self.jukebox_place().is_some() && self.last_from_jukebox && self.repeats(Repeat::All);
        self.jukebox_after().or_else(|| {
            if !again {
                return None;
            }
            if !self.settings.shuffle {
                return self.jukebox.iter().find(|v| !v.upcoming);
            }
            // The round play_next makes, from a copy: the same.
            self.jukebox_round(&mut { self.rng })
                .into_iter()
                .find(|v| !v.upcoming)
        })
    }

    /// A new shuffled round of the whole Jukebox, not starting with the
    /// video it's at.
    fn jukebox_round(&self, rng: &mut Rng) -> Vec<&Video> {
        let at = self.jukebox_at.as_ref();
        rng.round(self.jukebox.iter().collect(), |v| Some(&v.id) == at)
    }

    /// Whether `repeat` is what plays again.
    fn repeats(&self, repeat: Repeat) -> bool {
        self.settings.repeat == repeat
    }

    /// The first up next; once there's none, with repeat all while the
    /// list up next is what plays, the first of its next round.
    fn up_next_front(&self) -> Option<&Video> {
        let again = !self.last_from_jukebox && self.repeats(Repeat::All);
        self.up_next.front().or_else(|| {
            if !again {
                return None;
            }
            // The round play_next makes, from a copy: the same.
            let first = *self.round_order(&mut { self.rng }).first()?;
            self.round.get(first)
        })
    }

    /// The order repeat all plays the list up next was taken from again
    /// (places in `round`): its own, or shuffled, not starting with the
    /// video that played last.
    fn round_order(&self, rng: &mut Rng) -> Vec<usize> {
        let order = (0..self.round.len()).collect();
        if !self.shuffling() {
            return order;
        }
        let last = self.last_played.as_ref();
        rng.round(order, |&i| Some(&self.round[i].id) == last)
    }

    /// The Jukebox goes back one: the video it was at plays next.
    fn jukebox_step_back(&mut self) {
        let before = {
            let queue = self.jukebox_queue();
            self.jukebox_place()
                .and_then(|i| i.checked_sub(1))
                .map(|i| queue[i].id.clone())
        };
        self.jukebox_at = before;
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
        self.jukebox_next().or(self.up_next_front())
    }

    /// What plays when the video playing ends: the Jukebox's next video
    /// whatever autoplay says (you put it there), else the first up next
    /// with autoplay on. Nothing with repeat one: it plays again.
    pub fn plays_next(&self) -> Option<&Video> {
        if self.repeats(Repeat::One) {
            return None;
        }
        self.jukebox_next()
            .or(self.up_next_front().filter(|_| self.settings.autoplay))
    }

    /// Plays the Jukebox's next video, else the first up next (only with
    /// autoplay on, if `auto`: the video before it ended). Whether there was
    /// one.
    fn play_next(&mut self, audio_only: bool, auto: bool) -> bool {
        let (video, from_jukebox) = match self.jukebox_next().cloned() {
            Some(video) => {
                // Shuffled, after its round: a new one, the one shown.
                if self.settings.shuffle && self.jukebox_after().is_none() {
                    let mut rng = self.rng;
                    let order: Vec<VideoId> = self
                        .jukebox_round(&mut rng)
                        .iter()
                        .map(|v| v.id.clone())
                        .collect();
                    (self.jukebox_order, self.rng) = (order, rng);
                }
                (video, true)
            }
            None if auto && !self.settings.autoplay => return false,
            None => {
                // Repeat all: the list again, from its first (shuffled, in
                // a new order).
                if self.up_next.is_empty() && self.up_next_front().is_some() {
                    let mut rng = self.rng;
                    let order = self.round_order(&mut rng);
                    self.up_next = order.into_iter().map(|i| self.round[i].clone()).collect();
                    self.rng = rng;
                }
                match self.up_next.pop_front() {
                    Some(video) => (video, false),
                    None => return false,
                }
            }
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
        // Audio only: whatever plays, however it was asked for, is the
        // sound alone.
        let audio_only = audio_only || self.settings.audio_only;
        let (Some(yt), Some(_)) = (self.yt.clone(), self.tools.mpv.as_ref()) else {
            return self.error(self.tools.missing().unwrap_or_default());
        };
        // The Jukebox goes on from here, played or not: autoplay skips one
        // that can't be played.
        if from_jukebox {
            self.jukebox_at = Some(video.id.clone());
        }
        self.last_from_jukebox = from_jukebox;
        self.last_played = Some(video.id.clone());
        // It doesn't play again from up next, and it's no longer one that
        // couldn't be played: it's loading.
        self.up_next.retain(|v| v.id != video.id);
        self.failed.remove(&video.id);
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
        // Not the video playing, which plays on (another quality of it
        // failed).
        let id = &resolving.video.id;
        if self.playing.as_ref().is_none_or(|p| p.video.id != *id) {
            self.could_not_play(id, shown);
        }
        // One in the list that can't be played (removed, private) is
        // skipped, a few in a row at most; a bot check stops autoplay.
        if resolving.auto && self.skipped < MAX_SKIPS && !slowed_down(&error) {
            self.skipped += 1;
            self.play_next(resolving.audio_only, true);
        }
    }

    /// A video that couldn't be played: marked on its card until it plays,
    /// with why in the status bar while it's selected, and selected in the
    /// view shown if it's there, since N and autoplay don't move the
    /// selection: Enter or `a` tries it again.
    fn could_not_play(&mut self, id: &VideoId, why: String) {
        if let Some(i) = self.videos.iter().position(|v| v.id == *id) {
            self.select(i);
        }
        self.failed.insert(id.clone(), why);
    }

    /// Why the selected video couldn't be played, where Enter would play
    /// it: the videos have the keys, or its page is open and up next
    /// doesn't.
    pub fn failed_selected(&self) -> Option<&str> {
        let enter_plays_it = match &self.watch {
            Some(watch) => watch.pane != WatchPane::UpNext,
            None => self.focus == Focus::Grid && self.prompt.is_none(),
        };
        if !enter_plays_it {
            return None;
        }
        let id = &self.selected_video()?.id;
        self.failed.get(id).map(String::as_str)
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
        // In Pepper, the picture plays in the terminal, through
        // pepper-play; anywhere else (or without a token), in mpv's window.
        let inline = self
            .inline_player(audio_only)
            .and_then(|(program, port)| Some((program, port, pepper::new_token()?)));
        let program = inline.as_ref().map_or(mpv, |(program, ..)| program.clone());
        let asked = here.or(start_once);
        let start_at = asked.or_else(|| {
            self.store
                .progress(&video.id)
                .ok()
                .flatten()
                .filter(|&(at, length)| at > RESUME_AFTER && at < length - RESUME_AFTER)
                .map(|(at, _)| at)
        });
        // Carried on from where it was left: `0` plays it from its start.
        let resumed = asked.is_none() && start_at.is_some();
        let start_at = start_at.filter(|&at| at > 0.0);
        self.plays += 1;
        // Subtitles in the first of your languages the video has them in;
        // none for sound alone.
        let languages = self.settings.subtitle_languages();
        let caption = crate::ytdlp::pick_caption(&streams.captions, &languages)
            .filter(|_| !audio_only)
            .cloned();
        let title = self.title(&video).to_string();
        let start = Start {
            mpv: &program,
            tools: &self.tools,
            streams: &streams,
            title: &title,
            start_at,
            audio_only,
            subtitles: caption.as_ref().map(|c| c.url.as_str()),
            subtitles_shown: self.settings.subtitles,
            volume: self.settings.volume(),
            muted: self.muted,
            looped: self.repeats(Repeat::One),
            // The channel under the title in Now Playing; not for a live
            // stream, which stays a bare URL.
            artist: (crate::player::NOW_PLAYING
                && self.settings.now_playing
                && !video.live
                && !video.channel.is_empty())
            .then_some(video.channel.as_str()),
            now_playing: self.settings.now_playing,
            pepper: inline
                .as_ref()
                .map(|(_, port, token)| Pepper { port, token }),
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
                let over = if resumed {
                    " · 0 plays it from the start"
                } else {
                    ""
                };
                self.info(format!("{what} “{title}”{from}{over}"));
                if !audio_only
                    && self
                        .watch
                        .as_ref()
                        .is_some_and(|w| w.info.video.id != video.id)
                {
                    self.open_watch(video.clone());
                }
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
                    lap: Instant::now(),
                    skip: None,
                    chapters: streams.chapters,
                    chapters_sent: false,
                    quality: streams.quality,
                    subtitles: caption.map(|c| c.name),
                    spectrum: Spectrum::default(),
                    filter: Filter::Out,
                    started: false,
                    cover: None,
                    cover_sent: false,
                    pepper: inline.map(|(.., token)| token),
                });
                self.look_up_segments();
                // Now Playing's picture, where mpv shows one.
                if let Some(playing) = &self.playing
                    && self.shows_now_playing()
                    && playing.controllable()
                {
                    let video = &playing.video;
                    self.images
                        .cover(&video.id, video.thumbnail_urls(), playing.player.play);
                }
            }
            Err(e) => {
                let name = if inline.is_some() {
                    "pepper-play"
                } else {
                    "mpv"
                };
                self.error(format!("{name} didn't start: {e}"));
            }
        }
    }

    /// Asks SponsorBlock what to skip in the video playing, if it's on and
    /// tuitube can seek in mpv (not on Windows yet). A live stream has
    /// nothing marked yet.
    fn look_up_segments(&mut self) {
        let Some(playing) = &self.playing else {
            return;
        };
        let categories = self.settings.skip_categories(playing.audio_only);
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
                // Repeat one: mpv went back to the start from the end. What
                // SponsorBlock marked is skipped again, as the first time.
                let lap_end = playing
                    .duration
                    .map_or(1.0, |d| (d - LAP_FROM_END).max(1.0));
                if self.settings.repeat == Repeat::One && at < 1.0 && playing.position >= lap_end {
                    if playing.lap.elapsed() >= MIN_LAP {
                        playing.skipper.again();
                    }
                    playing.lap = Instant::now();
                }
                playing.position = at;
                if let Some(segment) = playing.skipper.due(at, playing.duration) {
                    playing.player.seek_to(segment.end);
                    playing.skip = Some(Skip {
                        category: segment.category,
                        seconds: segment.end - at,
                        at: Instant::now(),
                    });
                    // Where it went, so a skip to the end ends a lap.
                    playing.position = segment.end;
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
            PlayerEventKind::Paused(paused) => {
                playing.paused = paused;
                self.update_visualizer();
            }
            PlayerEventKind::Restarted => {
                playing.started = true;
                self.retried = None;
                playing.give_cover();
                self.update_visualizer();
            }
            PlayerEventKind::Visualizer(added) => {
                // Taken out since it was asked for: it's out again by now.
                if playing.filter == Filter::Asked {
                    playing.filter = if added { Filter::In } else { Filter::Out };
                    if !added {
                        self.visualizer_failed = true;
                        self.error(
                            "No visualizer: this mpv's FFmpeg can't measure the sound (it needs astats, bandpass, amerge and pan)",
                        );
                    }
                }
                self.update_visualizer();
            }
            PlayerEventKind::Spectrum(levels) => {
                let now = Instant::now();
                playing.spectrum.push(levels, now);
                playing.spectrum.step(now);
            }
            // Changed in mpv's window too: the next video plays as loud.
            PlayerEventKind::Volume(volume) => {
                let volume = volume.round().clamp(0.0, 100.0) as u32;
                if volume != self.settings.volume() {
                    self.settings.volume = volume;
                    self.volume_unsaved = true;
                }
            }
            PlayerEventKind::Muted(muted) => self.muted = muted,
            PlayerEventKind::Remote(Remote::Next) => self.remote_next(),
            PlayerEventKind::Remote(Remote::Previous) => self.remote_previous(),
            PlayerEventKind::Ended => playing.ended = true,
            PlayerEventKind::Failed => self.on_stream_failed(),
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

    /// mpv couldn't play the streams. Before anything played (YouTube
    /// refused the addresses, as it now and then does: HTTP 403), new ones
    /// are asked for, once, from where it was to start; else it's said. mpv
    /// exits after, which stops the playback as any end does.
    fn on_stream_failed(&mut self) {
        let Some(playing) = &self.playing else {
            return;
        };
        let video = playing.video.clone();
        let (audio_only, from_jukebox, at) =
            (playing.audio_only, playing.from_jukebox, playing.position);
        let title = self.title(&video).to_string();
        if playing.started || self.retried.as_ref() == Some(&video.id) {
            self.retried = None;
            self.error(format!(
                "Can't play “{title}”: YouTube refused its stream. Try again in a moment; a newer yt-dlp may help"
            ));
            return self.could_not_play(&video.id, "YouTube refused its stream".into());
        }
        self.retried = Some(video.id.clone());
        if at > 0.0 {
            self.start_once = Some((video.id.clone(), at));
        }
        self.info(format!(
            "YouTube refused the stream of “{title}”: asking again"
        ));
        self.play(video, audio_only, false, from_jukebox);
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
        self.last_from_jukebox = from_jukebox;
        self.last_played = Some(video.id.clone());
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
            lap: Instant::now(),
            skip: None,
            chapters: Vec::new(),
            chapters_sent: false,
            quality: None,
            subtitles: None,
            spectrum: Spectrum::default(),
            filter: Filter::Out,
            started: false,
            cover: None,
            cover_sent: false,
            pepper: None,
        });
    }

    /// Whether the player bar shows the visualizer: it's on, only the sound
    /// plays, and mpv can say how loud it is (not on Windows yet), or it's
    /// the demo's.
    pub fn visualizing(&self) -> bool {
        self.settings.visualizer
            && !self.visualizer_failed
            && self.playing.as_ref().is_some_and(|p| {
                p.audio_only && (self.demo || (p.player.has_mpv() && p.controllable()))
            })
    }

    /// Puts the visualizer's filter in the mpv playing once it plays, or
    /// takes it out, and asks for its numbers while the bars show and the
    /// sound isn't paused (paused, they stay as they were).
    fn update_visualizer(&mut self) {
        let wanted = self.visualizing();
        let Some(playing) = self.playing.as_mut().filter(|p| p.player.has_mpv()) else {
            return;
        };
        match (wanted, playing.filter) {
            (true, Filter::Out) if playing.started => {
                playing.player.add_visualizer();
                playing.filter = Filter::Asked;
            }
            (false, Filter::Asked | Filter::In) => {
                playing.player.remove_visualizer();
                playing.filter = Filter::Out;
            }
            _ => {}
        }
        let follow = wanted && playing.filter == Filter::In && !playing.paused;
        playing.player.follow_spectrum(follow);
    }

    /// The demo's visualizer: made-up levels, as mpv's would come.
    pub fn demo_frame(&mut self) {
        if let Some(playing) = self.playing.as_mut() {
            let now = Instant::now();
            let seconds = now.saturating_duration_since(playing.lap).as_secs_f64();
            playing.spectrum.hear(visualizer::demo_levels(seconds), now);
        }
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

    /// The system's ⏭ (a media key, Control Center, a notch app): the next
    /// video, as N; the next chapter when nothing is up next.
    fn remote_next(&mut self) {
        let Some(playing) = &self.playing else {
            return;
        };
        let (audio_only, chapters) = (playing.audio_only, !playing.chapters.is_empty());
        if self.play_next(audio_only, false) {
            return;
        }
        if chapters {
            return self.step_chapter(true);
        }
        self.info("Nothing up next");
    }

    /// The system's ⏮: this chapter's start or the one before, as `[`; a
    /// video without chapters from its start.
    fn remote_previous(&mut self) {
        let Some(playing) = &self.playing else {
            return;
        };
        if playing.chapters.is_empty() {
            return self.go_to(0.0);
        }
        self.step_chapter(false);
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

    /// `w`: saves the selected video to Watch later, or asks to take it off.
    fn toggle_watch_later(&mut self) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        if self.watch_later.contains(&video.id) {
            return self.ask_to_take_off(Kept::WatchLater, &video);
        }
        let _ = self.store.save_videos(std::slice::from_ref(&video));
        match self.store.add_to_watch_later(&video.id, now()) {
            Ok(_) => {
                self.watch_later.insert(video.id.clone());
                self.info(format!("Saved to Watch later: “{}”", self.title(&video)));
            }
            Err(e) => self.error(format!("Couldn't save: {e}")),
        }
    }

    /// `x`: asks to take the selected video off the list shown, if it's one
    /// you keep.
    fn remove_selected(&mut self) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        if let Some(list) = Kept::of(&self.view) {
            self.ask_to_take_off(list, &video);
        }
    }

    fn ask_to_take_off(&mut self, list: Kept, video: &Video) {
        let title = self.title(video).to_string();
        self.confirm = Some(Confirm::TakeOff(list, video.id.clone(), title));
    }

    /// Takes the video off the list, after a `y`.
    fn take_off(&mut self, list: Kept, id: &VideoId, title: &str) {
        match list {
            Kept::WatchLater => {
                if let Err(e) = self.store.remove_from_watch_later(id) {
                    return self.error(format!("Couldn't save: {e}"));
                }
                self.watch_later.remove(id);
                self.info(format!("Removed from Watch later: “{title}”"));
                if self.view == View::WatchLater {
                    self.reload();
                }
            }
            Kept::Jukebox => {
                self.take_off_jukebox(id);
                self.info(format!("Taken off the Jukebox: “{title}”"));
            }
            Kept::History => {
                let _ = self.store.forget_watch(id);
                self.progress.remove(id);
                self.info(format!("Removed from History: “{title}”"));
                self.reload();
            }
            Kept::Playlist(playlist, name) => {
                if let Err(e) = self.store.remove_from_playlist(playlist, id) {
                    return self.error(format!("Couldn't save: {e}"));
                }
                self.info(format!("Taken off “{name}”: “{title}”"));
                self.reload_subscriptions();
                self.reload();
            }
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
        self.sync_jukebox_order();
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
                // Shuffled, it plays next all the same.
                if self.settings.shuffle {
                    self.jukebox_order.retain(|id| *id != video.id);
                    let after = self
                        .jukebox_at
                        .as_ref()
                        .and_then(|at| self.jukebox_order.iter().position(|id| id == at))
                        .map_or(0, |i| i + 1);
                    self.jukebox_order.insert(after, video.id.clone());
                }
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

    /// The settings tab's Jukebox row: the Jukebox as a list, or cards with
    /// thumbnails.
    fn toggle_jukebox_layout(&mut self) {
        self.settings.jukebox_list = !self.settings.jukebox_list;
        self.save_setting("jukebox_list", self.settings.jukebox_list.into());
    }

    /// The settings tab's row: every view as a list, or cards.
    fn toggle_list(&mut self) {
        self.settings.list = !self.settings.list;
        self.save_setting("list", self.settings.list.into());
    }

    /// Whether the videos show as a list, a row each, rather than cards:
    /// every view with `list`, the Jukebox with `jukebox_list` too.
    pub fn shows_list(&self) -> bool {
        self.settings.list || (self.view == View::Jukebox && self.settings.jukebox_list)
    }

    /// `v`: the view shown as a list, or as cards. In the Jukebox, only it,
    /// unless every view is a list: then every view goes back to cards, the
    /// Jukebox too.
    fn switch_layout(&mut self) {
        if self.view == View::Jukebox && !self.settings.list {
            self.toggle_jukebox_layout();
            return self.info(if self.settings.jukebox_list {
                "Jukebox: a list"
            } else {
                "Jukebox: cards"
            });
        }
        self.toggle_list();
        if !self.settings.list && self.view == View::Jukebox && self.settings.jukebox_list {
            self.toggle_jukebox_layout();
        }
        self.info(if self.settings.list {
            "Every view: a list"
        } else {
            "Every view: cards"
        });
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

    /// Subscribes to the selected video's channel, or asks to unsubscribe.
    fn toggle_subscription(&mut self) {
        let Some(video) = self.selected_video().cloned() else {
            return;
        };
        let Some(id) = video.channel_id.clone() else {
            return self.error("YouTube didn't say whose video this is");
        };
        if self.store.is_subscribed(&id).unwrap_or(false) {
            self.confirm = Some(Confirm::Unsubscribe(id, video.channel));
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
        if matches!(self.view, View::Home | View::Shorts | View::Group(..)) {
            self.reload();
        }
    }

    fn undo_unsubscribe(&mut self) {
        if let Some((id, name)) = self.last_unsubscribed.take() {
            let _ = self.store.subscribe(&id, &name);
            self.gone_checks.remove(&id);
            self.reload_subscriptions();
            self.info(format!("Subscribed to {name} again"));
            if matches!(self.view, View::Home | View::Shorts | View::Group(..)) {
                self.reload();
            }
        }
    }

    // Groups of subscriptions.

    /// `F`: the groups popup for a channel you subscribe to: the one
    /// selected in the sidebar, else the selected video's (the watch
    /// page's), else the channel shown. With no group yet, it opens on
    /// naming the first.
    fn open_groups(&mut self) {
        let channel = if self.watch.is_none() && self.focus == Focus::Sidebar {
            match self.entry(self.sidebar_selected) {
                Entry::Channel(i) => self
                    .subscriptions
                    .get(i)
                    .map(|c| (c.id.clone(), c.title.clone())),
                _ => {
                    return self
                        .info("F on a subscription, or on a video, puts its channel in groups");
                }
            }
        } else {
            self.selected_video()
                .and_then(|v| Some((v.channel_id.clone()?, v.channel.clone())))
                .or_else(|| match &self.view {
                    View::Channel(id, name) => Some((id.clone(), name.clone())),
                    _ => None,
                })
        };
        let Some((id, name)) = channel else {
            return self.error("YouTube didn't say whose video this is");
        };
        let Some(subscribed) = self.subscriptions.iter().find(|c| c.id == id) else {
            return self.info(format!(
                "Groups are of your subscriptions: S subscribes to {name} first"
            ));
        };
        let name = if subscribed.title.is_empty() {
            name
        } else {
            subscribed.title.clone()
        };
        let inside = self.store.groups_of(&id).unwrap_or_default();
        self.group_menu = Some(GroupMenu {
            channel: id,
            name,
            inside: inside.into_iter().collect(),
            selected: 0,
            scroll: 0,
            naming: self.groups.is_empty().then(Naming::default),
        });
    }

    /// The groups popup's keys: Enter or Space puts the channel in the group
    /// under the cursor or takes it out of it; on "New group", or with `n`,
    /// a name is typed; `r` renames the group under the cursor and `x`
    /// deletes it, after a `y`. Esc closes it, or leaves the name typed.
    fn on_group_key(&mut self, key: KeyEvent, ctrl: bool) {
        let count = self.groups.len();
        let Some(menu) = self.group_menu.as_mut() else {
            return;
        };
        if let Some(naming) = menu.naming.as_mut() {
            match key.code {
                // With no group to tick, the popup has nothing else.
                KeyCode::Esc if count == 0 => self.group_menu = None,
                KeyCode::Esc => menu.naming = None,
                KeyCode::Enter => self.name_group(),
                KeyCode::Backspace => {
                    naming.text.pop();
                }
                KeyCode::Char('u') if ctrl => naming.text.clear(),
                KeyCode::Char(c)
                    if !ctrl && !c.is_control() && naming.text.chars().count() < MAX_GROUP_NAME =>
                {
                    naming.text.push(c);
                }
                _ => {}
            }
            return;
        }
        // The groups, then "New group".
        let last = count;
        let group = self
            .groups
            .get(menu.selected)
            .map(|g| (g.id, g.name.clone()));
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'F') if !ctrl => self.group_menu = None,
            KeyCode::Char('j') | KeyCode::Down => menu.selected = (menu.selected + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => menu.selected = 0,
            KeyCode::Char('G') | KeyCode::End => menu.selected = last,
            KeyCode::Char('n') if !ctrl => menu.naming = Some(Naming::default()),
            KeyCode::Enter | KeyCode::Char(' ') => match group {
                Some((id, _)) => self.toggle_in_group(id),
                None => menu.naming = Some(Naming::default()),
            },
            KeyCode::Char('r') if !ctrl => {
                if let Some((id, name)) = group {
                    menu.naming = Some(Naming {
                        group: Some(id),
                        text: name,
                    });
                }
            }
            KeyCode::Char('x' | 'd') if !ctrl => {
                if let Some((id, _)) = group {
                    self.confirm = Some(Confirm::DeleteGroup(id));
                }
            }
            _ => {}
        }
    }

    /// Puts the popup's channel in the group, or takes it out of it.
    fn toggle_in_group(&mut self, group: i64) {
        let Some(menu) = self.group_menu.as_mut() else {
            return;
        };
        let inside = !menu.inside.contains(&group);
        if let Err(e) = self.store.set_in_group(group, &menu.channel, inside) {
            return self.error(format!("Couldn't save: {e}"));
        }
        if inside {
            menu.inside.insert(group);
        } else {
            menu.inside.remove(&group);
        }
        let channel = menu.name.clone();
        self.reload_subscriptions();
        if matches!(self.view, View::Group(id, _) if id == group) {
            self.reload();
        }
        let name = self.group_name(group);
        self.info(if inside {
            format!("Put {channel} in “{name}”")
        } else {
            format!("Took {channel} out of “{name}”")
        });
    }

    /// Enter on a name typed: a new group, with the popup's channel in it,
    /// or the group renamed.
    fn name_group(&mut self) {
        let Some(menu) = self.group_menu.as_mut() else {
            return;
        };
        let Some(naming) = menu.naming.clone() else {
            return;
        };
        let name = video::one_line(&naming.text, MAX_GROUP_NAME);
        if name.is_empty() {
            return self.error("A group needs a name");
        }
        let channel = (menu.channel.clone(), menu.name.clone());
        let group = match naming.group {
            Some(group) => match self.store.rename_group(group, &name) {
                Ok(true) => group,
                Ok(false) => return self.error(format!("There's a group called “{name}” already")),
                Err(e) => return self.error(format!("Couldn't save: {e}")),
            },
            None => {
                let made = self.store.create_group(&name).and_then(|group| {
                    self.store.set_in_group(group, &channel.0, true)?;
                    Ok(group)
                });
                match made {
                    Ok(group) => group,
                    Err(e) => return self.error(format!("Couldn't save: {e}")),
                }
            }
        };
        self.reload_subscriptions();
        // A new name everywhere the group is shown, or was.
        let renamed = |view: &mut View| {
            if let View::Group(id, shown) = view
                && *id == group
            {
                *shown = name.clone();
            }
        };
        renamed(&mut self.view);
        self.back.iter_mut().for_each(|(view, _)| renamed(view));
        if matches!(self.view, View::Group(id, _) if id == group) {
            self.reload();
        }
        let place = self.groups.iter().position(|g| g.id == group);
        if let Some(menu) = self.group_menu.as_mut() {
            menu.naming = None;
            menu.selected = place.unwrap_or(menu.selected);
            if naming.group.is_none() {
                menu.inside.insert(group);
            }
        }
        self.info(match naming.group {
            Some(_) => format!("Renamed the group “{name}”"),
            None => format!("Put {} in the new group “{name}”", channel.1),
        });
    }

    fn group_name(&self, group: i64) -> String {
        self.groups
            .iter()
            .find(|g| g.id == group)
            .map(|g| g.name.clone())
            .unwrap_or_default()
    }

    /// Deletes the group, after a `y`: its channels stay subscriptions.
    fn delete_group(&mut self, group: i64) {
        let name = self.group_name(group);
        if let Err(e) = self.store.delete_group(group) {
            return self.error(format!("Couldn't save: {e}"));
        }
        self.back
            .retain(|(view, _)| !matches!(view, View::Group(id, _) if *id == group));
        self.reload_subscriptions();
        if matches!(self.view, View::Group(id, _) if id == group) {
            if self.back.is_empty() {
                self.back.push((View::Home, 0));
            }
            self.go_back();
        }
        if let Some(menu) = self.group_menu.as_mut() {
            menu.inside.remove(&group);
            menu.selected = menu.selected.min(self.groups.len());
        }
        self.info(format!(
            "Deleted the group “{name}”: its channels are still your subscriptions"
        ));
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

    /// `z`: shuffle on or off. On, what's left of the Jukebox, and of a
    /// playlist, Watch later or a Mix up next, plays in an order of its
    /// own; off, in the list's own order again. Either way no video plays
    /// twice before its round is over.
    fn toggle_shuffle(&mut self) {
        self.settings.shuffle = !self.settings.shuffle;
        self.save_setting("shuffle", self.settings.shuffle.into());
        if self.settings.shuffle {
            self.shuffle_jukebox(None);
        } else {
            self.jukebox_order.clear();
        }
        if self.round_shuffles {
            let mut left: Vec<Video> = self.up_next.drain(..).collect();
            if self.settings.shuffle {
                self.rng.shuffle(&mut left);
            } else {
                let place = |v: &Video| self.round.iter().position(|r| r.id == v.id);
                left.sort_by_key(|v| place(v).unwrap_or(usize::MAX));
            }
            self.up_next = left.into();
        }
        self.info(if self.settings.shuffle {
            "Shuffle on: the Jukebox, playlists and Mixes play in an order of their own, each video once"
        } else {
            "Shuffle off: lists play in their own order"
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
        let categories = self.settings.skip_categories(false);
        if categories.is_empty() {
            return self.error(
                "SponsorBlock on, but sponsorblock_categories in settings.toml names none tuitube knows",
            );
        }
        let names: Vec<&str> = categories.iter().map(|c| sponsorblock::shown(c)).collect();
        let listening = if categories.contains(&sponsorblock::NOT_MUSIC) {
            ""
        } else {
            " (and what isn't music, while you listen)"
        };
        self.info(format!(
            "SponsorBlock on: skips {}{listening}. sponsor.ajay.app gets a hash of each video's id",
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

    /// Audio only on or off. Turned on while a video plays with its picture,
    /// it goes on as sound alone from where it is, as YouTube Music's switch
    /// does (where mpv can say where it is).
    fn toggle_audio_only(&mut self) {
        self.settings.audio_only = !self.settings.audio_only;
        self.save_setting("audio_only", self.settings.audio_only.into());
        if !self.settings.audio_only {
            return self
                .info("Audio only off: Enter plays videos with their picture again, a listens");
        }
        let what =
            "Audio only on: Enter listens, as a does: no window, and only the sound is downloaded";
        if self.playing.as_ref().is_some_and(|p| !p.audio_only) {
            // play() makes it sound only now.
            self.play_again(what.into(), false);
        } else {
            self.info(what);
        }
    }

    /// In Pepper, the picture in the terminal or in mpv's window: from the
    /// next video on.
    fn toggle_inline_video(&mut self) {
        self.settings.inline_video = !self.settings.inline_video;
        self.save_setting("inline_video", self.settings.inline_video.into());
        let where_ = if self.settings.inline_video {
            "in the terminal"
        } else {
            "in mpv's window"
        };
        self.info(if self.pepper.is_some() {
            format!("Videos play {where_} from the next one on")
        } else {
            format!("Videos play {where_} in Pepper (this terminal isn't)")
        });
    }

    fn toggle_visualizer(&mut self) {
        self.settings.visualizer = !self.settings.visualizer;
        self.save_setting("visualizer", self.settings.visualizer.into());
        self.update_visualizer();
        self.info(if !self.settings.visualizer {
            "Visualizer off"
        } else if self.visualizer_failed {
            "Visualizer on, but this mpv's FFmpeg can't measure the sound"
        } else {
            "Visualizer on: bars that move with the sound, while only the sound plays"
        });
    }

    fn toggle_visualizer_white(&mut self) {
        self.settings.visualizer_white = !self.settings.visualizer_white;
        self.save_setting("visualizer_white", self.settings.visualizer_white.into());
        self.info(if self.settings.visualizer_white {
            "Visualizer in white"
        } else {
            "Visualizer in the rainbow's colors"
        });
    }

    /// `r` goes round them: off, all, one. Repeat one starts or ends on the
    /// video playing at once, where mpv can be told.
    fn set_repeat(&mut self, repeat: Repeat) {
        let looped = |r: Repeat| r == Repeat::One;
        let turns = looped(self.settings.repeat) != looped(repeat);
        self.settings.repeat = repeat;
        self.save_setting("repeat", repeat.name().into());
        let later = match self.playing.as_ref().map(|p| (p, p.controllable())) {
            Some((playing, true)) => {
                playing.player.set_looped(looped(repeat));
                ""
            }
            Some((_, false)) if turns => ", from the next video",
            _ => "",
        };
        self.info(match repeat {
            Repeat::Off => format!("Repeat off{later}"),
            Repeat::All if self.settings.autoplay => {
                format!("Repeat all: after the last video, the first again{later}")
            }
            Repeat::All => format!(
                "Repeat all: after the Jukebox's last video, its first again{later}. Lists play on with autoplay: A"
            ),
            Repeat::One => format!("Repeat one: the video plays again each time it ends{later}"),
        });
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

    /// `m`: sound off or on again, for this session.
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

    /// Now Playing on or off: for the video playing at once, and the ones
    /// after it.
    fn toggle_now_playing(&mut self) {
        self.settings.now_playing = !self.settings.now_playing;
        self.save_setting("now_playing", self.settings.now_playing.into());
        self.apply_now_playing();
        let anywhere = crate::player::NOW_PLAYING || crate::mpris::AVAILABLE;
        self.info(match (anywhere, self.settings.now_playing) {
            (false, _) => "Now Playing is only on macOS and Linux for now",
            (true, true) => {
                "Now Playing on: what plays shows in Control Center and notch apps, with the media keys"
            }
            (true, false) => {
                "Now Playing off: other apps aren't shown what plays, and the media keys don't reach it"
            }
        });
    }

    /// Puts what plays in Now Playing or takes it out: on Linux, tuitube
    /// joins the session bus or leaves it; on macOS, the mpv playing is told.
    /// Put in, the video playing gets its picture if it had none; on macOS
    /// its channel shows from the next video.
    fn apply_now_playing(&mut self) {
        let on = self.settings.now_playing;
        if crate::mpris::AVAILABLE {
            if !on {
                self.mpris = None;
            } else if self.mpris.is_none() {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    if let Ok(mpris) = crate::mpris::Mpris::start(tx.clone()).await {
                        let _ = tx.send(AppEvent::MprisStarted(mpris));
                    }
                });
            }
        }
        let Some(playing) = self.playing.as_ref().filter(|p| p.controllable()) else {
            return;
        };
        if crate::player::NOW_PLAYING {
            playing.player.set_now_playing(on);
        }
        if on && playing.cover.is_none() && (crate::player::NOW_PLAYING || crate::mpris::AVAILABLE)
        {
            let video = &playing.video;
            self.images
                .cover(&video.id, video.thumbnail_urls(), playing.player.play);
        }
    }

    /// Now Playing shows what plays, here: mpv's own on macOS, tuitube on
    /// the session bus on Linux.
    fn shows_now_playing(&self) -> bool {
        self.settings.now_playing && (crate::player::NOW_PLAYING || self.mpris.is_some())
    }

    /// What Now Playing shows on Linux: the video playing, as tuitube shows
    /// it.
    pub fn now_shown(&self) -> crate::mpris::Shown {
        let volume = if self.muted {
            0.0
        } else {
            f64::from(self.settings.volume()) / 100.0
        };
        let Some(playing) = &self.playing else {
            return crate::mpris::Shown {
                volume,
                ..Default::default()
            };
        };
        let video = &playing.video;
        crate::mpris::Shown {
            track: Some(crate::mpris::Track {
                play: playing.player.play,
                title: self.title(video).to_string(),
                channel: video.channel.clone(),
                url: video.id.url(),
                art: playing
                    .cover
                    .as_deref()
                    .and_then(|path| reqwest::Url::from_file_path(path).ok())
                    .map(String::from),
                length: playing.duration,
            }),
            paused: playing.paused,
            position: playing.position,
            volume,
            can_seek: playing.controllable() && !video.live,
        }
    }

    /// Tells Now Playing on Linux what plays, once a wake: only what
    /// changed goes out.
    fn tell_mpris(&mut self) {
        if self.mpris.is_none() {
            return;
        }
        let shown = self.now_shown();
        if let Some(mpris) = self.mpris.as_mut() {
            mpris.update(shown);
        }
    }

    /// What the desktop asks through Now Playing: what the keys do.
    fn on_mpris(&mut self, ask: Ask) {
        let Some(playing) = &self.playing else {
            return;
        };
        let (play, paused, at) = (playing.player.play, playing.paused, playing.position);
        let length = playing.duration.unwrap_or(f64::INFINITY);
        match ask {
            Ask::Next => self.remote_next(),
            Ask::Previous => self.remote_previous(),
            Ask::Play | Ask::Pause if (ask == Ask::Play) != paused => {}
            Ask::Play | Ask::Pause | Ask::PlayPause => playing.player.toggle_pause(),
            Ask::Stop => self.stop(),
            // Before the start is the start; past the end, the end.
            Ask::Seek(ahead) if ahead.is_finite() => self.go_to((at + ahead).clamp(0.0, length)),
            Ask::SetPosition { play: asked, at }
                if asked == play && at.is_finite() && (0.0..=length).contains(&at) =>
            {
                self.go_to(at);
            }
            Ask::Volume(volume) if volume.is_finite() => {
                self.set_volume((volume.clamp(0.0, 1.0) * 100.0).round() as u32);
            }
            Ask::Seek(_) | Ask::SetPosition { .. } | Ask::Volume(_) => {}
        }
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
                if self.settings.now_playing && !on_disk.now_playing {
                    self.settings.now_playing = false;
                    self.apply_now_playing();
                }
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
            AppEvent::Cover { play, path } => {
                if let Some(playing) = self.playing.as_mut().filter(|p| p.player.play == play) {
                    playing.cover = Some(path);
                    playing.give_cover();
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
            AppEvent::Mpris(ask) => self.on_mpris(ask),
            AppEvent::MprisStarted(mpris) => {
                // Turned off again while it connected: given up at once.
                if self.settings.now_playing {
                    self.mpris = Some(mpris);
                }
            }
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
        let naming = self.group_menu.as_ref().is_some_and(|m| m.naming.is_some());
        if self.prompt.is_none() && self.command.is_none() && !naming {
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
                } else if let Some(naming) =
                    self.group_menu.as_mut().and_then(|m| m.naming.as_mut())
                {
                    let room = MAX_GROUP_NAME.saturating_sub(naming.text.chars().count());
                    let line = video::one_line(text::first_chars(&text, 2 * MAX_GROUP_NAME), room);
                    naming.text.push_str(&line);
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
                    Confirm::DeleteGroup(id) => self.delete_group(id),
                    Confirm::TakeOff(list, id, title) => self.take_off(list, &id, &title),
                    Confirm::Unsubscribe(id, name) => self.unsubscribe(&id, &name),
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
        if self.group_menu.is_some() {
            return self.on_group_key(key, ctrl);
        }
        // Esc leaves full screen before it closes the page under it.
        if self.fills_tab() && key.code == KeyCode::Esc {
            self.fullscreen = false;
            return;
        }
        if self.watch.is_some() && self.on_watch_key(key, ctrl) {
            return;
        }
        if std::mem::take(&mut self.pending_g) && key.code == KeyCode::Char('g') {
            return self.jump_to(0);
        }
        if let Some(way) = toward(&key) {
            return self.go_toward(way);
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
            KeyCode::Char('/' | 's') if !ctrl => self.open_search(),
            KeyCode::Char('I') if !ctrl => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Import,
                    text: String::new(),
                });
            }
            KeyCode::Char('A') if !ctrl => self.toggle_autoplay(),
            KeyCode::Char('r') if !ctrl => self.set_repeat(self.settings.repeat.next()),
            KeyCode::Char('z') if !ctrl => self.toggle_shuffle(),
            KeyCode::Char('T') if !ctrl => self.toggle_subtitles(),
            KeyCode::Char('F') if !ctrl => self.open_groups(),
            KeyCode::Char('i') if !ctrl => self.open_info(),
            KeyCode::Char('p') if !ctrl => self.toggle_watch(),
            KeyCode::Char('P') if !ctrl => self.toggle_pip(),
            KeyCode::Char('f') if !ctrl => self.toggle_fullscreen(),
            KeyCode::Char('N') if !ctrl => self.skip_to_next(),
            // From the sidebar too, while something plays.
            KeyCode::Char('0') if !ctrl && (self.playing.is_some() || self.resolving.is_some()) => {
                self.back_to_start();
            }
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
            KeyCode::Char('m') if !ctrl => self.toggle_mute(),
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
            // The watch page covers the sidebar and the cards.
            _ if self.watch.is_some() => {}
            _ => match self.focus {
                Focus::Sidebar => self.on_sidebar_key(key),
                Focus::Grid => self.on_grid_key(key),
            },
        }
    }

    /// X: stops playing, leaves the video's page for the list, and forgets
    /// what was up next.
    fn stop(&mut self) {
        self.fullscreen = false;
        self.watch = None;
        self.stop_playing();
        self.resolving = None;
        self.up_next.clear();
        self.round.clear();
        self.round_shuffles = false;
        self.info("Stopped");
    }

    /// Ctrl and an arrow or `h j k l` in the sidebar or the videos: the
    /// pane that way (the search box's keys go from it themselves).
    fn go_toward(&mut self, way: Toward) {
        match (self.focus, way) {
            (Focus::Sidebar, Toward::Right) => self.go_to_videos(),
            (Focus::Grid, Toward::Left) => self.focus = Focus::Sidebar,
            (Focus::Grid, Toward::Up) => self.open_search(),
            _ => {}
        }
    }

    /// The search box, with what was typed in it when it was left, else
    /// the last search.
    fn open_search(&mut self) {
        let text = self
            .search_typed
            .take()
            .unwrap_or_else(|| self.last_query());
        self.prompt = Some(Prompt {
            kind: PromptKind::Search,
            text,
        });
    }

    /// `l` or `→` in the sidebar (Ctrl too): into the videos, opening the
    /// row. On Search, into its results, or the search box while there are
    /// none; `Enter` there always goes to the search box.
    fn go_to_videos(&mut self) {
        let entry = self.entry(self.sidebar_selected);
        if entry != Entry::Search {
            return self.open_entry(entry);
        }
        if matches!(self.view, View::Search(_)) && !self.videos.is_empty() {
            self.focus = Focus::Grid;
        } else {
            self.open_search();
        }
    }

    /// Opens a sidebar row: its list, or the search box.
    fn open_entry(&mut self, entry: Entry) {
        if entry == Entry::Search {
            return self.open_search();
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
            Command::Repeat(repeat) => {
                self.set_repeat(repeat.unwrap_or(self.settings.repeat.next()))
            }
            Command::Mute => self.toggle_mute(),
            Command::Play => self.play_selected(false),
            Command::Listen => self.play_selected(true),
            Command::FromStart => self.back_to_start(),
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
            Command::Groups => self.open_groups(),
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
            Toggle::Shuffle => self.toggle_shuffle(),
            Toggle::Subtitles => self.toggle_subtitles(),
            Toggle::SponsorBlock => self.toggle_sponsorblock(),
            Toggle::DeArrow => self.toggle_dearrow(),
            Toggle::Shorts => self.toggle_shorts(),
            Toggle::Live => self.toggle_live(),
            Toggle::History => self.toggle_history(),
            Toggle::NowPlaying => self.toggle_now_playing(),
            Toggle::InlineVideo => self.toggle_inline_video(),
            Toggle::JukeboxAutoremove => self.toggle_jukebox_autoremove(),
            Toggle::AudioOnly => self.toggle_audio_only(),
            Toggle::Visualizer => self.toggle_visualizer(),
            Toggle::VisualizerWhite => self.toggle_visualizer_white(),
            Toggle::Descriptions => {
                self.toggle_descriptions();
                self.info(if self.settings.descriptions {
                    "Descriptions shown"
                } else {
                    "Descriptions hidden"
                });
            }
            Toggle::List => {
                self.toggle_list();
                self.info(if self.settings.list {
                    "Every view: a list"
                } else {
                    "Every view: cards"
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
        // Ctrl down or left: out of the search box to the videos or the
        // sidebar, keeping what was typed for when it's back. Ctrl-h stays a
        // Backspace here, as in a shell: some terminals send it for one.
        if prompt.kind == PromptKind::Search
            && key.code != KeyCode::Char('h')
            && let Some(way @ (Toward::Down | Toward::Left)) = toward(&key)
        {
            let text = std::mem::take(&mut prompt.text);
            self.prompt = None;
            self.search_typed = (text != self.last_query()).then_some(text);
            self.focus = match way {
                Toward::Down => Focus::Grid,
                _ => Focus::Sidebar,
            };
            return;
        }
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
            KeyCode::Char('h') if ctrl => {
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

    /// A video's comments, asked for the first time its Comments tab opens
    /// (the `i` popup's, or the watch page's): yt-dlp reads its page, as
    /// for playing.
    fn load_comments(&mut self, id: VideoId) {
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
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q' | 'i')) {
            self.info_popup = None;
            return;
        }
        match info_key(popup, key, ctrl) {
            InfoKey::Comments => {
                let id = popup.video.id.clone();
                self.load_comments(id);
            }
            InfoKey::GoTo(video, at) => {
                self.info_popup = None;
                self.go_to_in(video, at);
            }
            InfoKey::Handled | InfoKey::Ignored => {}
        }
    }

    /// Opens the watch page on `video`, the whole of it as stored.
    fn open_watch(&mut self, video: Video) {
        let video = self.store.video(&video.id).ok().flatten().unwrap_or(video);
        self.watch = Some(Watch {
            info: InfoPopup {
                video,
                tab: InfoTab::Description,
                line: 0,
                scroll: 0,
                description_scroll: Cell::new(0),
            },
            pane: WatchPane::Info,
            next_selected: 0,
            next_scroll: Cell::new(0),
        });
    }

    /// `p`: the watch page of the video playing (or loading), else of the
    /// selected one; from the page, back to the list.
    fn toggle_watch(&mut self) {
        if self.watch.take().is_some() {
            return;
        }
        let video = self
            .playing
            .as_ref()
            .map(|p| &p.video)
            .or(self.resolving.as_ref().map(|r| &r.video))
            .or(self.selected_video())
            .cloned();
        if let Some(video) = video {
            self.open_watch(video);
        }
    }

    /// `f`: inside Pepper, the picture fills the whole tab, or goes back
    /// (Esc too); in mpv's window, that window fills the screen, or goes
    /// back.
    fn toggle_fullscreen(&mut self) {
        let Some(playing) = &self.playing else {
            return self.info("Full screen is for a video playing");
        };
        if playing.audio_only {
            self.info("Sound alone has no picture to fill the screen with");
        } else if playing.pepper.is_some() {
            self.fullscreen = !self.fullscreen;
        } else if playing.controllable() {
            playing.player.toggle_fullscreen();
        } else {
            self.info("f in mpv's window makes it full screen");
        }
    }

    /// Whether the picture playing inside Pepper fills the whole tab.
    pub fn fills_tab(&self) -> bool {
        self.fullscreen && self.plays_inline()
    }

    /// `P`: hides the picture-in-picture box, or shows it again.
    fn toggle_pip(&mut self) {
        if !self.plays_inline() {
            return self.info("The picture-in-picture box shows a video playing inside Pepper");
        }
        self.pip_hidden = !self.pip_hidden;
        self.info(if self.pip_hidden {
            "Picture-in-picture hidden: P shows it, p opens the video's page"
        } else {
            "Picture-in-picture shown"
        });
    }

    /// Whether the watch page is on the video playing, or loading to, with
    /// its picture: else Enter there plays it (sound alone turns into the
    /// picture).
    pub fn watching_what_plays(&self) -> bool {
        let Some(watch) = &self.watch else {
            return false;
        };
        let id = &watch.info.video.id;
        self.playing
            .as_ref()
            .is_some_and(|p| p.video.id == *id && !p.audio_only)
            || self
                .resolving
                .as_ref()
                .is_some_and(|r| r.video.id == *id && !r.audio_only)
    }

    /// Whether the video playing shows inside the terminal (Pepper).
    pub fn plays_inline(&self) -> bool {
        self.playing.as_ref().is_some_and(|p| p.pepper.is_some())
    }

    /// What plays after the video playing, in order, as the watch page
    /// lists it: the rest of the Jukebox (true), then up next.
    pub fn up_next_list(&self) -> Vec<(&Video, bool)> {
        let queue = self.jukebox_queue();
        let from = self.jukebox_place().map_or(0, |i| i + 1);
        let mut list: Vec<(&Video, bool)> = queue[from.min(queue.len())..]
            .iter()
            .filter(|v| !v.upcoming)
            .map(|&v| (v, true))
            .collect();
        list.extend(self.up_next.iter().map(|v| (v, false)));
        list
    }

    /// Enter in up next: that video now. The ones before it are skipped,
    /// as if N had been pressed till it came.
    fn play_up_next(&mut self, i: usize) {
        let Some((video, from_jukebox)) = self
            .up_next_list()
            .get(i)
            .map(|&(video, from_jukebox)| (video.clone(), from_jukebox))
        else {
            return;
        };
        if !from_jukebox && let Some(at) = self.up_next.iter().position(|v| v.id == video.id) {
            self.up_next.drain(..at);
        }
        self.play(video, false, false, from_jukebox);
    }

    /// The watch page's keys. False for a key it leaves to the rest: the
    /// player's keys, `:`, `?`.
    fn on_watch_key(&mut self, key: KeyEvent, ctrl: bool) -> bool {
        let watching = self.watching_what_plays();
        let Some(watch) = self.watch.as_mut() else {
            return false;
        };
        // Ctrl and an arrow or `h j k l`: the description or comments, or
        // up next, as the list's panes; never the panes behind the page.
        if let Some(way) = toward(&key) {
            match way {
                Toward::Left => watch.pane = WatchPane::Info,
                Toward::Right => watch.pane = WatchPane::UpNext,
                Toward::Up | Toward::Down => {}
            }
            return true;
        }
        // Tab goes between the description and the comments, from up next
        // too (`info_key`).
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            watch.pane = WatchPane::Info;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => {
                self.watch = None;
                return true;
            }
            KeyCode::Char('o') if ctrl => {
                self.watch = None;
                return true;
            }
            // Back to the list: q quits only from there.
            KeyCode::Char('q') if !ctrl => {
                self.watch = None;
                return true;
            }
            // What the cards' keys do to a video, to the page's.
            KeyCode::Char('w') if !ctrl => self.toggle_watch_later(),
            KeyCode::Char('e') if !ctrl => self.add_to_jukebox(false),
            KeyCode::Char('E') if !ctrl => self.add_to_jukebox(true),
            KeyCode::Char('S') if !ctrl => self.toggle_subscription(),
            KeyCode::Char('o') if !ctrl => self.open_in_browser(),
            KeyCode::Char('y') if !ctrl => self.copy_link(),
            // These show another list, in place of the page.
            KeyCode::Char('c') if !ctrl => {
                self.open_selected_channel();
                self.watch = None;
            }
            KeyCode::Char('M') if !ctrl => {
                self.open_mix();
                self.watch = None;
            }
            KeyCode::Char('/' | 's') if !ctrl => {
                self.watch = None;
                return false;
            }
            // Its sound alone, as `a` on its card.
            KeyCode::Char('a') if !ctrl => {
                let video = watch.info.video.clone();
                self.play(video, true, false, false);
            }
            KeyCode::Char('0') if !ctrl => self.back_to_start(),
            // A page opened on a video that isn't playing: Enter plays it.
            KeyCode::Enter if watch.pane == WatchPane::Info && !watching => {
                let video = watch.info.video.clone();
                self.play(video, false, false, false);
            }
            _ => match watch.pane {
                WatchPane::Info => {
                    return match info_key(&mut watch.info, key, ctrl) {
                        InfoKey::Comments => {
                            let id = watch.info.video.id.clone();
                            self.load_comments(id);
                            true
                        }
                        InfoKey::GoTo(video, at) => {
                            self.go_to_in(video, at);
                            true
                        }
                        InfoKey::Handled => true,
                        InfoKey::Ignored => false,
                    };
                }
                WatchPane::UpNext => return self.on_up_next_key(key, ctrl),
            },
        }
        true
    }

    fn on_up_next_key(&mut self, key: KeyEvent, ctrl: bool) -> bool {
        let count = self.up_next_list().len();
        let Some(watch) = self.watch.as_mut() else {
            return false;
        };
        let last = count.saturating_sub(1);
        let delta: isize = match key.code {
            KeyCode::Char('h') | KeyCode::Left => {
                watch.pane = WatchPane::Info;
                return true;
            }
            KeyCode::Enter => {
                let i = watch.next_selected;
                self.play_up_next(i);
                return true;
            }
            KeyCode::Char('j') | KeyCode::Down => 1,
            KeyCode::Char('k') | KeyCode::Up => -1,
            KeyCode::Char('d') if ctrl => HELP_PAGE,
            KeyCode::Char('u') if ctrl => -HELP_PAGE,
            KeyCode::PageDown => 2 * HELP_PAGE,
            KeyCode::PageUp => -2 * HELP_PAGE,
            KeyCode::Char('g') | KeyCode::Home => isize::MIN,
            KeyCode::Char('G') | KeyCode::End => isize::MAX,
            _ => return false,
        };
        watch.next_selected = watch.next_selected.saturating_add_signed(delta).min(last);
        true
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
            HelpMenu::LIST => self.toggle_list(),
            HelpMenu::JUKEBOX_LIST => self.toggle_jukebox_layout(),
            HelpMenu::JUKEBOX_AUTOREMOVE => self.toggle_jukebox_autoremove(),
            HelpMenu::AUTOPLAY => self.toggle_autoplay(),
            HelpMenu::SHUFFLE => self.toggle_shuffle(),
            HelpMenu::SUBTITLES => self.toggle_subtitles(),
            HelpMenu::SPONSORBLOCK => self.toggle_sponsorblock(),
            HelpMenu::DEARROW => self.toggle_dearrow(),
            HelpMenu::HISTORY => self.toggle_history(),
            HelpMenu::NOW_PLAYING => self.toggle_now_playing(),
            HelpMenu::INLINE_VIDEO => self.toggle_inline_video(),
            HelpMenu::AUDIO_ONLY => self.toggle_audio_only(),
            HelpMenu::VISUALIZER => self.toggle_visualizer(),
            HelpMenu::VISUALIZER_WHITE => self.toggle_visualizer_white(),
            row if (HelpMenu::VIDEO_QUALITY..HelpMenu::AUDIO_QUALITY).contains(&row) => {
                self.pick_height(settings::HEIGHTS[row - HelpMenu::VIDEO_QUALITY]);
            }
            row if (HelpMenu::AUDIO_QUALITY..HelpMenu::AUDIO_ONLY).contains(&row) => {
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
            KeyCode::Enter => {
                self.open_entry(self.entry(self.sidebar_selected));
                None
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.go_to_videos();
                None
            }
            KeyCode::Char('C') if self.entry(self.sidebar_selected) == Entry::Jukebox => {
                self.ask_to_clear_jukebox();
                None
            }
            KeyCode::Char('C') => {
                if let Entry::Group(i) = self.entry(self.sidebar_selected)
                    && let Some(group) = self.groups.get(i)
                {
                    self.confirm = Some(Confirm::DeleteGroup(group.id));
                }
                None
            }
            KeyCode::Char('x' | 'd') if !ctrl => {
                if let Entry::Channel(i) = self.entry(self.sidebar_selected)
                    && let Some(c) = self.subscriptions.get(i).cloned()
                {
                    self.confirm = Some(Confirm::Unsubscribe(c.id, c.title));
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
            // Left from a row's first card goes to the row before's last,
            // right from its last to the next row's first.
            KeyCode::Char('h') | KeyCode::Left => self.select(self.selected.saturating_sub(1)),
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
            KeyCode::Char('0') => self.back_to_start(),
            KeyCode::Char('M') => self.open_mix(),
            KeyCode::Char('e') => self.add_to_jukebox(false),
            KeyCode::Char('E') => self.add_to_jukebox(true),
            KeyCode::Char('J') => self.move_in_jukebox(true),
            KeyCode::Char('K') => self.move_in_jukebox(false),
            KeyCode::Char('v') => self.switch_layout(),
            KeyCode::Char('C') if self.view == View::Jukebox => self.ask_to_clear_jukebox(),
            KeyCode::Char('C') if matches!(self.view, View::MyPlaylist(..)) => {
                self.ask_to_delete_playlist();
            }
            KeyCode::Char('C') => {
                if let View::Group(id, _) = self.view {
                    self.confirm = Some(Confirm::DeleteGroup(id));
                }
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

/// Writes escape sequences straight to the terminal, after a frame (or
/// nothing, for none).
fn write_to_terminal(sequences: &str) {
    use std::io::Write;
    if sequences.is_empty() {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(sequences.as_bytes());
    let _ = out.flush();
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

/// Whether shuffle plays `view`'s videos in an order of their own: the
/// Jukebox, playlists (Watch later is one on YouTube) and Mixes.
fn shuffles(view: &View) -> bool {
    matches!(
        view,
        View::Jukebox
            | View::WatchLater
            | View::Playlist(..)
            | View::MyPlaylist(..)
            | View::Mix(..)
    )
}

/// `videos`, each once (a playlist can hold a video twice), in order.
fn once_each<'a>(videos: impl Iterator<Item = &'a Video>) -> Vec<Video> {
    let mut seen = HashSet::new();
    videos
        .filter(|v| seen.insert(v.id.clone()))
        .cloned()
        .collect()
}
/// A key in a video's info, the `i` popup's or the watch page's: Tab and
/// left/right switch between the description and the comments, `j k` and
/// the paging keys move, Enter on a description's line with a time goes
/// there.
fn info_key(info: &mut InfoPopup, key: KeyEvent, ctrl: bool) -> InfoKey {
    match key.code {
        KeyCode::Tab
        | KeyCode::BackTab
        | KeyCode::Char('h' | 'l')
        | KeyCode::Left
        | KeyCode::Right => {
            info.tab = match info.tab {
                InfoTab::Description => InfoTab::Comments,
                InfoTab::Comments => InfoTab::Description,
            };
            return if info.tab == InfoTab::Comments {
                InfoKey::Comments
            } else {
                InfoKey::Handled
            };
        }
        KeyCode::Enter if info.tab == InfoTab::Description => {
            let line = info.video.description.lines().nth(info.line);
            return match line.and_then(time_in) {
                Some(at) => InfoKey::GoTo(info.video.clone(), at),
                None => InfoKey::Handled,
            };
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
        _ => return InfoKey::Ignored,
    };
    match info.tab {
        InfoTab::Description => {
            let last = info.video.description.lines().count().saturating_sub(1);
            info.line = info.line.saturating_add_signed(delta).min(last);
        }
        // Drawing stops it at the end.
        InfoTab::Comments => info.scroll = info.scroll.saturating_add_signed(delta),
    }
    InfoKey::Handled
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
    let mut child = {
        let _starting = crate::tools::starting();
        command.spawn()?
    };
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

    /// An app as it starts, with the keys in the sidebar.
    fn started() -> App {
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

    /// An app with the keys in the videos, where most tests press them.
    pub fn app() -> App {
        let mut app = started();
        app.focus = Focus::Grid;
        app
    }

    #[tokio::test]
    async fn tuitube_starts_with_the_keys_in_the_sidebar() {
        let mut app = started();
        assert_eq!(app.focus, Focus::Sidebar);
        with_feed(&mut app, 3);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.view, View::Shorts, "j moves in the sidebar");
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
        // Left from a row's first card: the row before's last; right from
        // its last, the next row's first. Neither leaves the videos.
        app.on_key(key(KeyCode::Left));
        assert_eq!((app.selected, app.focus), (5, Focus::Grid));
        app.on_key(key(KeyCode::Char('l')));
        assert_eq!(app.selected, 6);
        app.on_key(key(KeyCode::Home));
        app.on_key(key(KeyCode::Char('h')));
        app.on_key(key(KeyCode::Up));
        assert_eq!((app.selected, app.focus), (0, Focus::Grid), "the first");
        assert!(app.prompt.is_none(), "up from the top stays too");
    }

    #[tokio::test]
    async fn ctrl_and_an_arrow_or_vim_key_go_to_the_pane_that_way() {
        let mut app = app();
        with_feed(&mut app, 10);
        app.grid.set(GridShape { cols: 3, rows: 2 });
        let ctrl = |code| KeyEvent::new(code, KeyModifiers::CONTROL);
        app.select(4);
        app.on_key(ctrl(KeyCode::Char('h')));
        assert_eq!(app.focus, Focus::Sidebar);
        assert_eq!(app.selected, 4, "the card stays selected");
        app.on_key(key(KeyCode::Left));
        app.on_key(ctrl(KeyCode::Down));
        assert_eq!(app.focus, Focus::Sidebar, "nothing that way");
        app.on_key(ctrl(KeyCode::Right));
        assert_eq!(app.focus, Focus::Grid);

        // Up to the search box; down from it, what was typed kept for when
        // it's back.
        app.on_key(ctrl(KeyCode::Char('k')));
        assert!(matches!(&app.prompt, Some(p) if p.kind == PromptKind::Search));
        for c in "rust".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(ctrl(KeyCode::Char('j')));
        assert!(app.prompt.is_none());
        assert_eq!((app.focus, app.selected), (Focus::Grid, 4));
        assert_eq!(app.search_typed.as_deref(), Some("rust"));
        app.on_key(ctrl(KeyCode::Up));
        assert_eq!(app.prompt.as_ref().map(|p| p.text.as_str()), Some("rust"));
        // Ctrl-h in it is a Backspace (some terminals send it for one).
        app.on_key(ctrl(KeyCode::Char('h')));
        assert_eq!(app.prompt.as_ref().map(|p| p.text.as_str()), Some("rus"));
        app.on_key(key(KeyCode::Char('t')));
        // Left from it: the sidebar.
        app.on_key(ctrl(KeyCode::Left));
        assert!(app.prompt.is_none());
        assert_eq!(app.focus, Focus::Sidebar);
        // `/` has it back too; Esc lets it go.
        app.on_key(key(KeyCode::Char('/')));
        assert_eq!(app.prompt.as_ref().map(|p| p.text.as_str()), Some("rust"));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.search_typed, None);

        // On Search in the sidebar, l goes to the search box while there
        // are no results...
        app.show(View::Search(String::new()), false);
        app.sync_sidebar();
        assert_eq!(app.entry(app.sidebar_selected), Entry::Search);
        app.on_key(key(KeyCode::Char('l')));
        assert!(app.prompt.is_some());
        app.on_key(key(KeyCode::Esc));
        // ...and into them once there are.
        app.search_results = (0..3)
            .map(|i| video(&format!("found{i:06}"), CH, None))
            .collect();
        app.show(View::Search("rust".into()), false);
        app.sync_sidebar();
        app.focus = Focus::Sidebar;
        app.on_key(key(KeyCode::Right));
        assert!(app.prompt.is_none());
        assert_eq!(app.focus, Focus::Grid);
        assert_eq!(app.videos.len(), 3);
        // Enter there always goes to the search box.
        app.on_key(ctrl(KeyCode::Char('h')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.prompt.as_ref().map(|p| p.text.as_str()), Some("rust"));
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
        assert_eq!(app.subscriptions.len(), 1, "asked first");
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.subscriptions.is_empty(), "unsubscribed");
        app.on_key(key(KeyCode::Char('u')));
        assert_eq!(app.subscriptions.len(), 1, "undone");
        // S on one of its videos asks too.
        app.focus = Focus::Grid;
        app.on_key(key(KeyCode::Char('S')));
        assert!(matches!(app.confirm, Some(Confirm::Unsubscribe(..))));
        app.on_key(key(KeyCode::Char('n')));
        assert_eq!(app.subscriptions.len(), 1, "kept");
        app.on_key(key(KeyCode::Char('S')));
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.subscriptions.is_empty());
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
            app.store.add_to_watch_later(&v.id, 1).unwrap();
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
        // Taking it off asks first: any key but y keeps it, Enter too.
        app.on_key(key(KeyCode::Char('x')));
        assert!(matches!(
            app.confirm,
            Some(Confirm::TakeOff(Kept::WatchLater, ref id, _)) if *id == saved
        ));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.confirm, None);
        assert_eq!(app.videos.len(), 1, "kept");
        assert!(app.watch_later.contains(&saved));
        // w on it asks too.
        app.on_key(key(KeyCode::Char('w')));
        assert!(app.confirm.is_some());
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.videos.is_empty());
        assert!(!app.watch_later.contains(&saved));
        assert!(app.store.watch_later().unwrap().is_empty());
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
        assert_eq!(app.store.history(10).unwrap().len(), 1, "asked first");
        app.on_key(key(KeyCode::Char('y')));
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

    /// The first video's sound playing alone, in a pretend mpv that has
    /// started it, its visualizer's filter in.
    pub fn listening(app: &mut App) {
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, true);
        app.playing.as_mut().unwrap().player = Player::pretend(app.plays);
        mpv_says(
            app,
            [
                PlayerEventKind::Restarted,
                PlayerEventKind::Visualizer(true),
            ],
        );
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

        app.on_key(key(KeyCode::Char('m')));
        assert!(app.muted);
        assert!(app.status.as_ref().unwrap().text.starts_with("Muted"));
        // Changing the volume turns the sound on again, as on YouTube.
        app.on_key(key(KeyCode::Char('-')));
        assert!(!app.muted);
        app.on_key(key(KeyCode::Char('m')));
        app.on_key(key(KeyCode::Char('m')));
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
        app.settings.now_playing = true;
        // Written by hand, or by another tuitube, after this one started.
        std::fs::write(
            &path,
            "history = false\nsponsorblock = false\ndearrow = false\nnow_playing = false\n",
        )
        .unwrap();
        app.on_key(key(KeyCode::Char('-')));
        app.save_volume();
        assert!(!app.settings.history && !app.settings.sponsorblock && !app.settings.dearrow);
        assert!(!app.settings.now_playing);
        let on_disk = Settings::load(&path).unwrap();
        assert_eq!(on_disk.volume(), 95);
        assert!(!on_disk.dearrow, "never turned back on");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn now_playing_turns_off_and_on_from_the_settings_tab_and_a_command() {
        let dir = std::env::temp_dir().join(format!("tuitube-now-playing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = settings::path(&dir);
        let mut app = app();
        app.settings_path = Some(path.clone());
        assert!(app.settings.now_playing, "on unless turned off");
        with_feed(&mut app, 1);
        app.show_playing(app.videos[0].clone(), 0.0, 600.0, false);

        change_setting(&mut app, HelpMenu::NOW_PLAYING);
        assert!(!app.settings.now_playing);
        assert!(!Settings::load(&path).unwrap().now_playing, "saved");
        let said = app.status.as_ref().unwrap().text.clone();
        assert!(said.starts_with("Now Playing"), "{said}");

        app.set_toggle(crate::commands::Toggle::NowPlaying, Some(true));
        assert!(app.settings.now_playing);
        assert!(Settings::load(&path).unwrap().now_playing);
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
        app.store.add_to_watch_later(&blocked.id, 1).unwrap();
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

    #[tokio::test]
    async fn now_playings_picture_goes_to_mpv_once_it_plays() {
        let mut app = app();
        with_feed(&mut app, 2);
        let listed: Vec<Video> = app.videos.clone();
        let path = std::env::temp_dir().join("p-vid00000000.jpg");
        let cover = |app: &App| {
            let playing = app.playing.as_ref().unwrap();
            (playing.cover.is_some(), playing.cover_sent)
        };
        app.show_playing(listed[0].clone(), 0.0, 600.0, false);
        let play = app.playing.as_ref().unwrap().player.play;

        // Another playback's picture is dropped.
        app.on_event(AppEvent::Cover {
            play: play + 1,
            path: path.clone(),
        });
        assert_eq!(cover(&app), (false, false));
        // Made before mpv plays: kept until it does, then given once.
        app.on_event(AppEvent::Cover {
            play,
            path: path.clone(),
        });
        assert_eq!(cover(&app), (true, false));
        mpv_says(&mut app, [PlayerEventKind::Restarted]);
        assert_eq!(cover(&app), (true, true));

        // Made after mpv plays: given at once.
        app.show_playing(listed[1].clone(), 0.0, 600.0, false);
        let play = app.playing.as_ref().unwrap().player.play;
        mpv_says(&mut app, [PlayerEventKind::Restarted]);
        assert_eq!(cover(&app), (false, false));
        app.on_event(AppEvent::Cover { play, path });
        assert_eq!(cover(&app), (true, true));
    }

    #[tokio::test]
    async fn now_playing_on_linux_shows_what_plays_and_does_what_its_asked() {
        let mut app = app();
        with_feed(&mut app, 3);
        let listed: Vec<Video> = app.videos.clone();
        let at = |app: &App| app.playing.as_ref().unwrap().position;
        let ask = |app: &mut App, ask: Ask| app.on_event(AppEvent::Mpris(ask));
        assert_eq!(app.now_shown().track, None, "nothing plays");
        ask(&mut app, Ask::Next);
        assert!(app.playing.is_none(), "nothing to do");

        app.show_playing(listed[0].clone(), 30.0, 600.0, false);
        let play = app.playing.as_ref().unwrap().player.play;
        let path = std::env::temp_dir().join(format!("p-{}.jpg", listed[0].id));
        app.on_event(AppEvent::Cover {
            play,
            path: path.clone(),
        });
        let shown = app.now_shown();
        let track = shown.track.as_ref().unwrap();
        assert_eq!(track.play, play);
        assert_eq!(track.title, listed[0].title);
        assert_eq!(track.channel, listed[0].channel);
        assert_eq!(track.url, listed[0].id.url());
        assert_eq!(track.length, Some(600.0));
        let art = reqwest::Url::from_file_path(&path).unwrap();
        assert_eq!(track.art.as_deref(), Some(art.as_str()));
        assert_eq!(
            (shown.position, shown.paused, shown.can_seek),
            (30.0, false, true)
        );

        // A position of this playback, inside it; nothing else.
        ask(
            &mut app,
            Ask::SetPosition {
                play: play + 1,
                at: 100.0,
            },
        );
        assert_eq!(at(&app), 30.0, "another playback's");
        ask(&mut app, Ask::SetPosition { play, at: 100.0 });
        assert_eq!(at(&app), 100.0);
        ask(&mut app, Ask::SetPosition { play, at: 9000.0 });
        assert_eq!(at(&app), 100.0, "past the end");
        ask(&mut app, Ask::Seek(-500.0));
        assert_eq!(at(&app), 0.0, "before the start is the start");
        ask(&mut app, Ask::Seek(f64::NAN));
        assert_eq!(at(&app), 0.0);
        ask(&mut app, Ask::Volume(0.42));
        assert_eq!(app.settings.volume(), 42);
        assert_eq!(app.now_shown().volume, 0.42);

        // ⏭ as N; stop as X.
        app.up_next = listed[1..].iter().cloned().collect();
        ask(&mut app, Ask::Next);
        assert_eq!(app.up_next.len(), 1);
        app.show_playing(listed[0].clone(), 30.0, 600.0, false);
        ask(&mut app, Ask::Stop);
        assert!(app.playing.is_none() && app.up_next.is_empty());
    }

    #[tokio::test]
    async fn the_systems_next_and_previous_move_as_n_and_brackets() {
        let mut app = app();
        with_feed(&mut app, 3);
        let listed: Vec<Video> = app.videos.clone();
        let at = |app: &App| app.playing.as_ref().unwrap().position;
        let next = || PlayerEventKind::Remote(Remote::Next);
        let previous = || PlayerEventKind::Remote(Remote::Previous);

        // ⏭ takes what's up next, as N.
        app.show_playing(listed[0].clone(), 100.0, 600.0, false);
        app.up_next = listed[1..].iter().cloned().collect();
        mpv_says(&mut app, [next()]);
        assert_eq!(app.up_next.len(), 1);

        // Nothing up next: ⏭ goes to the next chapter, ⏮ as `[`.
        app.show_playing(listed[0].clone(), 75.0, 600.0, false);
        app.up_next.clear();
        app.playing.as_mut().unwrap().chapters = vec![
            Chapter {
                start: 0.0,
                title: "Intro".into(),
            },
            Chapter {
                start: 60.0,
                title: "Middle".into(),
            },
            Chapter {
                start: 180.0,
                title: "Outro".into(),
            },
        ];
        mpv_says(&mut app, [next()]);
        assert_eq!(at(&app), 180.0);
        mpv_says(&mut app, [PlayerEventKind::Position(200.0)]);
        mpv_says(&mut app, [previous()]);
        assert_eq!(at(&app), 180.0, "well into it: its start");
        mpv_says(&mut app, [previous()]);
        assert_eq!(at(&app), 60.0, "just started: the one before");

        // No chapters: ⏮ from the start; ⏭ says there's nothing next.
        app.playing.as_mut().unwrap().chapters.clear();
        mpv_says(&mut app, [PlayerEventKind::Position(300.0), previous()]);
        assert_eq!(at(&app), 0.0);
        mpv_says(&mut app, [next()]);
        assert_eq!(app.status.as_ref().unwrap().text, "Nothing up next");
        assert_eq!(at(&app), 0.0, "still playing, where it was");
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

    #[tokio::test]
    async fn pepper_is_told_where_the_picture_goes_and_when_it_goes() {
        use ratatui::layout::Rect;
        let mut app = app();
        with_feed(&mut app, 1);
        let area = Rect::new(2, 3, 40, 11);
        // Not inside Pepper: never a word, whatever's laid out.
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, false);
        app.video_box.set(Some(area));
        assert_eq!(app.video_placement(), "");

        app.playing.as_mut().unwrap().pepper = Some("tok".into());
        assert_eq!(app.video_placement(), pepper::place("tok", area));
        assert_eq!(app.video_placement(), "", "said once");
        // A popup would be under the picture: it goes, and comes back.
        app.help = Some(HelpMenu::new(HelpTab::Keys));
        assert_eq!(app.video_placement(), pepper::remove("tok"));
        app.help = None;
        assert_eq!(app.video_placement(), pepper::place("tok", area));
        // Moved (the page closed, a resize): placed again where it is.
        let corner = Rect::new(100, 30, 30, 8);
        app.video_box.set(Some(corner));
        assert_eq!(app.video_placement(), pepper::place("tok", corner));
        // Stopped: gone.
        app.stop_playing();
        assert_eq!(app.video_placement(), pepper::remove("tok"));
        assert_eq!(app.video_placement(), "", "said once");
    }

    #[tokio::test]
    async fn the_picture_plays_in_pepper_only_when_it_can() {
        let mut app = app();
        let helper = PathBuf::from("/Applications/Pepper.app/Contents/MacOS/pepper-play");
        // Not in Pepper, or no pepper-play: mpv's window.
        assert_eq!(app.inline_player(false), None);
        app.pepper = Some("io.github.erictran308.pepper.1".into());
        assert_eq!(app.inline_player(false), None);
        app.tools.pepper_play = Some(helper.clone());
        assert_eq!(
            app.inline_player(false),
            Some((helper, "io.github.erictran308.pepper.1".into()))
        );
        // Sound alone has no picture; and the setting turns it off.
        assert_eq!(app.inline_player(true), None);
        app.settings.inline_video = false;
        assert_eq!(app.inline_player(false), None);
    }

    #[tokio::test]
    async fn enter_opens_the_videos_page_and_esc_goes_back() {
        let mut app = app();
        with_feed(&mut app, 3);
        let first = app.videos[0].id.clone();
        app.on_key(key(KeyCode::Enter));
        let watch = app.watch.as_ref().expect("its page");
        assert_eq!(watch.info.video.id, first);
        assert_eq!(app.up_next_list().len(), 2, "the cards after it");
        // Actions act on the page's video.
        assert_eq!(app.selected_video().map(|v| &v.id), Some(&first));
        let ctrl = |code| KeyEvent::new(code, KeyModifiers::CONTROL);
        let keys_at = |app: &App| {
            let watch = app.watch.as_ref().unwrap();
            (watch.pane, watch.info.tab)
        };
        // Tab, and ← → or h l: the comments, and back.
        app.on_key(key(KeyCode::Tab));
        assert_eq!(keys_at(&app), (WatchPane::Info, InfoTab::Comments));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(keys_at(&app), (WatchPane::Info, InfoTab::Description));
        app.on_key(key(KeyCode::Char('l')));
        assert_eq!(keys_at(&app), (WatchPane::Info, InfoTab::Comments));
        app.on_key(key(KeyCode::Left));
        assert_eq!(keys_at(&app), (WatchPane::Info, InfoTab::Description));
        // Ctrl and an arrow or h j k l: up next, as the list's panes.
        app.on_key(ctrl(KeyCode::Char('l')));
        assert_eq!(keys_at(&app).0, WatchPane::UpNext);
        // j k move in up next, not the cards behind the page.
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.watch.as_ref().unwrap().next_selected, 1);
        assert_eq!(app.selected, 0);
        app.on_key(ctrl(KeyCode::Left));
        assert_eq!(keys_at(&app), (WatchPane::Info, InfoTab::Description));
        // Never to the panes behind the page.
        let focus = app.focus;
        for code in [KeyCode::Up, KeyCode::Char('j'), KeyCode::Char('h')] {
            app.on_key(ctrl(code));
        }
        assert_eq!(app.focus, focus);
        assert!(app.prompt.is_none());
        assert_eq!(keys_at(&app), (WatchPane::Info, InfoTab::Description));
        // Tab in up next: the comments.
        app.on_key(ctrl(KeyCode::Right));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(keys_at(&app), (WatchPane::Info, InfoTab::Comments));
        app.on_key(key(KeyCode::Esc));
        assert!(app.watch.is_none());
        // p: the page again (of what's selected, as nothing plays here).
        app.on_key(key(KeyCode::Char('p')));
        assert!(app.watch.is_some());
        app.on_key(key(KeyCode::Char('p')));
        assert!(app.watch.is_none());
        // q on the page goes back to the list; there, it quits.
        app.on_key(key(KeyCode::Char('p')));
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.watch.is_none());
        assert!(!app.quit);
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[tokio::test]
    async fn enter_in_up_next_plays_that_video_and_skips_the_ones_before() {
        let (mut app, dir) = app_that_plays("up-next");
        with_feed(&mut app, 4);
        let ids: Vec<VideoId> = app.videos.iter().map(|v| v.id.clone()).collect();
        app.on_key(key(KeyCode::Enter));
        app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.resolving.as_ref().map(|r| &r.video.id), Some(&ids[2]));
        let next: Vec<&VideoId> = app.up_next.iter().map(|v| &v.id).collect();
        assert_eq!(next, [&ids[3]]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn x_stops_and_goes_back_to_the_list() {
        let (mut app, dir) = app_that_plays("stop-page");
        with_feed(&mut app, 3);
        app.on_key(key(KeyCode::Enter));
        assert!(app.resolving.is_some() && app.watch.is_some());
        app.on_key(key(KeyCode::Char('X')));
        assert!(app.resolving.is_none());
        assert!(app.watch.is_none(), "the list again");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn audio_only_makes_enter_listen_wherever_a_video_starts() {
        let (mut app, dir) = app_that_plays("audio-only");
        with_feed(&mut app, 3);
        let listens = |app: &App| app.resolving.as_ref().map(|r| r.audio_only);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(listens(&app), Some(false), "the picture, at first");
        assert!(app.watch.is_some(), "on its page");
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(listens(&app), Some(true));

        change_setting(&mut app, HelpMenu::AUDIO_ONLY);
        assert!(app.settings.audio_only);
        assert!(app.status.as_ref().unwrap().text.contains("Enter listens"));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(listens(&app), Some(true), "Enter is a");
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(listens(&app), Some(true));
        // A time in a description, autoplay and N go through the same.
        let mut v = app.videos[1].clone();
        v.description = "0:30 The song".into();
        app.store.save_videos(std::slice::from_ref(&v)).unwrap();
        app.reload();
        app.select(1);
        app.on_key(key(KeyCode::Char('i')));
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(listens(&app), Some(true));
        app.run_command(Command::Play);
        assert_eq!(listens(&app), Some(true));

        app.run_command(Command::Set(Toggle::AudioOnly, Some(false)));
        assert!(!app.settings.audio_only);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(listens(&app), Some(false), "the picture again");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn audio_only_turned_on_while_the_picture_plays_says_so() {
        let mut app = app();
        with_feed(&mut app, 2);
        // Where mpv can't say where it is (or in the demo), it isn't
        // played again: the next video listens.
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, false);
        app.run_command(Command::Set(Toggle::AudioOnly, None));
        assert!(app.settings.audio_only);
        assert!(app.resolving.is_none());
        assert!(app.playing.as_ref().is_some_and(|p| !p.audio_only));
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .text
                .starts_with("Audio only on")
        );
    }

    #[tokio::test]
    async fn the_visualizers_filter_goes_in_once_mpv_plays_and_out_when_off() {
        let mut app = app();
        with_feed(&mut app, 2);
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, true);
        app.playing.as_mut().unwrap().player = Player::pretend(app.plays);
        let filter = |app: &App| app.playing.as_ref().unwrap().filter;
        assert!(app.visualizing(), "on by default, for sound alone");
        mpv_says(&mut app, [PlayerEventKind::Paused(false)]);
        assert_eq!(filter(&app), Filter::Out, "not before the sound plays");
        mpv_says(&mut app, [PlayerEventKind::Restarted]);
        assert_eq!(filter(&app), Filter::Asked);
        mpv_says(&mut app, [PlayerEventKind::Restarted]);
        assert_eq!(filter(&app), Filter::Asked, "asked once");
        mpv_says(&mut app, [PlayerEventKind::Visualizer(true)]);
        assert_eq!(filter(&app), Filter::In);
        mpv_says(
            &mut app,
            [PlayerEventKind::Spectrum([-20.0; visualizer::BANDS])],
        );

        change_setting(&mut app, HelpMenu::VISUALIZER);
        assert!(!app.settings.visualizer && !app.visualizing());
        assert_eq!(filter(&app), Filter::Out, "taken out");
        app.run_command(Command::Set(Toggle::Visualizer, Some(true)));
        assert_eq!(filter(&app), Filter::Asked, "in again, as it plays");
        // An mpv whose FFmpeg can't make it: said once, not asked again.
        mpv_says(&mut app, [PlayerEventKind::Visualizer(false)]);
        assert_eq!(filter(&app), Filter::Out);
        assert!(!app.visualizing());
        assert!(app.status.as_ref().unwrap().error);
        app.show_playing(app.videos[1].clone(), 0.0, 100.0, true);
        app.playing.as_mut().unwrap().player = Player::pretend(app.plays);
        mpv_says(&mut app, [PlayerEventKind::Restarted]);
        assert_eq!(filter(&app), Filter::Out);
    }

    #[tokio::test]
    async fn the_visualizer_shows_only_while_the_sound_alone_plays_where_mpv_answers() {
        let mut app = app();
        with_feed(&mut app, 2);
        listening(&mut app);
        assert!(app.visualizing());
        mpv_control(&mut app, false);
        assert!(!app.visualizing(), "Windows: mpv can't be asked");
        listening(&mut app);
        app.show_playing(app.videos[0].clone(), 10.0, 100.0, false);
        app.playing.as_mut().unwrap().player = Player::pretend(app.plays);
        mpv_says(&mut app, [PlayerEventKind::Restarted]);
        assert!(!app.visualizing(), "a video shows its picture");
        assert_eq!(app.playing.as_ref().unwrap().filter, Filter::Out);
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

        // x, then y, takes one off; C, then y, deletes the playlist.
        app.focus = Focus::Grid;
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.videos.len(), 2, "asked first");
        app.on_key(key(KeyCode::Char('y')));
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
        // Outside the Jukebox, J and K do nothing.
        app.on_key(key(KeyCode::Char('J')));
        assert_eq!(jukebox(&app).len(), 3);

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
        assert_eq!(jukebox(&app).len(), 3, "asked first");
        app.on_key(key(KeyCode::Char('y')));
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

    #[tokio::test]
    async fn v_shows_every_view_as_a_list_or_just_the_jukebox() {
        let mut app = app();
        with_feed(&mut app, 3);
        assert!(
            !app.settings.list && !app.shows_list(),
            "cards unless asked"
        );
        app.on_key(key(KeyCode::Char('v')));
        assert!(app.settings.list && app.shows_list());
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .text
                .contains("Every view: a list")
        );
        app.show(View::Jukebox, false);
        assert!(app.shows_list(), "the Jukebox too");
        // In the Jukebox, every view back to cards, the Jukebox's own list
        // setting too: v always turns the view you're in.
        app.settings.jukebox_list = true;
        app.on_key(key(KeyCode::Char('v')));
        assert!(!app.settings.list && !app.settings.jukebox_list);
        assert!(!app.shows_list());
        // Then only the Jukebox.
        app.on_key(key(KeyCode::Char('v')));
        assert!(app.settings.jukebox_list && !app.settings.list);
        assert!(app.shows_list());
        app.show(View::Home, false);
        assert!(!app.shows_list(), "the rest stay cards");
        // The settings tab and the command line.
        change_setting(&mut app, HelpMenu::LIST);
        assert!(app.settings.list);
        app.run_command(Command::Set(Toggle::List, Some(false)));
        assert!(!app.settings.list);
    }

    #[tokio::test]
    async fn r_repeats_the_list_then_one_video_then_neither() {
        let (mut app, dir) = app_that_plays("repeat");
        with_feed(&mut app, 3);
        // Home lists the newest first: vid00000002 … vid00000000.
        let listed = app.videos.clone();
        let said = |app: &App| app.status.as_ref().unwrap().text.clone();
        assert_eq!(app.settings.repeat, Repeat::Off);
        app.select(2);
        app.on_key(key(KeyCode::Enter));
        // Back from its page to the cards, which the rest picks from.
        app.on_key(key(KeyCode::Esc));
        app.resolving = None;
        ended(&mut app, &listed[2]);
        assert_eq!(loading(&app), None, "after the last card, nothing");

        // r: repeat all. After the last card, the first again, then the rest.
        app.on_key(key(KeyCode::Char('r')));
        assert_eq!(app.settings.repeat, Repeat::All);
        assert!(said(&app).starts_with("Repeat all"));
        app.show_playing(listed[2].clone(), 10.0, 600.0, false);
        assert_eq!(app.plays_next().unwrap().id, listed[0].id, "Next: says so");
        ended(&mut app, &listed[2]);
        assert_eq!(loading(&app), Some("vid00000002"));
        assert_eq!(ids(&app.up_next), ["vid00000001", "vid00000000"]);
        // Only with autoplay, as the cards after it; N plays it all the same.
        app.resolving = None;
        app.up_next.clear();
        app.settings.autoplay = false;
        ended(&mut app, &listed[2]);
        assert_eq!(loading(&app), None);
        app.show_playing(listed[2].clone(), 10.0, 600.0, false);
        app.resolving = Some(Resolving {
            request: app.resolves,
            video: listed[2].clone(),
            audio_only: false,
            auto: false,
            from_jukebox: false,
            carry_on: false,
        });
        app.on_key(key(KeyCode::Char('N')));
        assert_eq!(loading(&app), Some("vid00000002"));
        app.settings.autoplay = true;

        // The Jukebox, while it's what plays: from its first after its last,
        // not the list up next.
        app.resolving = None;
        for n in [0, 1] {
            app.select(n);
            app.on_key(key(KeyCode::Char('e')));
        }
        ended(&mut app, &listed[1]);
        assert_eq!(loading(&app), Some("vid00000002"));
        assert!(!app.up_next.is_empty(), "up next waits");

        // r again: repeat one. mpv plays the video over and over: nothing
        // plays after it (N still does), and what SponsorBlock marked is
        // skipped each time round.
        app.resolving = None;
        app.on_key(key(KeyCode::Char('r')));
        assert_eq!(app.settings.repeat, Repeat::One);
        assert!(said(&app).starts_with("Repeat one"), "{}", said(&app));
        app.settings.sponsorblock = true;
        app.show_playing(listed[2].clone(), 0.0, 600.0, false);
        assert!(app.plays_next().is_none());
        assert!(app.next_video().is_some());
        sponsor_at(&mut app, 10.0, 40.0, 12.0);
        assert_eq!(shown_skip(&app), Some(("sponsor", 28.0)));
        let round_again = |app: &mut App| {
            let playing = app.playing.as_mut().unwrap();
            playing.skip = None;
            playing.lap = Instant::now() - MIN_LAP;
            mpv_says(app, [PlayerEventKind::Position(599.5)]);
            mpv_says(app, [PlayerEventKind::Position(0.0)]);
            mpv_says(app, [PlayerEventKind::Position(12.0)]);
            shown_skip(app)
        };
        assert_eq!(round_again(&mut app), Some(("sponsor", 28.0)));
        // Played once, not repeated: skipped only the first time.
        app.settings.repeat = Repeat::Off;
        assert_eq!(round_again(&mut app), None);
        app.settings.repeat = Repeat::One;
        assert_eq!(round_again(&mut app), Some(("sponsor", 28.0)));
        // A seek back from midway isn't a lap: nothing is skipped again.
        let playing = app.playing.as_mut().unwrap();
        playing.skip = None;
        playing.lap = Instant::now() - MIN_LAP;
        for at in [300.0, 0.0, 12.0] {
            mpv_says(&mut app, [PlayerEventKind::Position(at)]);
        }
        assert_eq!(shown_skip(&app), None);
        // A segment over the whole video: skipped to the end once, then the
        // next lap plays, not a skip to the end over and over.
        app.show_playing(listed[2].clone(), 0.0, 600.0, false);
        sponsor_at(&mut app, 0.0, 600.0, 0.0);
        assert_eq!(shown_skip(&app), Some(("sponsor", 600.0)));
        app.playing.as_mut().unwrap().skip = None;
        for at in [0.0, 12.0] {
            mpv_says(&mut app, [PlayerEventKind::Position(at)]);
        }
        assert_eq!(shown_skip(&app), None);

        // On Windows, mpv can't be told: from the next video.
        mpv_control(&mut app, false);
        app.on_key(key(KeyCode::Char('r')));
        assert_eq!(app.settings.repeat, Repeat::Off);
        assert_eq!(said(&app), "Repeat off, from the next video");
        mpv_control(&mut app, true);

        // The command line, and the settings file.
        let file = crate::settings::path(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        app.settings_path = Some(file.clone());
        let run = |app: &mut App, line: &str| {
            typed(app, line);
            app.on_key(key(KeyCode::Enter));
        };
        run(&mut app, ":repeat one");
        assert_eq!(app.settings.repeat, Repeat::One);
        assert_eq!(Settings::load(&file).unwrap().repeat, Repeat::One);
        run(&mut app, ":set norepeat");
        assert_eq!(app.settings.repeat, Repeat::Off);
        run(&mut app, ":repeat");
        assert_eq!(app.settings.repeat, Repeat::All);
        assert_eq!(Settings::load(&file).unwrap().repeat, Repeat::All);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn ended(app: &mut App, video: &Video) {
        app.show_playing(video.clone(), 599.0, 600.0, false);
        mpv_says(app, [PlayerEventKind::Ended, PlayerEventKind::Exited]);
    }

    #[tokio::test]
    async fn a_video_that_cant_be_played_is_marked_and_selected_to_try_again() {
        let (mut app, dir) = app_that_plays("failed");
        with_feed(&mut app, 4);
        app.focus = Focus::Grid;
        app.settings.autoplay = true;
        // Home lists the newest first: vid00000003 … vid00000000.
        let listed = app.videos.clone();
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(loading(&app), Some("vid00000003"));
        app.resolving = None;
        // Browsing on while it plays. It ends, and the next can't be played:
        // marked and selected, and autoplay goes on to the one after it.
        app.select(3);
        ended(&mut app, &listed[0]);
        assert_eq!(loading(&app), Some("vid00000002"));
        resolved(&mut app, Err(anyhow::anyhow!("Video unavailable")));
        assert_eq!(loading(&app), Some("vid00000001"));
        assert_eq!(app.selected, 1);
        assert!(app.failed_selected().unwrap().contains("unavailable"));
        // Not said where Enter would do something else.
        app.focus = Focus::Sidebar;
        assert_eq!(app.failed_selected(), None);
        app.focus = Focus::Grid;

        // N while one plays: the one it couldn't load is selected, and the
        // one playing plays on.
        app.resolving = None;
        app.show_playing(listed[2].clone(), 10.0, 600.0, true);
        app.on_key(key(KeyCode::Char('N')));
        assert_eq!(loading(&app), Some("vid00000000"));
        resolved(&mut app, Err(anyhow::anyhow!("Video unavailable")));
        assert_eq!(app.selected, 3);
        assert!(app.playing.is_some());

        // a tries it again: loading, no longer marked.
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(loading(&app), Some("vid00000000"));
        assert!(!app.failed.contains_key(&listed[3].id));
        assert!(app.failed.contains_key(&listed[1].id));

        // Another quality of the video playing failed: it plays on,
        // unmarked.
        app.resolving = None;
        app.show_playing(listed[3].clone(), 10.0, 600.0, false);
        app.play(listed[3].clone(), true, false, false);
        resolved(&mut app, Err(anyhow::anyhow!("Video unavailable")));
        assert!(!app.failed.contains_key(&listed[3].id));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn f_puts_a_channel_in_groups_whose_rows_show_their_videos() {
        let mut app = app();
        with_feed(&mut app, 3);
        let first_channel = app.menu().len();
        let type_in = |app: &mut App, text: &str| {
            for c in text.chars() {
                app.on_key(key(KeyCode::Char(c)));
            }
        };
        // F on a subscription in the sidebar: no group yet, so it names one.
        app.focus = Focus::Sidebar;
        app.sidebar_selected = first_channel;
        app.on_key(key(KeyCode::Char('F')));
        assert!(app.group_menu.as_ref().unwrap().naming.is_some());
        type_in(&mut app, "Rust");
        app.on_key(key(KeyCode::Enter));
        let id = app.groups[0].id;
        assert_eq!(
            (app.groups[0].name.as_str(), app.groups[0].count),
            ("Rust", 1)
        );
        let menu = app.group_menu.as_ref().unwrap();
        assert!(menu.inside.contains(&id) && menu.naming.is_none());
        // Its row comes under the menu; the channel stays selected.
        assert_eq!(app.entry(first_channel), Entry::Group(0));
        assert_eq!(app.entry(app.sidebar_selected), Entry::Channel(0));

        // Enter takes it out, Space puts it back in.
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.groups[0].count, 0);
        app.on_key(key(KeyCode::Char(' ')));
        assert_eq!(app.groups[0].count, 1);
        // r renames it; a paste goes in the name.
        app.on_key(key(KeyCode::Char('r')));
        for _ in 0..4 {
            app.on_key(key(KeyCode::Backspace));
        }
        app.on_terminal(Event::Paste("Code\u{1b}[2J".into()));
        app.on_key(key(KeyCode::Enter));
        assert!(app.groups[0].name.starts_with("Code"));
        assert!(!app.groups[0].name.contains('\u{1b}'), "cleaned");
        app.on_key(key(KeyCode::Char('r')));
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_in(&mut app, "Code");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.groups[0].name, "Code");
        app.on_key(key(KeyCode::Esc));
        assert!(app.group_menu.is_none());

        // Its row shows its channels' videos, as Home does.
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.view, View::Group(id, "Code".into()));
        assert_eq!(app.videos.len(), 3);
        // From a card too; with none of your subscriptions, it says so.
        app.focus = Focus::Grid;
        app.on_key(key(KeyCode::Char('F')));
        assert_eq!(app.group_menu.as_ref().unwrap().name, "BekBrace");
        app.on_key(key(KeyCode::Char('q')));
        app.videos
            .push(video("vid00000099", "UCxxxxxxxxxxxxxxxxxxxxxx", Some(1)));
        app.select(3);
        app.on_key(key(KeyCode::Char('F')));
        assert!(app.group_menu.is_none());
        assert!(app.status.as_ref().unwrap().text.contains("S subscribes"));

        // C deletes it, after a y: the channel stays a subscription.
        app.reload();
        app.on_key(key(KeyCode::Char('C')));
        assert_eq!(app.confirm, Some(Confirm::DeleteGroup(id)));
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.groups.is_empty());
        assert_eq!(app.view, View::Home);
        assert_eq!(app.subscriptions.len(), 1);
        assert_eq!(app.entry(app.sidebar_selected), Entry::Home);
    }

    #[tokio::test]
    async fn a_refused_stream_is_asked_for_again_once() {
        let (mut app, dir) = app_that_plays("refused");
        with_feed(&mut app, 2);
        let video = app.videos[0].clone();
        // YouTube refused the first addresses: new ones, at once.
        app.show_playing(video.clone(), 0.0, 600.0, false);
        mpv_says(&mut app, [PlayerEventKind::Failed, PlayerEventKind::Exited]);
        assert_eq!(app.resolving.as_ref().map(|r| &r.video.id), Some(&video.id));
        assert!(app.status.as_ref().unwrap().text.contains("asking again"));
        // Refused again: said, not asked a third time.
        app.resolving = None;
        app.show_playing(video.clone(), 0.0, 600.0, false);
        mpv_says(&mut app, [PlayerEventKind::Failed, PlayerEventKind::Exited]);
        assert!(app.resolving.is_none());
        let said = app.status.as_ref().unwrap();
        assert!(
            said.error && said.text.contains("YouTube refused its stream"),
            "{}",
            said.text
        );
        assert!(app.failed.contains_key(&video.id), "marked");
        // Once it has played, a failure (the addresses expired) isn't retried.
        app.show_playing(video.clone(), 0.0, 600.0, false);
        mpv_says(
            &mut app,
            [PlayerEventKind::Restarted, PlayerEventKind::Failed],
        );
        assert!(app.resolving.is_none());
        let _ = std::fs::remove_dir_all(dir);
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
        app.on_key(key(KeyCode::Char('y')));
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

    /// The ids of `videos`, sorted: what a list holds, whatever its order.
    fn sorted<'a>(videos: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        let mut ids: Vec<&str> = videos.into_iter().collect();
        ids.sort_unstable();
        ids
    }

    #[tokio::test]
    async fn shuffle_plays_each_video_of_a_list_once_in_an_order_of_its_own() {
        let mut app = app();
        app.rng = Rng::seeded(1);
        with_feed(&mut app, 12);
        for i in 0..12 {
            app.select(i);
            app.on_key(key(KeyCode::Char('w')));
        }
        app.show(View::WatchLater, false);
        let listed: Vec<String> = app.videos.iter().map(|v| v.id.as_str().into()).collect();
        assert_eq!(listed.len(), 12);
        let some =
            |from: usize| -> Vec<&str> { listed[from..].iter().map(String::as_str).collect() };

        // Off: the cards after it, in order.
        app.select(3);
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(ids(&app.up_next), some(4));
        // z: what's left, shuffled: the same videos, each once.
        app.on_key(key(KeyCode::Char('z')));
        assert!(app.settings.shuffle);
        assert_ne!(ids(&app.up_next), some(4), "shuffled");
        assert_eq!(sorted(ids(&app.up_next)), sorted(some(4)));
        // z again: in the list's order, still only what's left.
        app.on_key(key(KeyCode::Char('z')));
        assert_eq!(ids(&app.up_next), some(4));

        // On, a video played from the list: every other one after it, each
        // once, not in the list's order.
        app.on_key(key(KeyCode::Char('z')));
        app.select(0);
        app.on_key(key(KeyCode::Char('a')));
        assert_ne!(ids(&app.up_next), some(1));
        assert_eq!(sorted(ids(&app.up_next)), sorted(some(1)));

        // Repeat all: after the round, a new one, not starting with the
        // video that ended; the one shown next is the one that plays.
        app.settings.repeat = Repeat::All;
        app.up_next.clear();
        let last = app.videos[7].clone();
        app.show_playing(last.clone(), 599.0, 600.0, true);
        let shown = app.plays_next().unwrap().id.clone();
        assert_ne!(shown, last.id);
        mpv_says(&mut app, [PlayerEventKind::Ended, PlayerEventKind::Exited]);
        assert_eq!(app.up_next.len(), 11, "the one shown was taken to play");
        assert!(app.up_next.iter().all(|v| v.id != shown));
        let mut round = ids(&app.up_next);
        round.push(shown.as_str());
        assert_eq!(sorted(round), sorted(some(0)));

        // Off in the settings tab, on from the command line.
        change_setting(&mut app, HelpMenu::SHUFFLE);
        assert!(!app.settings.shuffle);
        typed(&mut app, ":shuffle on");
        app.on_key(key(KeyCode::Enter));
        assert!(app.settings.shuffle);
    }

    #[tokio::test]
    async fn shuffle_plays_the_jukebox_each_video_once_a_round() {
        let (mut app, dir) = app_that_plays("shuffled");
        app.rng = Rng::seeded(2);
        with_feed(&mut app, 10);
        let listed = app.videos.clone();
        let find = |id: &str| listed.iter().find(|v| v.id.as_str() == id).unwrap().clone();
        for i in 0..8 {
            app.select(i);
            app.on_key(key(KeyCode::Char('e')));
        }
        app.on_key(key(KeyCode::Char('z')));

        // Played from the Jukebox: from that one, then in an order of its own.
        app.show(View::Jukebox, false);
        app.select(0);
        app.on_key(key(KeyCode::Char('a')));
        let mut played = vec![loading(&app).unwrap().to_owned()];
        assert_eq!(played[0], listed[0].id.as_str());
        // Put in while it plays: at the end, it plays this round all the
        // same; put next, it plays next.
        app.show(View::Home, false);
        app.select(8);
        app.on_key(key(KeyCode::Char('e')));
        app.select(9);
        app.on_key(key(KeyCode::Char('E')));
        assert_eq!(app.plays_next().unwrap().id, listed[9].id);
        for _ in 1..10 {
            let next = app.plays_next().unwrap().id.as_str().to_owned();
            app.resolving = None;
            ended(&mut app, &find(played.last().unwrap()));
            assert_eq!(loading(&app), Some(next.as_str()), "the one shown");
            played.push(next);
        }
        assert_eq!(played[1], listed[9].id.as_str(), "E: next");
        let played: Vec<&str> = played.iter().map(String::as_str).collect();
        assert_eq!(sorted(played.clone()), sorted(jukebox(&app)), "each once");
        assert_ne!(played, jukebox(&app), "in an order of its own");
        // After its last, nothing: repeat is off.
        let last = find(played[9]);
        app.resolving = None;
        ended(&mut app, &last);
        assert_eq!(loading(&app), None);

        // Repeat all: a new round, not starting with the one that ended.
        app.settings.repeat = Repeat::All;
        app.show_playing(last.clone(), 599.0, 600.0, true);
        let shown = app.plays_next().unwrap().id.clone();
        assert_ne!(shown, last.id);
        mpv_says(&mut app, [PlayerEventKind::Ended, PlayerEventKind::Exited]);
        assert_eq!(loading(&app), Some(shown.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn zero_goes_back_to_the_start_of_what_plays() {
        let (mut app, dir) = app_that_plays("from-start");
        with_feed(&mut app, 2);
        let (first, second) = (app.videos[0].clone(), app.videos[1].clone());
        // Nothing playing: the selected video plays as Enter plays it, from
        // 0:00 rather than where it was left.
        app.on_key(key(KeyCode::Char('0')));
        assert_eq!(loading(&app), Some(first.id.as_str()));
        assert_eq!(app.start_once, Some((first.id.clone(), 0.0)));
        assert!(app.watch.is_some(), "on its page");

        // Listening to the next one (N), the first still selected: the one
        // playing goes back to its start, still sound alone, with no page.
        app.watch = None;
        (app.resolving, app.start_once) = (None, None);
        app.show_playing(second.clone(), 300.0, 600.0, true);
        app.on_key(key(KeyCode::Char('0')));
        let playing = app.playing.as_ref().unwrap();
        assert_eq!(
            (
                playing.video.id.as_str(),
                playing.position,
                playing.audio_only
            ),
            (second.id.as_str(), 0.0, true)
        );
        assert!(app.resolving.is_none() && app.watch.is_none());
        // On its page, and from the sidebar, the same.
        app.open_watch(second.clone());
        app.playing.as_mut().unwrap().position = 300.0;
        app.on_key(key(KeyCode::Char('0')));
        assert_eq!(app.playing.as_ref().unwrap().position, 0.0);
        app.watch = None;
        app.focus = Focus::Sidebar;
        app.playing.as_mut().unwrap().position = 300.0;
        app.on_key(key(KeyCode::Char('0')));
        assert_eq!(app.playing.as_ref().unwrap().position, 0.0);

        // The next one loading: it starts at its start; the one playing
        // stays where it is.
        app.playing.as_mut().unwrap().position = 300.0;
        app.resolving = Some(Resolving {
            request: app.resolves,
            video: first.clone(),
            audio_only: true,
            auto: false,
            from_jukebox: false,
            carry_on: true,
        });
        app.on_key(key(KeyCode::Char('0')));
        assert_eq!(app.start_once, Some((first.id.clone(), 0.0)));
        assert!(!app.resolving.as_ref().unwrap().carry_on);
        assert_eq!(app.playing.as_ref().unwrap().position, 300.0);

        // Where tuitube can't move mpv (Windows): said, not moved.
        app.resolving = None;
        mpv_control(&mut app, false);
        app.on_key(key(KeyCode::Char('0')));
        assert_eq!(app.playing.as_ref().unwrap().position, 300.0);
        assert!(app.status.as_ref().unwrap().error);
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
