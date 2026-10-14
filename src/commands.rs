//! The command line (`:`): every action and quick setting by name, for vim
//! users and for anyone who'd rather type what they want than remember its
//! key. Every command and every argument completes as you type: the list
//! above the line narrows, Tab (or the arrows) puts the one chosen on the
//! line, and Enter runs the line as it is: never a guess at what part of
//! one meant. `:quality 720`, `:sound low`, `:theme nord`,
//! `:autoplay off`, `:set noautoplay`, `:1:23`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::app::{App, Entry, View};
use crate::settings::{self, HEIGHTS, Repeat, Settings};
use crate::ytdlp::AudioQuality;
use crate::{chapters, config, text, theme, video};

/// Files and folders a folder's completion lists, at most.
const MAX_FILES: usize = 200;
/// Entries read from one folder to complete a file name, at most.
const MAX_READ: usize = 5000;

/// What a command's argument is: how it completes and how it's read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    None,
    /// A picture height from [`HEIGHTS`].
    Height,
    Sound,
    Theme,
    /// `on` or `off`; without it, the setting is turned over.
    OnOff,
    /// `off`, `all` or `one`; without it, the next of them.
    Repeat,
    /// A volume, in percent: 0 to 100.
    Volume,
    /// A list in the sidebar.
    Place,
    Jukebox,
    /// One of your subscriptions.
    Channel,
    /// One of the chapters of the video playing.
    Chapter,
    /// A time in the video playing: `1:23`, `+30`, `50%`.
    Time,
    /// Words to search YouTube for.
    Query,
    /// A file: subscriptions.csv from Google Takeout.
    File,
}

/// What a command needs to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Needs {
    Nothing,
    /// A video selected in the grid.
    Video,
    /// A video playing.
    Playing,
    /// A video playing or loading.
    Playback,
}

/// A command: its name, the other names it answers to, its argument, and
/// what it does.
pub struct Spec {
    pub name: &'static str,
    aliases: &'static [&'static str],
    pub arg: Arg,
    /// It runs without its argument too.
    optional: bool,
    /// The argument as the list shows it: `<height>`, `[on|off]`.
    pub hint: &'static str,
    pub about: &'static str,
    /// The key that does the same, if one does.
    pub key: &'static str,
    needs: Needs,
}

#[allow(clippy::too_many_arguments)]
const fn spec(
    name: &'static str,
    aliases: &'static [&'static str],
    arg: Arg,
    optional: bool,
    hint: &'static str,
    about: &'static str,
    key: &'static str,
    needs: Needs,
) -> Spec {
    Spec {
        name,
        aliases,
        arg,
        optional,
        hint,
        about,
        key,
        needs,
    }
}

/// A command that turns a setting on or off.
const fn toggle(name: &'static str, about: &'static str, key: &'static str) -> Spec {
    spec(
        name,
        &[],
        Arg::OnOff,
        true,
        "[on|off]",
        about,
        key,
        Needs::Nothing,
    )
}

/// A command that takes nothing.
const fn plain(
    name: &'static str,
    aliases: &'static [&'static str],
    about: &'static str,
    key: &'static str,
    needs: Needs,
) -> Spec {
    spec(name, aliases, Arg::None, false, "", about, key, needs)
}

