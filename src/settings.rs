//! User settings, kept in `settings.toml` in the data directory.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::icons::IconMode;
use crate::images::ImageMode;
use crate::video::Video;
use crate::ytdlp::AudioQuality;
use crate::{sponsorblock, theme};

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
    /// Remember what you watch here, and where you stopped, so a video
    /// carries on from there. Kept only on this computer.
    pub history: bool,
    /// When a video ends, play the next one: the next card in the list it
    /// was played from, or the rest of its Mix. `A` turns it on and off.
    /// The Jukebox plays on either way: its videos are ones you chose.
    pub autoplay: bool,
    /// Skip the parts of videos SponsorBlock's users marked (sponsor reads,
    /// intros, outros). Off unless turned on: for each video played, it asks
    /// sponsor.ajay.app about every video whose id hashes like it, which
    /// tells that server you played one of them. Settings (`O`) turns it on
    /// and off.
    pub sponsorblock: bool,
    /// Which parts SponsorBlock skips: sponsor, selfpromo, interaction,
    /// intro, outro, preview, hook, music_offtopic, filler.
    pub sponsorblock_categories: Vec<String>,
    /// Show the start of each video's description under it.
    pub descriptions: bool,
    /// Show subtitles, when a video has them in one of
    /// `subtitle_languages`. `T` turns them on and off.
    pub subtitles: bool,
    /// The languages subtitles are wanted in, first first: YouTube's codes
    /// (`en`, `fr`, `pt-BR`). Empty: this computer's language, then English.
    pub subtitle_languages: Vec<String>,
    /// Show the Jukebox as a list, a row per video, instead of cards with
    /// thumbnails. `v` in the Jukebox switches.
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: theme::DEFAULT.into(),
            max_height: 1080,
            audio_quality: AudioQuality::High,
            history: true,
            autoplay: true,
            sponsorblock: false,
            sponsorblock_categories: ["sponsor", "intro", "outro"].map(String::from).into(),
            descriptions: true,
            subtitles: false,
            subtitle_languages: Vec::new(),
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
                let name = crate::video::one_line(name, 40);
                unknown.push(format!("the SponsorBlock category “{name}”"));
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
    /// once, as its own strings.
    pub fn skip_categories(&self) -> Vec<&'static str> {
        sponsorblock::CATEGORIES
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| self.sponsorblock_categories.iter().any(|n| n == c))
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

    /// Whether these settings keep `video` out of the lists: a Short, or a
    /// live stream or premiere, turned off.
    pub fn hides(&self, video: &Video) -> bool {
        (!self.shorts && video.short) || (!self.live && (video.live || video.upcoming))
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
const KNOWN: [&str; 22] = [
    "theme",
    "max_height",
    "audio_quality",
    "history",
    "autoplay",
    "sponsorblock",
    "sponsorblock_categories",
    "descriptions",
    "subtitles",
    "subtitle_languages",
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
];

/// Settings earlier versions wrote, skipped without a warning.
const RETIRED: [&str; 1] = ["refresh_hours"];

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
        assert_eq!(settings.skip_categories(), ["sponsor", "outro"]);
        assert!(!Settings::default().sponsorblock, "opt-in");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
