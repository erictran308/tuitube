//! User settings, kept in `settings.toml` in the data directory.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::icons::IconMode;
use crate::ids::ChannelId;
use crate::images::ImageMode;
use crate::links::{self, Link};
use crate::video::{self, Video};
use crate::ytdlp::AudioQuality;
use crate::{config, sponsorblock, theme};

/// Entries of `blocked_channels` and of `blocked_words` read, at most.
const MAX_BLOCKED: usize = 1000;

/// The picture heights the settings offer, tallest first: YouTube's
/// 2160p (4K) down to 360p.
pub const HEIGHTS: [u32; 6] = [2160, 1440, 1080, 720, 480, 360];

/// How the settings and the command line name a height of [`HEIGHTS`].
pub fn height_label(height: u32) -> String {
    match height {
        2160 => "Up to 2160p (4K)".into(),
        1080 => "Up to 1080p (HD)".into(),
        360 => "Up to 360p, to save data".into(),
        h => format!("Up to {h}p"),
    }
}

/// What plays again when the video playing ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    /// The Jukebox, or the list the video was played from: its first video
    /// again after its last.
    All,
    /// The video playing, over and over: mpv plays it again from its start.
    One,
}

impl Repeat {
    /// In the order `r` goes round them, as YouTube's loop button does.
    pub const CHOICES: [Repeat; 3] = [Repeat::Off, Repeat::All, Repeat::One];