/// Every command, in the order the list shows them: the quick settings
/// first.
pub const COMMANDS: &[Spec] = &[
    spec(
        "quality",
        &["video"],
        Arg::Height,
        false,
        "<height>",
        "Video quality: up to 360p … 2160p (4K)",
        "",
        Needs::Nothing,
    ),
    spec(
        "sound",
        &["audio"],
        Arg::Sound,
        false,
        "<high|low>",
        "Sound quality: high or low (saves data)",
        "",
        Needs::Nothing,
    ),
    spec(
        "audio-only",
        &["sound-only", "music"],
        Arg::OnOff,
        true,
        "[on|off]",
        "Audio only, as YouTube Music: Enter plays the sound alone",
        "",
        Needs::Nothing,
    ),
    spec(
        "visualizer",
        &["bars"],
        Arg::OnOff,
        true,
        "[on|off]",
        "Colored bars that move with the sound, while only it plays",
        "",
        Needs::Nothing,
    ),
    toggle(
        "inline-video",
        "In Pepper, the picture plays in the terminal, not in mpv's window",
        "",
    ),
    toggle("autoplay", "Autoplay: the next video when one ends", "A"),
    spec(
        "repeat",
        &["loop"],
        Arg::Repeat,
        true,
        "[off|all|one]",
        "Repeat: the list from its first after its last, or one video",
        "r",
        Needs::Nothing,
    ),
    spec(
        "subtitles",
        &["subs", "captions", "cc"],
        Arg::OnOff,
        true,
        "[on|off]",
        "Subtitles, in your language when a video has them",
        "T",
        Needs::Nothing,
    ),
    toggle(
        "sponsorblock",
        "Skip sponsors with SponsorBlock (asks sponsor.ajay.app)",
        "",
    ),
    toggle(
        "dearrow",
        "Better titles from DeArrow (asks sponsor.ajay.app)",
        "",
    ),
    spec(
        "volume",
        &["vol"],
        Arg::Volume,
        false,
        "<0-100>",
        "Volume, in percent",
        "- +",
        Needs::Nothing,
    ),
    plain("mute", &["unmute"], "Sound off or on", "M", Needs::Nothing),
    spec(
        "theme",
        &["colors"],
        Arg::Theme,
        false,
        "<name>",
        "Colors: one of the built-in themes",
        "",
        Needs::Nothing,
    ),
    toggle("shorts", "Show Shorts", ""),
    toggle("live", "Show live streams and premieres", ""),
    toggle("descriptions", "Show the start of each description", ""),
    toggle("list", "Show every view as a list, without thumbnails", "v"),
    toggle("history", "Keep a history of what you watch, here only", ""),
    toggle(
        "now-playing",
        "Show what plays in Now Playing, with the media keys (macOS)",
        "",
    ),
    toggle(
        "jukebox-list",
        "Show the Jukebox as a list, without thumbnails",
        "v",
    ),
    toggle(
        "jukebox-autoremove",
        "Take a video off the Jukebox once it has played",
        "",
    ),
    plain(
        "play",
        &[],
        "Play the selected video",
        "Enter",
        Needs::Video,
    ),
    plain(
        "listen",
        &[],
        "Listen to the selected video: sound only",
        "a",
        Needs::Video,
    ),
    plain(
        "pause",
        &["resume"],
        "Pause or play",
        "Space",
        Needs::Playing,
    ),
    plain("stop", &[], "Stop playing", "X", Needs::Playback),
    plain(
        "info",
        &["description", "comments"],
        "The selected video's description and comments",
        "i",
        Needs::Video,
    ),
    plain(
        "next",
        &["skip"],
        "Play the next video now",
        "N",
        Needs::Playback,
    ),
    spec(
        "seek",
        &["goto"],
        Arg::Time,
        false,
        "<time>",
        "Go to a time: 1:23, 90, +30, -1:00, 50% (or type the time alone)",
        "t",
        Needs::Playing,
    ),
    spec(
        "chapter",
        &["chapters"],
        Arg::Chapter,
        true,
        "[title]",
        "Go to a chapter, or list them",
        "t",
        Needs::Playing,
    ),
    plain(
        "prev-chapter",
        &[],
        "The chapter before, or this one's start",
        "[",
        Needs::Playing,
    ),
    plain("next-chapter", &[], "The next chapter", "]", Needs::Playing),
    plain(
        "mix",
        &[],
        "YouTube's Mix of the selected video (music only)",
        "m",
        Needs::Video,
    ),
    spec(
        "jukebox",
        &[],
        Arg::Jukebox,
        false,
        "<add|next|clear>",
        "Put the selected video in the Jukebox, or clear it",
        "e",
        Needs::Nothing,
    ),
    plain(
        "later",
        &["watch-later"],
        "Save to Watch later, or take it off",
        "w",
        Needs::Video,
    ),
    plain(
        "remove",
        &[],
        "Take the selected video off the Jukebox, Watch later or History",
        "x",
        Needs::Video,
    ),
    spec(
        "channel",
        &[],
        Arg::Channel,
        true,
        "[name]",
        "The selected video's channel, or one you subscribe to",
        "c",
        Needs::Nothing,
    ),
    plain(
        "subscribe",
        &["unsubscribe"],
        "Subscribe to the selected video's channel, or unsubscribe",
        "S",
        Needs::Video,
    ),
    plain(
        "undo",
        &[],
        "Subscribe again to the channel you just left",
        "u",
        Needs::Nothing,
    ),
    plain(
        "browser",
        &["open"],
        "Open the selected video in your browser",
        "o",
        Needs::Video,
    ),
    plain(
        "copy",
        &["link"],
        "Copy the selected video's link",
        "y",
        Needs::Video,
    ),
    spec(
        "go",
        &[],
        Arg::Place,
        false,
        "<place>",
        "Home, Shorts, Jukebox, Watch later, History",
        "",
        Needs::Nothing,
    ),
    spec(
        "search",
        &["find"],
        Arg::Query,
        true,
        "[words]",
        "Search YouTube",
        "/",
        Needs::Nothing,
    ),
    plain(
        "back",
        &[],
        "Back to the view before",
        "Backspace",
        Needs::Nothing,
    ),
    plain(
        "refresh",
        &["reload"],
        "Refresh: your subscriptions, the search or the channel",
        "R",
        Needs::Nothing,
    ),
    spec(
        "import",
        &[],
        Arg::File,
        true,
        "[file]",
        "Import from Google Takeout: a file, or the unzipped folder",
        "I",
        Needs::Nothing,
    ),
    plain("keys", &["help", "h"], "Every key", "?", Needs::Nothing),
    plain(
        "settings",
        &["options"],
        "The settings",
        "O",
        Needs::Nothing,
    ),
    plain("quit", &["q", "exit"], "Quit tuitube", "q", Needs::Nothing),
];

/// A setting turned on or off with a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    Autoplay,
    Subtitles,
    SponsorBlock,
    DeArrow,
    Shorts,
    Live,
    Descriptions,
    History,
    NowPlaying,
    List,
    JukeboxList,
    JukeboxAutoremove,
    AudioOnly,
    Visualizer,
    InlineVideo,
}

impl Toggle {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "autoplay" => Toggle::Autoplay,
            "subtitles" | "subs" | "captions" | "cc" => Toggle::Subtitles,
            "sponsorblock" => Toggle::SponsorBlock,
            "dearrow" => Toggle::DeArrow,
            "shorts" => Toggle::Shorts,
            "live" => Toggle::Live,
            "descriptions" => Toggle::Descriptions,
            "history" => Toggle::History,
            "now-playing" => Toggle::NowPlaying,
            "list" => Toggle::List,
            "jukebox-list" => Toggle::JukeboxList,
            "jukebox-autoremove" => Toggle::JukeboxAutoremove,
            "audio-only" | "sound-only" | "music" => Toggle::AudioOnly,
            "visualizer" | "bars" => Toggle::Visualizer,
            "inline-video" => Toggle::InlineVideo,
            _ => return None,
        })
    }

    pub fn is_on(self, settings: &Settings) -> bool {
        match self {
            Toggle::Autoplay => settings.autoplay,
            Toggle::Subtitles => settings.subtitles,
            Toggle::SponsorBlock => settings.sponsorblock,
            Toggle::DeArrow => settings.dearrow,
            Toggle::Shorts => settings.shorts,
            Toggle::Live => settings.live,
            Toggle::Descriptions => settings.descriptions,
            Toggle::History => settings.history,
            Toggle::NowPlaying => settings.now_playing,
            Toggle::List => settings.list,
            Toggle::JukeboxList => settings.jukebox_list,
            Toggle::JukeboxAutoremove => settings.jukebox_autoremove,
            Toggle::AudioOnly => settings.audio_only,
            Toggle::Visualizer => settings.visualizer,
            Toggle::InlineVideo => settings.inline_video,
        }
    }
}

/// What `jukebox` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JukeboxAction {
    Add,
    Next,
    Clear,
}

/// A command line, read.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Quality(u32),
    Sound(AudioQuality),
    Theme(&'static str),
    /// A setting on, off, or (`None`) turned over.
    Set(Toggle, Option<bool>),
    /// The volume, in percent.
    Volume(u32),
    /// What plays again, or (`None`) the next choice.
    Repeat(Option<Repeat>),
    Mute,
    Play,
    Listen,
    Pause,
    Stop,
    Next,
    GoTo(f64),
    Chapters,
    Chapter(usize),
    PreviousChapter,
    NextChapter,
    Mix,
    Jukebox(JukeboxAction),
    WatchLater,
    Remove,
    /// The selected video's channel (`None`), or a sidebar row.
    Channel(Option<usize>),
    Subscribe,
    Undo,
    Browser,
    Copy,
    Go(Entry),
    /// Words to search for; none opens the search box.
    Search(String),
    /// A file to import; none opens the import box.
    Import(String),
    Back,
    Refresh,
    Keys,
    Settings,
    Info,
    Quit,
}

/// One completion: what goes on the line, and what the list says of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub text: String,
    /// A command's argument, as the list shows it.
    pub hint: &'static str,
    pub about: String,
    pub key: &'static str,
    /// The value in use: the quality, the theme, on or off.
    pub current: bool,
    /// It can run now (a command that needs a video playing can't while
    /// none is).
    pub ready: bool,
    /// A command that takes something after it: a space follows it.
    takes_arg: bool,
}

/// The completions of a line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Completion {
    /// Where in the line what they complete starts.
    pub from: usize,
    /// Best first.
    pub candidates: Vec<Candidate>,
    /// What they are: "Commands", or the command's about.
    pub title: String,
}

/// Folders listed to complete file names, each read once a session.
type Folders = HashMap<PathBuf, Vec<(String, bool)>>;

/// The line being typed after `:`.
#[derive(Default)]
pub struct CommandLine {
    pub text: String,
    /// The completions of the line as typed.
    pub completion: Completion,
    /// The line as typed, while Tab or the arrows put completions on it.
    typed: Option<String>,
    /// The completion on the line, once Tab or an arrow chose one.
    pub selected: Option<usize>,
    /// The first completion shown. Drawing moves it to show the selected.
    pub scroll: usize,
    /// Why the line didn't run, until it changes.
    pub error: Option<String>,
    folders: Folders,
}

impl CommandLine {
    /// The line the completions are of: as typed, not as Tab changed it.
    pub fn source(&self) -> &str {
        self.typed.as_deref().unwrap_or(&self.text)
    }

    /// The line with completion `i` in place of what it completes.
    pub fn with(&self, i: usize) -> Option<String> {
        let candidate = self.completion.candidates.get(i)?;
        let mut line = self.source()[..self.completion.from].to_string();
        line.push_str(&candidate.text);
        if candidate.takes_arg {
            line.push(' ');
        }
        Some(line)
    }

    /// Tab, the arrows: the next (or, `back`, the one before) completion,
    /// on the line, round from the last to the first.
    pub fn step(&mut self, back: bool) {
        let n = self.completion.candidates.len();
        if n == 0 {
            return;
        }
        let next = match (self.selected, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        };
        if self.typed.is_none() {
            self.typed = Some(self.text.clone());
        }
        self.selected = Some(next);
        if let Some(line) = self.with(next) {
            self.text = line;
        }
        self.error = None;
    }

    /// The line was typed in: its completions are of it now. They're
    /// worked out again with [`CommandLine::complete`].
    pub fn edited(&mut self) {
        self.typed = None;
        self.selected = None;
        self.scroll = 0;
        self.error = None;
    }

    /// Works out the completions of the line as typed.
    pub fn complete(&mut self, app: &App) {
        let mut folders = std::mem::take(&mut self.folders);
        self.completion = complete(self.source(), app, &mut folders);
        self.folders = folders;
    }
}

/// Where the command starts in `line`, after spaces and a vim user's
/// `set `, and whether that `set ` was there.
fn command_start(line: &str) -> (usize, bool) {
    let start = line.len() - line.trim_start().len();
    let rest = &line[start..];
    match rest.get(..4) {
        Some(set) if set.eq_ignore_ascii_case("set ") => {
            let after = &rest[4..];
            (start + 4 + after.len() - after.trim_start().len(), true)
        }
        _ => (start, false),
    }
}

/// The command named `word`, by its name or another it answers to.
pub fn find(word: &str) -> Option<&'static Spec> {
    let word = word.to_lowercase();
    COMMANDS
        .iter()
        .find(|s| s.name == word || s.aliases.contains(&word.as_str()))
}

fn needs_met(needs: Needs, app: &App) -> bool {
    match needs {
        Needs::Nothing => true,
        Needs::Video => app.selected_video().is_some(),
        Needs::Playing => app.playing.is_some(),
        Needs::Playback => app.playing.is_some() || app.resolving.is_some(),
    }
}

/// Reads a command line. The error says what's wrong, for the line.
pub fn parse(line: &str, app: &App) -> Result<Command, String> {
    let (start, _) = command_start(line);
    let rest = line[start..].trim_end();
    let (word, arg) = match rest.split_once(char::is_whitespace) {
        Some((word, arg)) => (word, arg.trim_start()),
        None => (rest, ""),
    };
    if word.is_empty() {
        return Err("Type a command: Tab lists them".into());
    }
    let lower = word.to_lowercase();
    // A time alone, as vim's `:42` goes to a line.
    if arg.is_empty()
        && lower.starts_with(|c: char| c.is_ascii_digit() || c == '+' || c == '-')
        && find(&lower).is_none()
    {
        return read(find("seek").expect("seek is a command"), word, app);
    }
    // vim's `:set noautoplay`.
    if let Some(toggle) = lower.strip_prefix("no").and_then(Toggle::named)
        && find(&lower).is_none()
        && arg.is_empty()
    {
        return Ok(Command::Set(toggle, Some(false)));
    }
    if lower == "norepeat" && arg.is_empty() {
        return Ok(Command::Repeat(Some(Repeat::Off)));
    }
    let Some(spec) = find(word) else {
        return Err(format!("No command “{}”", video::one_line(word, 40)));
    };
    read(spec, arg, app)
}