    /// Its name in settings.toml.
    pub fn name(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        }
    }

    /// The one after it, as `r` goes round.
    pub fn next(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The theme in use: one of the built-in ones (picked in Settings: `O`).
    pub theme: String,
    /// The best picture to play, in lines (YouTube's "1080p"): 2160, 1440,
    /// 1080, 720, 480, 360. Higher needs a faster connection and computer.
    /// Settings (`O`) picks it.
    pub max_height: u32,
    /// The sound: "high" (about 130 kbps, the best there is without an
    /// account) or "low" (about 70 kbps, to save data). Settings (`O`)
    /// picks it.
    pub audio_quality: AudioQuality,
    /// Play the sound alone, as YouTube Music does: Enter listens, as `a`
    /// does, with no window, and only the sound is downloaded. Settings
    /// (`O`) turns it on and off.
    pub audio_only: bool,
    /// While only the sound plays, colored bars in the player bar move with
    /// it. mpv measures the sound; nothing else runs and nothing is sent
    /// anywhere. Settings (`O`) turns it on and off.
    pub visualizer: bool,
    /// The visualizer's bars all in white (the theme's text color, so they
    /// show on a light theme too) rather than the rainbow. Settings (`O`)
    /// turns it on and off.
    pub visualizer_white: bool,
    /// Remember what you watch here, and where you stopped, so a video
    /// carries on from there. Kept only on this computer.
    pub history: bool,
    /// Show what plays in the system's Now Playing (on macOS: Control
    /// Center, the lock screen, notch apps), with its thumbnail and channel,
    /// and take the media keys. Any app that reads Now Playing sees the
    /// title. Settings (`O`) turns it on and off.
    pub now_playing: bool,
    /// In Pepper (a Ghostty build that shows video inside the terminal:
    /// `PEPPER_PORT` is set), play the picture in the terminal, through
    /// Pepper's player, pepper-play, rather than in mpv's window. Settings
    /// (`O`) turns it on and off.
    pub inline_video: bool,
    /// When a video ends, play the next one: the next card in the list it
    /// was played from, or the rest of its Mix. `A` turns it on and off.
    /// The Jukebox plays on either way: its videos are ones you chose.
    pub autoplay: bool,
    /// What plays again: "off"; "all", the Jukebox or the list a video was
    /// played from, from its first video again after its last; "one", the
    /// video playing, over and over. `r` goes round them.
    pub repeat: Repeat,
    /// Play the Jukebox, playlists (Watch later too) and Mixes in an order
    /// of their own, as YouTube's shuffle button: each video once a round,
    /// none twice in a row. `z` turns it on and off.
    pub shuffle: bool,
    /// Skip the parts of videos SponsorBlock's users marked (sponsor reads,
    /// intros, outros). Off unless turned on: for each video played, it asks
    /// sponsor.ajay.app about every video whose id hashes like it, which
    /// tells that server you played one of them. Settings (`O`) turns it on
    /// and off.
    pub sponsorblock: bool,
    /// Which parts SponsorBlock skips: sponsor, selfpromo, interaction,
    /// intro, outro, preview, hook, music_offtopic, filler.
    pub sponsorblock_categories: Vec<String>,
    /// Show the titles DeArrow's users wrote in place of clickbait ones.
    /// Off unless turned on: for each card shown, it asks sponsor.ajay.app
    /// about every video whose id hashes like it, which tells that server
    /// the videos you looked at were among them. Settings (`O`) turns it on
    /// and off.
    pub dearrow: bool,
    /// How loud videos play, in percent: 0 to 100. `-` and `+` change it.
    pub volume: u32,
    /// Channels whose videos never show, outside Watch later, History, the
    /// Jukebox and your playlists: each a channel's id (`UC…`), its link
    /// (`youtube.com/channel/UC…`) or its name as tuitube shows it, in any
    /// case. A handle (`@name`) can't be told from a video.
    pub blocked_channels: Vec<String>,
    /// Words whose videos never show, in the same places: a title holding
    /// one, as a whole word and in any case, hides its video (`minecraft`
    /// hides "MINECRAFT speedrun" but not "Minecrafter"). A phrase counts as
    /// one word.
    pub blocked_words: Vec<String>,
    /// Show the start of each video's description under it.
    pub descriptions: bool,
    /// Show subtitles, when a video has them in one of
    /// `subtitle_languages`. `T` turns them on and off.
    pub subtitles: bool,
    /// The languages subtitles are wanted in, first first: YouTube's codes
    /// (`en`, `fr`, `pt-BR`). Empty: this computer's language, then English.
    pub subtitle_languages: Vec<String>,
    /// Show every view as a list, a row per video with its channel, views
    /// and age, instead of cards with thumbnails: for terminals that don't
    /// draw images well. `v` switches.
    pub list: bool,
    /// Show the Jukebox as a list even when the other views are cards. `v`
    /// in the Jukebox switches.
    pub jukebox_list: bool,
    /// Take a video off the Jukebox once it has played from there to its
    /// end. Off: the Jukebox keeps its videos, and plays on from the one
    /// that played.
    pub jukebox_autoremove: bool,
    /// Show Shorts: the sidebar's Shorts, and Shorts among search results,
    /// a channel's videos and Mixes. Watch later and History list theirs
    /// either way.
    pub shorts: bool,
    /// Show live streams and premieres while they're live or haven't
    /// started, outside Watch later and History. A feed doesn't say which
    /// videos are live, so one shows until a look at its channel tells.
    pub live: bool,
    /// How old a channel's feed may get before it's fetched again, in
    /// minutes: at least 15 (YouTube's own feeds change no faster), at most
    /// a week. `R` fetches them all now.
    pub refresh_minutes: u32,
    /// How images are drawn: "auto" (what the terminal says it can do),
    /// "kitty", "sixel", "iterm2" or "blocks". `TT_IMAGES` overrides it.
    pub images: ImageMode,
    /// Which icons: "auto" (Nerd Font icons in Ghostty, kitty and WezTerm,
    /// which have them built in), "nerd" or "plain". `TT_ICONS` overrides it.
    pub icons: IconMode,
    /// The narrowest a video card may be, in columns.
    pub card_width: u16,
    /// The sidebar's width, in columns.
    pub sidebar_width: u16,
    /// Where yt-dlp, mpv and Deno are, if they aren't found on `PATH`.
    pub yt_dlp: Option<PathBuf>,
    pub mpv: Option<PathBuf>,
    pub deno: Option<PathBuf>,
    /// Where pepper-play (Pepper's player) is, if not where Pepper says
    /// (`PEPPER_PLAY`).
    pub pepper_play: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: theme::DEFAULT.into(),
            max_height: 1080,
            audio_quality: AudioQuality::High,
            audio_only: false,
            visualizer: true,
            visualizer_white: false,
            history: true,
            now_playing: true,
            inline_video: true,
            autoplay: true,
            repeat: Repeat::Off,
            shuffle: false,
            sponsorblock: false,
            sponsorblock_categories: ["sponsor", "intro", "outro"].map(String::from).into(),
            dearrow: false,
            volume: 100,
            blocked_channels: Vec::new(),
            blocked_words: Vec::new(),
            descriptions: true,
            subtitles: false,
            subtitle_languages: Vec::new(),
            list: false,
            jukebox_list: false,
            jukebox_autoremove: false,
            shorts: true,
            live: true,
            refresh_minutes: 30,
            images: ImageMode::Auto,
            icons: IconMode::Auto,
            card_width: 34,
            sidebar_width: 26,
            yt_dlp: None,
            mpv: None,
            deno: None,
            pepper_play: None,
        }
    }
}

impl Settings {
    /// Defaults if the file doesn't exist yet; an error if it can't be read.
    #[cfg(test)]
    pub fn load(path: &Path) -> Result<Self> {
        Ok(Self::load_checked(path)?.0)
    }

    /// The settings, and the names in the file tuitube doesn't know: a typo
    /// (`histroy = false`, a SponsorBlock category) would otherwise be
    /// ignored without a word.
    pub fn load_checked(path: &Path) -> Result<(Self, Vec<String>)> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), Vec::new()));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("cannot read {}", config::shown(path)));
            }
        };
        let shown = config::shown(path);
        let table: toml::Table =
            toml::from_str(&text).with_context(|| format!("{shown} is invalid"))?;
        let mut unknown: Vec<String> = table
            .keys()
            .filter(|key| !KNOWN.contains(&key.as_str()) && !RETIRED.contains(&key.as_str()))
            .map(|key| crate::text::clean(key))
            .collect();
        let settings: Self =
            toml::from_str(&text).with_context(|| format!("{shown} is invalid"))?;
        for name in &settings.sponsorblock_categories {
            if sponsorblock::category(name).is_none() {
                let name = video::one_line(name, 40);
                unknown.push(format!("the SponsorBlock category “{name}”"));
            }
        }
        for entry in settings.blocked_channels.iter().take(MAX_BLOCKED) {
            if matches!(blocked_channel(entry), Blocked::Unknown) {
                let entry = video::one_line(entry, 40);
                unknown.push(format!(
                    "the blocked channel “{entry}” (a handle: give its name or UC… id)"
                ));
            }
        }
        Ok((settings, unknown))
    }

    /// The card width as used: wide enough for a title, whatever the file
    /// says.
    pub fn card_width(&self) -> u16 {
        self.card_width.clamp(20, 80)
    }

    /// The best picture to play, in lines, whatever the file says.
    pub fn max_height(&self) -> u32 {
        self.max_height.clamp(144, 4320)
    }

    /// Which of [`HEIGHTS`] the settings show as chosen: the tallest that
    /// isn't more than `max_height` (a video has no picture between two of
    /// them).
    pub fn height_choice(&self) -> Option<usize> {
        HEIGHTS.iter().position(|&h| h <= self.max_height())
    }

    /// How old a channel's feed may get, in seconds.
    pub fn refresh_every(&self) -> i64 {
        i64::from(self.refresh_minutes.clamp(15, 7 * 24 * 60)) * 60
    }

    /// The image mode in use: `TT_IMAGES` if it names one, else the file's.
    pub fn image_mode(&self) -> ImageMode {
        config::var("TT_IMAGES")
            .and_then(|v| ImageMode::parse(&v))
            .unwrap_or(self.images)
    }

    /// The icon mode in use: `TT_ICONS` if it names one, else the file's.
    pub fn icon_mode(&self) -> IconMode {
        config::var("TT_ICONS")
            .and_then(|v| IconMode::parse(&v))
            .unwrap_or(self.icons)
    }

    /// The SponsorBlock categories to skip: the ones tuitube knows, each
    /// once, as its own strings. `listening` (only the sound plays) adds the
    /// parts of music videos that aren't music: talk before the song, a
    /// skit after it.
    pub fn skip_categories(&self, listening: bool) -> Vec<&'static str> {
        sponsorblock::CATEGORIES
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| {
                self.sponsorblock_categories.iter().any(|n| n == c)
                    || (listening && *c == sponsorblock::NOT_MUSIC)
            })
            .collect()
    }

    /// The languages subtitles are wanted in: `subtitle_languages`, or this
    /// computer's (`LC_ALL`, `LC_MESSAGES`, `LANG`) and then English. Only
    /// names that are language codes count.
    pub fn subtitle_languages(&self) -> Vec<String> {
        let system = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty());
        languages(&self.subtitle_languages, system.as_deref())
    }

    /// How loud videos play, in percent, whatever the file says.
    pub fn volume(&self) -> u32 {
        self.volume.min(100)
    }

    /// What keeps a video out of the lists, read once for all of them: a
    /// Short, or a live stream or premiere, turned off, or a video blocked.
    pub fn hiding(&self) -> impl Fn(&Video) -> bool + '_ {
        let blocks = self.blocks();
        move |video| {
            (!self.shorts && video.short)
                || (!self.live && (video.live || video.upcoming))
                || blocks.hold(video)
        }
    }

    /// `blocked_channels` and `blocked_words`, read.
    pub fn blocks(&self) -> Blocks {
        let mut blocks = Blocks::default();
        for entry in self.blocked_channels.iter().take(MAX_BLOCKED) {
            match blocked_channel(entry) {
                Blocked::Id(id) => {
                    blocks.ids.insert(id);
                }
                Blocked::Name(name) if !name.is_empty() => {
                    blocks.names.insert(name);
                }
                Blocked::Name(_) | Blocked::Unknown => {}
            }
        }
        blocks.words = self
            .blocked_words
            .iter()
            .take(MAX_BLOCKED)
            .map(|word| video::one_line(word, video::MAX_TITLE).to_lowercase())
            .filter(|word| !word.is_empty())
            .collect();
        blocks
    }

    /// How many channels and words are blocked, as the settings tab says.
    pub fn blocked(&self) -> (usize, usize) {
        let count = |list: &[String]| {
            list.iter()
                .take(MAX_BLOCKED)
                .filter(|entry| !entry.trim().is_empty())
                .count()
        };
        (count(&self.blocked_channels), count(&self.blocked_words))
    }

    pub fn sidebar_width(&self) -> u16 {
        self.sidebar_width.clamp(16, 50)
    }

    /// Sets one setting in settings.toml, as the file is now: whatever was
    /// written there since tuitube started (by hand, or by another tuitube)
    /// stays, so a `history = false` added meanwhile isn't undone. A file that
    /// no longer reads is left alone. Returns the settings the file now holds.
    pub fn save_one(path: &Path, key: &str, value: toml::Value) -> Result<Self> {
        let shown = config::shown(path);
        let mut table: toml::Table = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("{shown} is invalid, so it wasn't changed"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(e).with_context(|| format!("cannot read {shown}")),
        };
        table.insert(key.to_string(), value);
        let text = toml::to_string(&table)?;
        let settings: Self = toml::from_str(&text)?;
        write_private(path, &text).with_context(|| format!("cannot write {shown}"))?;
        Ok(settings)
    }
}