/// Reads `spec`'s argument.
fn read(spec: &Spec, arg: &str, app: &App) -> Result<Command, String> {
    if !needs_met(spec.needs, app) {
        return Err(match spec.needs {
            Needs::Video => "No video selected".into(),
            _ => "Nothing is playing".into(),
        });
    }
    if spec.arg == Arg::None {
        if !arg.is_empty() {
            return Err(format!("{} takes nothing after it", spec.name));
        }
    } else if arg.is_empty() && !spec.optional {
        return Err(format!("{} needs {}: {}", spec.name, spec.hint, spec.about));
    }
    let lower = arg.to_lowercase();
    let pick = |options: &str| format!("{} takes {options}", spec.name);
    Ok(match spec.name {
        "quality" => {
            let height = match lower.trim_end_matches('p') {
                "4k" => 2160,
                h => h.parse().unwrap_or(0),
            };
            if !HEIGHTS.contains(&height) {
                return Err(pick("2160, 1440, 1080, 720, 480 or 360"));
            }
            Command::Quality(height)
        }
        "sound" => match lower.as_str() {
            "high" => Command::Sound(AudioQuality::High),
            "low" => Command::Sound(AudioQuality::Low),
            _ => return Err(pick("high or low")),
        },
        "theme" => {
            let found = theme::names()
                .iter()
                .find(|(id, name)| id.eq_ignore_ascii_case(arg) || name.to_lowercase() == lower);
            match found {
                Some(&(id, _)) => Command::Theme(id),
                None => return Err(pick("the name of a theme: Tab lists them")),
            }
        }
        "go" => {
            let entry = match lower.as_str() {
                "home" => Entry::Home,
                "shorts" if app.settings.shorts => Entry::Shorts,
                "shorts" => return Err("Shorts are hidden: :shorts on shows them".into()),
                "search" => Entry::Search,
                "jukebox" => Entry::Jukebox,
                "later" | "watch-later" => Entry::WatchLater,
                "history" => Entry::History,
                _ => return Err(pick("home, shorts, search, jukebox, later or history")),
            };
            Command::Go(entry)
        }
        "jukebox" => {
            let action = match lower.as_str() {
                "add" => JukeboxAction::Add,
                "next" => JukeboxAction::Next,
                "clear" => JukeboxAction::Clear,
                _ => return Err(pick("add, next or clear")),
            };
            if action != JukeboxAction::Clear && app.selected_video().is_none() {
                return Err("No video selected".into());
            }
            Command::Jukebox(action)
        }
        "channel" if arg.is_empty() => {
            if app.selected_video().is_none_or(|v| v.channel_id.is_none()) {
                return Err("No video selected, or it has no channel".into());
            }
            Command::Channel(None)
        }
        "channel" => match app
            .subscriptions
            .iter()
            .position(|c| c.title.to_lowercase() == lower)
        {
            Some(i) => Command::Channel(Some(i)),
            None => return Err("You don't subscribe to a channel of that name".into()),
        },
        "chapter" if arg.is_empty() => Command::Chapters,
        "chapter" => {
            let list = app.playing.as_ref().map_or(&[][..], |p| &p.chapters);
            let named = list.iter().position(|c| c.title.to_lowercase() == lower);
            let at = || {
                list.iter()
                    .position(|c| video::duration(c.start as u32) == arg)
            };
            match named.or_else(at) {
                Some(i) => Command::Chapter(i),
                None => return Err("No chapter of that name: Tab lists them".into()),
            }
        }
        "seek" => {
            let playing = app.playing.as_ref().ok_or("Nothing is playing")?;
            let Some(at) = chapters::parse_time(arg, playing.position, playing.duration) else {
                return Err(format!(
                    "“{}” isn't a time: 1:23, 90, +30, -1:00 or 50%",
                    video::one_line(arg, 40)
                ));
            };
            if let Some(length) = playing.duration.filter(|&length| at >= length) {
                return Err(format!(
                    "The video is only {} long",
                    video::duration(length as u32)
                ));
            }
            Command::GoTo(at)
        }
        "repeat" => match lower.as_str() {
            "" => Command::Repeat(None),
            "off" | "no" => Command::Repeat(Some(Repeat::Off)),
            "all" => Command::Repeat(Some(Repeat::All)),
            "one" | "1" => Command::Repeat(Some(Repeat::One)),
            _ => return Err(pick("off, all or one, or nothing for the next")),
        },
        "volume" => match lower.trim_end_matches('%').trim_end().parse::<u32>() {
            Ok(volume) if volume <= 100 => Command::Volume(volume),
            _ => return Err(pick("a number from 0 to 100")),
        },
        "search" => Command::Search(arg.to_string()),
        "import" => Command::Import(arg.to_string()),
        name => {
            if let Some(toggle) = Toggle::named(name) {
                let on = match lower.as_str() {
                    "" => None,
                    "on" | "yes" | "true" => Some(true),
                    "off" | "no" | "false" => Some(false),
                    _ => return Err(pick("on or off, or nothing to turn it over")),
                };
                return Ok(Command::Set(toggle, on));
            }
            match name {
                "play" => Command::Play,
                "listen" => Command::Listen,
                "pause" => Command::Pause,
                "stop" => Command::Stop,
                "next" => Command::Next,
                "prev-chapter" => Command::PreviousChapter,
                "next-chapter" => Command::NextChapter,
                "mix" => Command::Mix,
                "later" => Command::WatchLater,
                "remove" => Command::Remove,
                "subscribe" => Command::Subscribe,
                "undo" => Command::Undo,
                "browser" => Command::Browser,
                "copy" => Command::Copy,
                "back" => Command::Back,
                "refresh" => Command::Refresh,
                "keys" => Command::Keys,
                "settings" => Command::Settings,
                "info" => Command::Info,
                "mute" => Command::Mute,
                _ => Command::Quit,
            }
        }
    })
}

/// The completions of `line`: of the command, or of its argument once a
/// space follows the command.
fn complete(line: &str, app: &App, folders: &mut Folders) -> Completion {
    let (start, set) = command_start(line);
    let rest = &line[start..];
    let Some(end) = rest.find(char::is_whitespace) else {
        return Completion {
            from: start,
            candidates: commands(rest, app, set),
            title: if set { "Settings" } else { "Commands" }.into(),
        };
    };
    let after = &rest[end..];
    let from = start + end + after.len() - after.trim_start().len();
    match find(&rest[..end]) {
        Some(spec) => Completion {
            from,
            candidates: arguments(spec, &line[from..], app, folders),
            title: spec.about.into(),
        },
        None => Completion {
            from,
            ..Completion::default()
        },
    }
}

/// How well `query` (lower case) matches a command, best 0; `None` if not
/// at all. Its name first, then its other names, then what it does, then
/// its letters in order (`jbl` for jukebox-list).
fn command_rank(query: &str, spec: &Spec) -> Option<u8> {
    if query.is_empty() || spec.name == query || spec.aliases.contains(&query) {
        return Some(0);
    }
    if spec.name.starts_with(query) {
        return Some(1);
    }
    if spec.aliases.iter().any(|a| a.starts_with(query)) {
        return Some(2);
    }
    if spec.name.contains(query) {
        return Some(3);
    }
    if let Some(rank) = text_rank(query, spec.about) {
        return Some(4 + rank);
    }
    in_order(query, spec.name).then_some(6)
}

/// How well `query` (lower case) matches a value and what's said of it.
fn value_rank(query: &str, text: &str, about: &str) -> Option<u8> {
    let text = text.to_lowercase();
    if query.is_empty() || text == query {
        return Some(0);
    }
    if text.starts_with(query) {
        return Some(1);
    }
    if text.contains(query) {
        return Some(2);
    }
    if let Some(rank) = text_rank(query, about) {
        return Some(3 + rank);
    }
    in_order(query, &text).then_some(5)
}

/// 0 if a word of `text` starts with `query`, 1 if `text` holds it.
fn text_rank(query: &str, text: &str) -> Option<u8> {
    let text = text.to_lowercase();
    let word_start = text
        .match_indices(query)
        .any(|(i, _)| i == 0 || !text[..i].ends_with(char::is_alphanumeric));
    if word_start {
        Some(0)
    } else {
        text.contains(query).then_some(1)
    }
}

/// Whether `query`'s letters are in `text`, in order.
fn in_order(query: &str, text: &str) -> bool {
    let mut letters = text.chars();
    query.chars().all(|q| letters.any(|c| c == q))
}

/// The commands matching `word`, best first; with `set `, the settings.
fn commands(word: &str, app: &App, settings_only: bool) -> Vec<Candidate> {
    let query = word.to_lowercase();
    let mut ranked: Vec<(u8, usize, &Spec)> = COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            !settings_only
                || matches!(
                    s.arg,
                    Arg::OnOff | Arg::Height | Arg::Sound | Arg::Theme | Arg::Volume | Arg::Repeat
                )
        })
        .filter_map(|(i, s)| Some((command_rank(&query, s)?, i, s)))
        .collect();
    ranked.sort_by_key(|&(rank, i, _)| (rank, i));
    ranked
        .into_iter()
        .map(|(_, _, s)| Candidate {
            text: s.name.into(),
            hint: s.hint,
            about: s.about.into(),
            key: s.key,
            current: false,
            ready: needs_met(s.needs, app),
            takes_arg: s.arg != Arg::None,
        })
        .collect()
}

/// A value an argument can take.
fn value(text: impl Into<String>, about: impl Into<String>, current: bool) -> Candidate {
    Candidate {
        text: text.into(),
        hint: "",
        about: about.into(),
        key: "",
        current,
        ready: true,
        takes_arg: false,
    }
}