/// Writes `text` to `path` readable only by the user, and all at once: a new
/// file of this process's own (two tuitubes saving at once can't remove or
/// publish each other's) renamed over the old one.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let new = path.with_extension(format!("toml.{}.new", std::process::id()));
    let write = || -> std::io::Result<()> {
        let _ = std::fs::remove_file(&new);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&new)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&new, path)
    };
    write()
}

/// The settings tuitube reads.
const KNOWN: [&str; 35] = [
    "theme",
    "max_height",
    "audio_quality",
    "audio_only",
    "visualizer",
    "visualizer_white",
    "history",
    "now_playing",
    "inline_video",
    "autoplay",
    "repeat",
    "shuffle",
    "sponsorblock",
    "sponsorblock_categories",
    "dearrow",
    "volume",
    "blocked_channels",
    "blocked_words",
    "descriptions",
    "subtitles",
    "subtitle_languages",
    "list",
    "jukebox_list",
    "jukebox_autoremove",
    "shorts",
    "live",
    "refresh_minutes",
    "images",
    "icons",
    "card_width",
    "sidebar_width",
    "yt_dlp",
    "mpv",
    "deno",
    "pepper_play",
];

/// Settings earlier versions wrote, skipped without a warning.
const RETIRED: [&str; 1] = ["refresh_hours"];