/// The values `spec`'s argument can take that match `arg`, best first.
fn arguments(spec: &Spec, arg: &str, app: &App, folders: &mut Folders) -> Vec<Candidate> {
    let settings = &app.settings;
    let playing = app.playing.as_ref();
    let chapter_now = playing.and_then(|p| chapters::current(&p.chapters, p.position));
    let values: Vec<Candidate> = match spec.arg {
        Arg::None => Vec::new(),
        Arg::File => return files(arg, folders),
        Arg::Height => HEIGHTS
            .iter()
            .enumerate()
            .map(|(i, &h)| {
                value(
                    h.to_string(),
                    settings::height_label(h),
                    settings.height_choice() == Some(i),
                )
            })
            .collect(),
        Arg::Sound => AudioQuality::ALL
            .iter()
            .map(|&q| {
                let about = match q {
                    AudioQuality::High => "About 130 kbps, the best without an account",
                    AudioQuality::Low => "About 70 kbps, to save data",
                };
                value(q.name(), about, settings.audio_quality == q)
            })
            .collect(),
        Arg::Theme => theme::names()
            .iter()
            .map(|(id, name)| value(*id, name.clone(), *id == settings.theme))
            .collect(),
        Arg::OnOff => {
            let on = Toggle::named(spec.name).is_some_and(|t| t.is_on(settings));
            vec![
                value("on", "Turn it on", on),
                value("off", "Turn it off", !on),
            ]
        }
        Arg::Repeat => Repeat::CHOICES
            .iter()
            .map(|&r| {
                let about = match r {
                    Repeat::Off => "Each video plays once",
                    Repeat::All => "The Jukebox or the list, from its first after its last",
                    Repeat::One => "The video playing, over and over",
                };
                value(r.name(), about, settings.repeat == r)
            })
            .collect(),
        Arg::Volume => [100, 75, 50, 25]
            .into_iter()
            .map(|v| value(v.to_string(), "", settings.volume() == v))
            .collect(),
        Arg::Place => [
            ("home", "Your subscriptions' newest videos", true),
            ("shorts", "Your subscriptions' Shorts", settings.shorts),
            ("search", "Search YouTube", true),
            ("jukebox", "The videos you put in to play", true),
            ("later", "Watch later", true),
            ("history", "What you watched here", true),
        ]
        .into_iter()
        .filter(|(_, _, shown)| *shown)
        .map(|(name, about, _)| value(name, about, false))
        .collect(),
        Arg::Jukebox => vec![
            value(
                "add",
                "Put the selected video in the Jukebox, last (e)",
                false,
            ),
            value("next", "Put it right after the one playing (E)", false),
            value(
                "clear",
                "Take every video off it, after you press y (C)",
                false,
            ),
        ],
        Arg::Channel => app
            .subscriptions
            .iter()
            .filter(|c| !c.title.is_empty())
            .map(|c| {
                let about = if c.gone { "Gone from YouTube" } else { "" };
                let open = matches!(&app.view, View::Channel(id, _) if *id == c.id);
                value(c.title.clone(), about, open)
            })
            .collect(),
        Arg::Chapter => playing
            .map(|p| &p.chapters[..])
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let at = video::duration(c.start as u32);
                let (text, about) = if c.title.is_empty() {
                    (at, String::new())
                } else {
                    (c.title.clone(), at)
                };
                value(text, about, chapter_now == Some(i))
            })
            .collect(),
        Arg::Time => playing
            .map(|p| &p.chapters[..])
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, c)| {
                value(
                    video::duration(c.start as u32),
                    c.title.clone(),
                    chapter_now == Some(i),
                )
            })
            .collect(),
        Arg::Query => {
            let last = app.last_query();
            if last.is_empty() {
                Vec::new()
            } else {
                vec![value(last, "Your last search", false)]
            }
        }
    };
    let query = arg.trim().to_lowercase();
    let mut ranked: Vec<(u8, usize, Candidate)> = values
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| Some((value_rank(&query, &c.text, &c.about)?, i, c)))
        .collect();
    ranked.sort_by_key(|&(rank, i, _)| (rank, i));
    ranked.into_iter().map(|(_, _, c)| c).collect()
}

/// File names completing `arg`: the folders and `.csv` files in the folder
/// it names (`~/` for your home folder), the ones starting with what's
/// after its last `/` first. A folder that may be on another computer
/// isn't looked in (on Windows, that would hand it your login hash).
fn files(arg: &str, folders: &mut Folders) -> Vec<Candidate> {
    let cut = arg.rfind(std::path::is_separator).map_or(0, |i| i + 1);
    let (typed_dir, name) = arg.split_at(cut);
    let mut found = Vec::new();
    if arg.is_empty() || arg == "~" {
        found.push(value("~/", "Your home folder", false));
    }
    let dir = match typed_dir.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => return found,
        },
        None if typed_dir.is_empty() => PathBuf::from("."),
        None => PathBuf::from(typed_dir),
    };
    if config::maybe_remote(&dir) {
        return found;
    }
    let entries = folders.entry(dir).or_insert_with_key(|dir| list(dir));
    let query = name.to_lowercase();
    let mut ranked: Vec<(u8, &String, bool)> = entries
        .iter()
        // Hidden ones only when asked for.
        .filter(|(n, _)| query.starts_with('.') || !n.starts_with('.'))
        .filter_map(|(n, is_dir)| {
            let lower = n.to_lowercase();
            let rank = if lower.starts_with(&query) {
                0
            } else if lower.contains(&query) {
                1
            } else {
                return None;
            };
            Some((rank, n, *is_dir))
        })
        .collect();
    ranked.sort_by_key(|a| (a.0, a.1.to_lowercase()));
    found.extend(ranked.into_iter().take(MAX_FILES).map(|(_, n, is_dir)| {
        let slash = if is_dir { "/" } else { "" };
        let about = if is_dir { "Folder" } else { "File" };
        value(format!("{typed_dir}{n}{slash}"), about, false)
    }));
    found
}