/// `blocked_channels` and `blocked_words`, read once to match many videos
/// against: a reload checks every video it lists.
#[derive(Default)]
pub struct Blocks {
    ids: HashSet<ChannelId>,
    /// Channels' names, cleaned as names from YouTube are, in lower case.
    names: HashSet<String>,
    /// In lower case; none empty, which would match nothing.
    words: Vec<String>,
}

impl Blocks {
    /// Whether `video` is blocked: its channel is, or its title (YouTube's,
    /// never DeArrow's) holds a blocked word.
    pub fn hold(&self, video: &Video) -> bool {
        if video
            .channel_id
            .as_ref()
            .is_some_and(|id| self.ids.contains(id))
        {
            return true;
        }
        if !video.channel.is_empty() && self.names.contains(&video.channel.to_lowercase()) {
            return true;
        }
        let title = video.title.to_lowercase();
        self.words.iter().any(|word| holds_word(&title, word))
    }
}

/// An entry of `blocked_channels`, read.
enum Blocked {
    Id(ChannelId),
    /// A channel's name, cleaned as names from YouTube are, in lower case.
    Name(String),
    /// A handle, or a link to one: videos don't say their channel's.
    Unknown,
}

fn blocked_channel(entry: &str) -> Blocked {
    let entry = entry.trim();
    if let Some(id) = ChannelId::parse(entry) {
        return Blocked::Id(id);
    }
    match links::parse(entry) {
        Some(Link::Channel(id)) => return Blocked::Id(id),
        Some(Link::Named(_)) => return Blocked::Unknown,
        _ => {}
    }
    if entry.starts_with('@') {
        return Blocked::Unknown;
    }
    Blocked::Name(video::one_line(entry, video::MAX_CHANNEL).to_lowercase())
}

/// Whether `text` holds `word` (both in lower case) as a whole word, not
/// inside a longer one: `ai` isn't in "maintain". Scripts written without
/// spaces between words (Chinese, Japanese, Thai) match anywhere.
fn holds_word(text: &str, word: &str) -> bool {
    // Letters and digits of scripts that put spaces between words.
    let runs_on = |c: char| {
        c.is_alphanumeric()
            && !matches!(
                u32::from(c),
                0x0E00..=0x109F | 0x1780..=0x17FF | 0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0xFF66..=0xFF9F
            )
    };
    let (Some(first), Some(last)) = (word.chars().next(), word.chars().next_back()) else {
        return false;
    };
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !(before.is_some_and(runs_on) && runs_on(first))
            && !(after.is_some_and(runs_on) && runs_on(last))
    })
}

/// The subtitle languages wanted: `set`, or the language of `system`
/// (a locale, `fr_FR.UTF-8`), then English.
fn languages(set: &[String], system: Option<&str>) -> Vec<String> {
    let code = |s: &str| {
        let parts: Vec<&str> = s.split('-').collect();
        (1..=3).contains(&parts.len())
            && (2..=3).contains(&parts[0].len())
            && parts[0].bytes().all(|b| b.is_ascii_alphabetic())
            && parts[1..]
                .iter()
                .all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
    };
    let mut wanted: Vec<String> = set.iter().filter(|s| code(s)).cloned().collect();
    if wanted.is_empty() {
        let spoken = system
            .and_then(|locale| locale.split(['_', '.', '@']).next())
            .filter(|lang| code(lang) && *lang != "C" && *lang != "POSIX")
            .map(str::to_lowercase);
        wanted.extend(spoken);
        if !wanted.iter().any(|lang| lang == "en") {
            wanted.push("en".into());
        }
    }
    wanted.truncate(8);
    wanted
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitles_come_in_the_languages_asked_for_or_this_computers() {
        let set = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(languages(&[], Some("fr_FR.UTF-8")), ["fr", "en"]);
        assert_eq!(languages(&[], Some("en_GB.UTF-8")), ["en"]);
        assert_eq!(languages(&[], Some("C")), ["en"]);
        assert_eq!(languages(&[], None), ["en"]);
        assert_eq!(
            languages(&set(&["pt-BR", "not a code", "de"]), Some("fr_FR")),
            ["pt-BR", "de"]
        );
    }

    #[test]
    fn blocked_channels_and_words_hide_their_videos() {
        let video = |title: &str, channel: &str, id: &str| Video {
            title: title.into(),
            channel: channel.into(),
            channel_id: ChannelId::parse(id),
            ..Video::placeholder(crate::ids::VideoId::parse("dQw4w9WgXcQ").unwrap(), false)
        };
        let id = "UCsBjURrPoezykLs9EqgamOA";
        let settings = Settings {
            blocked_channels: [
                "  fireship ",
                "https://www.youtube.com/channel/UCX6OQ3DkcsbYNE6H8uQQuVA",
                "@somehandle",
                "",
            ]
            .map(String::from)
            .into(),
            blocked_words: ["minecraft", "Ai", "", "  ", "you won't  believe", "原神"]
                .map(String::from)
                .into(),
            ..Settings::default()
        };
        let hides = settings.hiding();
        let shown = |title: &str, channel: &str, id: &str| !hides(&video(title, channel, id));
        assert!(
            !shown("Rust in 100 seconds", "Fireship", id),
            "by name, any case"
        );
        assert!(
            !shown("A video", "MrBeast", "UCX6OQ3DkcsbYNE6H8uQQuVA"),
            "by a link's id"
        );
        assert!(shown("A video", "", id), "no name to match");
        assert!(!shown("MINECRAFT speedrun", "Someone", id));
        assert!(!shown("Speedrun (#minecraft)", "Someone", id));
        assert!(shown("Minecrafter reacts", "Someone", id), "whole words");
        assert!(
            shown("How to maintain a bike", "Someone", id),
            "ai isn't in maintain"
        );
        assert!(!shown("Will AI take over?", "Someone", id));
        assert!(!shown("You won't believe this", "Someone", id), "a phrase");
        assert!(
            !shown("【原神】新キャラ", "Someone", id),
            "no spaces in Japanese"
        );
        assert!(
            shown("Rust in 100 seconds", "Someone", id),
            "empty words match nothing"
        );
        assert_eq!(settings.blocked(), (3, 4));
        assert!(
            !Settings::default()
                .blocks()
                .hold(&video("Anything", "Anyone", id))
        );
    }

    #[test]
    fn every_setting_is_known_and_nothing_else() {
        let all = Settings {
            yt_dlp: Some("/x".into()),
            mpv: Some("/x".into()),
            deno: Some("/x".into()),
            pepper_play: Some("/x".into()),
            ..Settings::default()
        };
        let table: toml::Table = toml::from_str(&toml::to_string(&all).unwrap()).unwrap();
        let mut written: Vec<&str> = table.keys().map(String::as_str).collect();
        let mut known = KNOWN.to_vec();
        written.sort_unstable();
        known.sort_unstable();
        assert_eq!(written, known);
    }

    #[test]
    fn a_blocked_handle_is_reported_as_unknown() {
        let dir = std::env::temp_dir().join(format!("tuitube-blocked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = path(&dir);
        std::fs::write(
            &file,
            "blocked_channels = [\"@foo\", \"youtube.com/@bar\", \"Fireship\", \"UCsBjURrPoezykLs9EqgamOA\"]\nblocked_words = [\"ai\"]\nvolume = 250\ndearrow = true\n",
        )
        .unwrap();
        let (settings, unknown) = Settings::load_checked(&file).unwrap();
        assert_eq!(unknown.len(), 2, "{unknown:?}");
        assert!(unknown[0].contains("“@foo”"), "{unknown:?}");
        assert!(settings.dearrow);
        assert_eq!(settings.volume(), 100, "at most 100");
        assert_eq!(Settings::default().volume(), 100);
        assert!(!Settings::default().dearrow, "opt-in");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_one_setting_keeps_what_the_file_gained_since_start() {
        let dir = std::env::temp_dir().join(format!("tuitube-save-one-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = path(&dir);
        std::fs::write(&file, "sponsorblock = true\n").unwrap();
        // Edited by hand (or by another tuitube) after this one started.
        std::fs::write(
            &file,
            "history = false\nsponsorblock = false\nmpv = \"/opt/x/mpv\"\nhistroy = 1\n",
        )
        .unwrap();
        let saved = Settings::save_one(&file, "autoplay", false.into()).unwrap();
        assert!(!saved.history && !saved.sponsorblock && !saved.autoplay);
        let (loaded, unknown) = Settings::load_checked(&file).unwrap();
        assert_eq!(loaded, saved);
        assert_eq!(loaded.mpv.as_deref(), Some(Path::new("/opt/x/mpv")));
        assert_eq!(unknown, ["histroy"], "an unknown name stays to be reported");
        // A file that doesn't read is left as it is.
        std::fs::write(&file, "history = \"no\"").unwrap();
        assert!(Settings::save_one(&file, "theme", "latte".into()).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "history = \"no\"");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_means_defaults_and_saving_round_trips() {
        let dir = std::env::temp_dir().join(format!("tuitube-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = path(&dir);
        let _ = std::fs::remove_file(&file);
        assert_eq!(Settings::load(&file).unwrap(), Settings::default());
        let settings = Settings {
            theme: "latte".into(),
            max_height: 720,
            audio_quality: AudioQuality::Low,
            ..Settings::default()
        };
        Settings::save_one(&file, "theme", "latte".into()).unwrap();
        Settings::save_one(&file, "audio_quality", "low".into()).unwrap();
        let saved = Settings::save_one(&file, "max_height", 720.into()).unwrap();
        assert_eq!(saved, settings, "the file's settings come back");
        assert_eq!(Settings::load(&file).unwrap(), settings);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "only yours");
        }
        std::fs::write(&file, "card_width = 2").unwrap();
        assert_eq!(Settings::load(&file).unwrap().card_width(), 20);
        // A height written by hand shows as the choice it plays as.
        for (height, choice) in [
            (900, Some(3)),
            (99_999, Some(0)),
            (1080, Some(2)),
            (200, None),
        ] {
            std::fs::write(&file, format!("max_height = {height}")).unwrap();
            assert_eq!(Settings::load(&file).unwrap().height_choice(), choice);
        }
        std::fs::write(&file, "audio_quality = \"best\"").unwrap();
        assert!(Settings::load(&file).is_err(), "not one tuitube knows");
        std::fs::write(&file, "histroy = false\nrefresh_hours = 72\nhistory = true").unwrap();
        let (settings, unknown) = Settings::load_checked(&file).unwrap();
        assert_eq!(
            unknown,
            ["histroy"],
            "a typo is reported, an old setting isn't"
        );
        assert!(settings.history);
        std::fs::write(
            &file,
            "sponsorblock = true\nsponsorblock_categories = [\"outro\", \"sponsr\", \"sponsor\", \"outro\"]",
        )
        .unwrap();
        let (settings, unknown) = Settings::load_checked(&file).unwrap();
        assert_eq!(unknown, ["the SponsorBlock category “sponsr”"]);
        assert_eq!(settings.skip_categories(false), ["sponsor", "outro"]);
        assert_eq!(
            settings.skip_categories(true),
            ["sponsor", "outro", "music_offtopic"],
            "listening, the parts that aren't music too"
        );
        assert!(!Settings::default().sponsorblock, "opt-in");
        std::fs::write(&file, "audio_only = true\nvisualizer = false").unwrap();
        let (settings, unknown) = Settings::load_checked(&file).unwrap();
        assert!(unknown.is_empty(), "{unknown:?}");
        assert!(settings.audio_only && !settings.visualizer);
        assert!(
            !Settings::default().audio_only,
            "videos play with their picture"
        );
        assert!(Settings::default().visualizer);
        assert!(
            !Settings::default().visualizer_white,
            "the rainbow at first"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