/// A folder's folders and the files Takeout writes (`.csv`, `.json`). A
/// name with characters that don't show (a control character, a bidi
/// override) is left out: it would show, or complete, as something it
/// isn't. Links aren't followed (a link into a network folder would connect
/// to it): one is listed only if it's named like one of those files.
fn list(dir: &Path) -> Vec<(String, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .take(MAX_READ)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name.chars().any(|c| c.is_control() || text::is_hidden(c)) {
                return None;
            }
            let is_dir = entry.file_type().ok()?.is_dir();
            let lower = name.to_lowercase();
            let takeout = lower.ends_with(".csv") || lower.ends_with(".json");
            (is_dir || takeout).then_some((name, is_dir))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{app, with_feed};

    fn texts(c: &Completion) -> Vec<&str> {
        c.candidates.iter().map(|c| c.text.as_str()).collect()
    }

    fn completed(line: &str, app: &App) -> Completion {
        complete(line, app, &mut Folders::new())
    }

    #[test]
    fn every_command_reads_and_has_a_name_of_its_own() {
        let mut names = std::collections::HashSet::new();
        for spec in COMMANDS {
            assert!(names.insert(spec.name), "{} twice", spec.name);
            for alias in spec.aliases {
                assert!(names.insert(alias), "{alias} twice");
            }
            assert!(!spec.about.is_empty());
            assert_eq!(spec.arg == Arg::None, spec.hint.is_empty(), "{}", spec.name);
            assert_eq!(
                find(&spec.name.to_uppercase()).map(|s| s.name),
                Some(spec.name)
            );
        }
    }

    #[tokio::test]
    async fn commands_complete_by_name_then_by_what_they_do() {
        let app = app();
        let all = completed("", &app);
        assert_eq!(
            all.candidates.len(),
            COMMANDS.len(),
            "an empty line lists them all"
        );
        assert_eq!(
            all.candidates[0].text, "quality",
            "the quick settings first"
        );
        assert_eq!(texts(&completed("qu", &app))[..2], ["quality", "quit"]);
        assert_eq!(texts(&completed("q", &app))[0], "quit", "vim's :q");
        assert_eq!(texts(&completed("audio", &app))[0], "sound", "another name");
        // By what it does.
        assert_eq!(texts(&completed("4k", &app)), ["quality"]);
        assert!(texts(&completed("resolution", &app)).is_empty());
        assert_eq!(
            texts(&completed("jbl", &app))[0],
            "jukebox-list",
            "in order"
        );
        let settings = completed("set ", &app);
        assert!(settings.candidates.iter().all(|c| c.text != "play"));
        assert_eq!(settings.title, "Settings");
        assert!(completed("  set  auto", &app).candidates[0].text == "autoplay");
        assert!(completed("set rep", &app).candidates[0].text == "repeat");
        let repeat = completed("repeat ", &app);
        assert_eq!(texts(&repeat), ["off", "all", "one"]);
        assert!(repeat.candidates[0].current, "off unless asked");
        // A command that needs a video playing can't run now.
        let pause = &completed("pause", &app).candidates[0];
        assert!(!pause.ready);
        assert!(completed("quality", &app).candidates[0].takes_arg);
    }

    #[tokio::test]
    async fn every_argument_completes() {
        let mut app = app();
        with_feed(&mut app, 2);
        app.settings.max_height = 720;
        let quality = completed("quality ", &app);
        assert_eq!(quality.from, "quality ".len());
        assert_eq!(
            texts(&quality),
            ["2160", "1440", "1080", "720", "480", "360"]
        );
        assert!(quality.candidates[3].current, "the one in use");
        assert_eq!(texts(&completed("quality 7", &app)), ["720"]);
        assert_eq!(
            texts(&completed("quality 4k", &app)),
            ["2160"],
            "by what it says"
        );
        assert_eq!(texts(&completed("sound l", &app)), ["low"]);
        assert_eq!(texts(&completed("theme dr", &app)), ["dracula"]);
        let on_off = completed("autoplay ", &app);
        assert_eq!(texts(&on_off), ["on", "off"]);
        assert!(on_off.candidates[0].current, "autoplay is on");
        assert_eq!(texts(&completed("go h", &app))[..2], ["home", "history"]);
        app.settings.shorts = false;
        assert!(!texts(&completed("go ", &app)).contains(&"shorts"));
        assert_eq!(
            texts(&completed("jukebox ", &app)),
            ["add", "next", "clear"]
        );
        assert_eq!(texts(&completed("channel bek", &app)), ["BekBrace"]);
        assert!(
            completed("search ", &app).candidates.is_empty(),
            "no search yet"
        );
        assert!(
            completed("chapter ", &app).candidates.is_empty(),
            "nothing playing"
        );
        assert!(
            completed("play ", &app).candidates.is_empty(),
            "takes nothing"
        );
        assert!(completed("nonsense ", &app).candidates.is_empty());

        app.show_playing(app.videos[0].clone(), 75.0, 600.0, false);
        app.playing.as_mut().unwrap().chapters = [(0.0, "Intro"), (60.0, "Middle"), (180.0, "")]
            .map(|(start, title)| crate::chapters::Chapter {
                start,
                title: title.into(),
            })
            .into();
        let chapter = completed("chapter ", &app);
        assert_eq!(
            texts(&chapter),
            ["Intro", "Middle", "3:00"],
            "untitled: its time"
        );
        assert!(chapter.candidates[1].current, "the one playing");
        assert_eq!(texts(&completed("chapter mid", &app)), ["Middle"]);
        assert_eq!(texts(&completed("seek 1", &app)), ["1:00"]);
        assert_eq!(
            texts(&completed("seek intro", &app)),
            ["0:00"],
            "by its title"
        );
    }

    #[test]
    fn file_names_complete_from_the_folder_typed() {
        let dir = std::env::temp_dir().join(format!("tuitube-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Takeout")).unwrap();
        for name in ["subscriptions.csv", "notes.txt", ".hidden.csv", "Subs2.CSV"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        // Windows allows no such name.
        if cfg!(unix) {
            std::fs::write(dir.join("bad\u{1b}[2J.csv"), "").unwrap();
        }
        let typed = format!("{}/", dir.display());
        let mut folders = Folders::new();
        let names = |arg: &str, folders: &mut Folders| -> Vec<String> {
            files(arg, folders)
                .into_iter()
                .map(|c| c.text.trim_start_matches(&typed).to_string())
                .collect()
        };
        assert_eq!(
            names(&typed, &mut folders),
            ["Subs2.CSV", "subscriptions.csv", "Takeout/"],
            "folders and .csv files, nothing hidden, nothing that doesn't show as it is"
        );
        assert_eq!(
            names(&format!("{typed}sub"), &mut folders),
            ["Subs2.CSV", "subscriptions.csv"]
        );
        assert_eq!(names(&format!("{typed}take"), &mut folders), ["Takeout/"]);
        assert_eq!(names(&format!("{typed}."), &mut folders)[0], ".hidden.csv");
        // A folder is read once: what's added later isn't seen this time.
        std::fs::write(dir.join("later.csv"), "").unwrap();
        assert!(names(&format!("{typed}lat"), &mut folders).is_empty());
        assert_eq!(files("", &mut folders)[0].text, "~/");
        if cfg!(windows) {
            assert!(files("\\\\server\\share\\", &mut folders).is_empty());
        }
        assert!(
            files("//server/share/", &mut folders).is_empty(),
            "not looked in"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn lines_read_as_commands_or_say_whats_wrong() {
        let mut app = app();
        with_feed(&mut app, 2);
        let p = |app: &App, line: &str| parse(line, app);
        assert_eq!(p(&app, "quality 720"), Ok(Command::Quality(720)));
        assert_eq!(p(&app, "repeat"), Ok(Command::Repeat(None)));
        assert_eq!(p(&app, "loop One"), Ok(Command::Repeat(Some(Repeat::One))));
        assert_eq!(
            p(&app, "set norepeat"),
            Ok(Command::Repeat(Some(Repeat::Off)))
        );
        assert!(
            p(&app, "repeat twice")
                .unwrap_err()
                .contains("off, all or one")
        );
        assert_eq!(p(&app, "QUALITY 1080p"), Ok(Command::Quality(1080)));
        assert_eq!(p(&app, "video 4K"), Ok(Command::Quality(2160)));
        assert!(p(&app, "quality 999").unwrap_err().contains("2160, 1440"));
        assert!(
            p(&app, "quality")
                .unwrap_err()
                .starts_with("quality needs <height>")
        );
        assert_eq!(p(&app, "sound low"), Ok(Command::Sound(AudioQuality::Low)));
        assert_eq!(p(&app, "theme Nord"), Ok(Command::Theme("nord")));
        assert_eq!(
            p(&app, "autoplay"),
            Ok(Command::Set(Toggle::Autoplay, None))
        );
        assert_eq!(
            p(&app, "set autoplay off"),
            Ok(Command::Set(Toggle::Autoplay, Some(false)))
        );
        assert_eq!(
            p(&app, "set noautoplay"),
            Ok(Command::Set(Toggle::Autoplay, Some(false)))
        );
        assert_eq!(
            p(&app, "sponsorblock yes"),
            Ok(Command::Set(Toggle::SponsorBlock, Some(true)))
        );
        assert_eq!(
            p(&app, "set nodearrow"),
            Ok(Command::Set(Toggle::DeArrow, Some(false)))
        );
        assert_eq!(
            p(&app, "audio-only on"),
            Ok(Command::Set(Toggle::AudioOnly, Some(true)))
        );
        assert_eq!(p(&app, "music"), Ok(Command::Set(Toggle::AudioOnly, None)));
        assert_eq!(
            p(&app, "set novisualizer"),
            Ok(Command::Set(Toggle::Visualizer, Some(false)))
        );
        assert_eq!(p(&app, "volume 35"), Ok(Command::Volume(35)));
        assert_eq!(p(&app, "vol 0%"), Ok(Command::Volume(0)));
        assert!(p(&app, "volume 150").unwrap_err().contains("0 to 100"));
        assert!(p(&app, "volume loud").unwrap_err().contains("0 to 100"));
        assert_eq!(p(&app, "mute"), Ok(Command::Mute));
        assert_eq!(p(&app, "q"), Ok(Command::Quit));
        assert_eq!(p(&app, "  quit  "), Ok(Command::Quit));
        assert_eq!(p(&app, "go later"), Ok(Command::Go(Entry::WatchLater)));
        assert_eq!(p(&app, "channel bekbrace"), Ok(Command::Channel(Some(0))));
        assert_eq!(
            p(&app, "channel"),
            Ok(Command::Channel(None)),
            "the selected video's"
        );
        assert_eq!(
            p(&app, "jukebox add"),
            Ok(Command::Jukebox(JukeboxAction::Add))
        );
        assert_eq!(
            p(&app, "search rust lang"),
            Ok(Command::Search("rust lang".into()))
        );
        assert_eq!(p(&app, "search"), Ok(Command::Search(String::new())));
        assert_eq!(
            p(&app, "import ~/subs.csv"),
            Ok(Command::Import("~/subs.csv".into()))
        );
        assert_eq!(p(&app, "play"), Ok(Command::Play));
        assert!(
            p(&app, "play now")
                .unwrap_err()
                .contains("nothing after it")
        );
        assert!(p(&app, "frobnicate").unwrap_err().starts_with("No command"));
        assert_eq!(p(&app, "pause"), Err("Nothing is playing".into()));
        assert_eq!(p(&app, "1:23"), Err("Nothing is playing".into()));

        app.show_playing(app.videos[0].clone(), 75.0, 600.0, false);
        app.playing.as_mut().unwrap().chapters = vec![crate::chapters::Chapter {
            start: 60.0,
            title: "Middle".into(),
        }];
        assert_eq!(p(&app, "1:23"), Ok(Command::GoTo(83.0)), "a time alone");
        assert_eq!(p(&app, "+30"), Ok(Command::GoTo(105.0)));
        assert_eq!(p(&app, "seek 50%"), Ok(Command::GoTo(300.0)));
        assert_eq!(
            p(&app, "seek 20:00"),
            Err("The video is only 10:00 long".into())
        );
        assert!(p(&app, "seek soon").unwrap_err().contains("isn't a time"));
        assert_eq!(p(&app, "chapter middle"), Ok(Command::Chapter(0)));
        assert_eq!(
            p(&app, "chapter 1:00"),
            Ok(Command::Chapter(0)),
            "or its time"
        );
        assert_eq!(p(&app, "chapter"), Ok(Command::Chapters), "the list");
        assert_eq!(p(&app, "pause"), Ok(Command::Pause));
        app.videos.clear();
        assert_eq!(p(&app, "play"), Err("No video selected".into()));
    }

    #[tokio::test]
    async fn tab_puts_each_completion_on_the_line_in_turn() {
        let app = app();
        let mut line = CommandLine {
            text: "qu".into(),
            ..CommandLine::default()
        };
        line.complete(&app);
        line.step(false);
        assert_eq!(
            line.text, "quality ",
            "a space after a command that takes more"
        );
        line.step(false);
        assert_eq!(line.text, "quit");
        line.step(true);
        assert_eq!(line.text, "quality ");
        line.step(true);
        assert_eq!(
            line.selected,
            Some(line.completion.candidates.len() - 1),
            "round"
        );
        assert_eq!(line.source(), "qu", "the list is still of what was typed");
        line.edited();
        line.text = "quality ".into();
        line.complete(&app);
        line.step(false);
        assert_eq!(line.text, "quality 2160");
    }
}
