//! What Google Takeout exports of YouTube, recognized by what's in a file,
//! since its folder and file names (and headers) are in the account's
//! language:
//! - subscriptions (`subscriptions/subscriptions.csv`): rows of channel id,
//!   channel URL, channel title;
//! - each playlist (`playlists/<name>-videos.csv`): rows of video id and
//!   when it was added, Watch later among them;
//! - watch history (`history/watch-history.json`): what was watched, when.
//!
//! Rows are read by position, and a row counts only if its id checks out. A
//! folder (the unzipped Takeout) is searched for them, a few levels down,
//! without following links. Other files Takeout writes (your own channel,
//! your uploads, comments) have other shapes and are passed over.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config;
use crate::ids::{ChannelId, PlaylistId, VideoId};
use crate::links::{self, Link};
use crate::store::Store;
use crate::video::{MAX_CHANNEL, MAX_TITLE, Video, one_line};

/// The biggest CSV read: a Takeout file of 5,000 subscriptions is about
/// 500 KB, a playlist of 5,000 videos under 300 KB.
const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// The biggest watch history read: about 500 bytes a video, so years of it.
const MAX_HISTORY_BYTES: u64 = 256 * 1024 * 1024;
/// The most channels imported: YouTube itself stops at 2,000 subscriptions.
pub const MAX_CHANNELS: usize = 5_000;
/// The most videos read from a playlist: YouTube's stop at 5,000.
const MAX_PLAYLIST: usize = 5_000;
/// The most videos of history imported, the newest.
const MAX_HISTORY: usize = 50_000;
/// How deep a folder is searched: `Takeout/YouTube and YouTube Music/
/// playlists/x.csv` is three down from the folder holding `Takeout`.
const MAX_DEPTH: usize = 4;
/// The most folder entries looked at.
const MAX_ENTRIES: usize = 2_000;

/// Watch later's name, as Takeout names its file in some of the languages
/// YouTube speaks. A Watch later not in this list is imported as a
/// playlist of that name.
const WATCH_LATER: [&str; 16] = [
    "watch later",
    "später ansehen",
    "à regarder plus tard",
    "ver más tarde",
    "guarda più tardi",
    "assistir mais tarde",
    "ver mais tarde",
    "later bekijken",
    "do obejrzenia",
    "смотреть позже",
    "後で見る",
    "나중에 볼 동영상",
    "稍后观看",
    "稍後觀看",
    "daha sonra izle",
    "xem sau",
];

/// A playlist's videos, each with when it was added to it.
pub type Added = Vec<(VideoId, Option<i64>)>;

/// What a Takeout file holds.
#[derive(Debug, PartialEq)]
pub enum Takeout {
    /// Channels, with their names.
    Subscriptions(Vec<(ChannelId, String)>),
    /// A playlist's name and its videos, with when each was added.
    Playlist {
        name: String,
        watch_later: bool,
        videos: Added,
    },
    /// Videos watched, the newest first, with as much as the history says
    /// of each (its title may be only its link: [`Video::needs_details`]).
    History(Vec<(Video, i64)>),
    /// Watch history as a web page, which has no times tuitube can read.
    HistoryPage,
}

/// What's at `path`: a Takeout file, or each one in a folder.
pub fn find(path: &Path) -> Result<Vec<Takeout>> {
    local(path)?;
    if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        let found = read_folder(path)?;
        if found.is_empty() {
            bail!(
                "{} has no Takeout files tuitube reads: subscriptions.csv, playlists' \
                 <name>-videos.csv, watch-history.json",
                config::shown(path)
            );
        }
        return Ok(found.into_iter().map(|(_, takeout)| takeout).collect());
    }
    Ok(vec![read(path)?])
}

/// What a file at `path` holds.
pub fn read(path: &Path) -> Result<Takeout> {
    let shown = config::shown(path);
    let json = extension_is(path, "json");
    // A history page is too big to read for nothing: its start says what
    // it is.
    if extension_is(path, "html") || extension_is(path, "htm") {
        let start = read_file(path, MAX_BYTES, true)?;
        if memchr_str(&start, b"youtube.com/watch?v=") {
            return Ok(Takeout::HistoryPage);
        }
        bail!("{shown} isn't a YouTube watch history");
    }
    let max = if json { MAX_HISTORY_BYTES } else { MAX_BYTES };
    let data = read_file(path, max, false)?;
    if json || data.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
        let history = history(&data);
        if history.is_empty() {
            bail!(
                "{shown} lists no videos watched: it should be watch-history.json from Google Takeout"
            );
        }
        return Ok(Takeout::History(history));
    }
    let channels = subscriptions(&data);
    if channels.len() > MAX_CHANNELS {
        bail!("{shown} lists more than {MAX_CHANNELS} channels");
    }
    if !channels.is_empty() {
        return Ok(Takeout::Subscriptions(channels));
    }
    if let Some((name, videos)) = playlist(&data) {
        let name = name.unwrap_or_else(|| playlist_name(path));
        let watch_later = WATCH_LATER.contains(&name.to_lowercase().as_str());
        return Ok(Takeout::Playlist {
            name,
            watch_later,
            videos,
        });
    }
    bail!(
        "{shown} isn't a Takeout file tuitube reads: subscriptions.csv, a playlist's \
         <name>-videos.csv, or watch-history.json"
    )
}

/// Every Takeout file under `dir`, each with where it is.
pub fn read_folder(dir: &Path) -> Result<Vec<(PathBuf, Takeout)>> {
    local(dir)?;
    let mut found = Vec::new();
    let mut folders = vec![(dir.to_path_buf(), 0)];
    let mut looked = 0;
    while let Some((folder, depth)) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            looked += 1;
            if looked > MAX_ENTRIES {
                return Ok(found);
            }
            // Links aren't followed: what the link's type says, not its
            // target's.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() && depth < MAX_DEPTH {
                folders.push((path, depth + 1));
            } else if kind.is_file()
                && ["csv", "json", "html"]
                    .iter()
                    .any(|e| extension_is(&path, e))
                && let Ok(takeout) = read(&path)
            {
                found.push((path, takeout));
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

/// What was imported, to say so.
#[derive(Debug, Default, PartialEq)]
pub struct Imported {
    pub channels: usize,
    pub new_channels: usize,
    pub watch_later: usize,
    /// Playlists, with how many videos each.
    pub playlists: Vec<(String, usize)>,
    pub history: usize,
    /// Watch history was there but not imported: kept off in the settings.
    pub history_off: bool,
    /// Watch history was there as a web page.
    pub history_page: bool,
}

impl Imported {
    /// What it says: "Imported 12 subscriptions (3 new), 40 videos to Watch
    /// later and 2 playlists".
    pub fn summary(&self) -> String {
        let count = |n: usize, one: &str| {
            let s = if n == 1 { "" } else { "s" };
            format!("{n} {one}{s}")
        };
        let mut parts = Vec::new();
        if self.channels > 0 {
            parts.push(format!(
                "{} ({} new)",
                count(self.channels, "subscription"),
                self.new_channels
            ));
        }
        if self.watch_later > 0 {
            parts.push(format!(
                "{} to Watch later",
                count(self.watch_later, "video")
            ));
        }
        match self.playlists.as_slice() {
            [] => {}
            [(name, n)] => parts.push(format!("the playlist “{name}” ({})", count(*n, "video"))),
            lists => parts.push(count(lists.len(), "playlist")),
        }
        if self.history > 0 {
            parts.push(format!("{} of history", count(self.history, "video")));
        }
        let mut said = match parts.as_slice() {
            [] => "Nothing new to import".to_string(),
            [one] => format!("Imported {one}"),
            [rest @ .., last] => format!("Imported {} and {last}", rest.join(", ")),
        };
        if self.history_off {
            said.push_str(". History is off, so it wasn't imported");
        } else if self.history_page {
            said.push_str(
                ". For your history, export it again as JSON (History: “Multiple formats”)",
            );
        }
        said
    }
}

/// Saves what was found, in one transaction each: subscriptions, Watch
/// later, playlists, and (if `history` is on) watch history.
pub fn save(store: &mut Store, found: Vec<Takeout>, history: bool) -> Result<Imported> {
    let mut imported = Imported::default();
    for takeout in found {
        match takeout {
            Takeout::Subscriptions(channels) => {
                imported.new_channels += store.subscribe_all(&channels)?;
                imported.channels += channels.len();
            }
            Takeout::Playlist {
                watch_later: true,
                videos,
                ..
            } => imported.watch_later += store.import_watch_later(&videos)?,
            Takeout::Playlist { name, videos, .. } => {
                let added = store.import_playlist(&name, &videos)?;
                imported.playlists.push((name, added));
            }
            Takeout::History(_) if !history => imported.history_off = true,
            Takeout::History(watched) => imported.history += store.import_history(&watched)?,
            Takeout::HistoryPage => imported.history_page = true,
        }
    }
    Ok(imported)
}

fn extension_is(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

/// Refuses a path that may be on another computer (`\\server\share`).
fn local(path: &Path) -> Result<()> {
    if config::maybe_remote(path) {
        bail!(
            "{} is on another computer, or may be (on Windows, use a path with a drive letter)",
            config::shown(path)
        );
    }
    Ok(())
}

fn memchr_str(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The file's bytes, at most `max` of them: past that, an error, or (if
/// `start`) only its start.
fn read_file(path: &Path, max: u64, start: bool) -> Result<Vec<u8>> {
    let shown = config::shown(path);
    local(path)?;
    // Opened without waiting (a named pipe would block the app), and read
    // only if what was opened is a plain file: a link to /dev/zero or a
    // terminal reports a size of 0 and never ends.
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let file = options
        .open(path)
        .with_context(|| format!("cannot read {shown}"))?;
    if !file.metadata()?.is_file() {
        bail!("{shown} isn't a file");
    }
    let mut data = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut data)
        .with_context(|| format!("cannot read {shown}"))?;
    if data.len() as u64 > max {
        if !start {
            bail!("{shown} is too big to be from Takeout");
        }
        data.truncate(max as usize);
    }
    Ok(data)
}

fn csv_rows(data: &[u8]) -> impl Iterator<Item = csv::StringRecord> + '_ {
    csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(data)
        .into_records()
        .flatten()
}

/// The channels in a subscriptions file. A row is one if its second column
/// is the channel's URL, or it has three columns starting with a channel id:
/// your own channel's file (`channel.csv`) has more.
fn subscriptions(data: &[u8]) -> Vec<(ChannelId, String)> {
    let mut channels: Vec<(ChannelId, String)> = Vec::new();
    let mut seen = HashSet::new();
    for record in csv_rows(data) {
        let field = |i| record.get(i).unwrap_or("").trim();
        let by_url = ChannelId::from_url(field(1));
        let by_id = ChannelId::parse(field(0)).filter(|_| record.len() <= 3);
        let Some(id) = by_url.or(by_id) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        channels.push((id, one_line(field(2), MAX_CHANNEL)));
        // One over the limit is enough to say there are too many.
        if channels.len() > MAX_CHANNELS {
            break;
        }
    }
    channels
}

/// A playlist file's videos, each once, and its name if the file says it.
/// Rows of a video id and when it was added: your uploads' file
/// (`videos.csv`) has more columns. Older exports start with the
/// playlist's own row (its id, …, its title) and a blank line.
fn playlist(data: &[u8]) -> Option<(Option<String>, Added)> {
    let mut name = None;
    let mut videos = Vec::new();
    let mut seen = HashSet::new();
    for (i, record) in csv_rows(data).enumerate() {
        let field = |i| record.get(i).unwrap_or("").trim();
        if i <= 1 && record.len() >= 5 && PlaylistId::parse(field(0)).is_some() {
            name = Some(one_line(field(4), MAX_TITLE)).filter(|n| !n.is_empty());
            continue;
        }
        let Some(id) = VideoId::parse(field(0)).filter(|_| record.len() <= 2) else {
            continue;
        };
        if seen.insert(id.clone()) {
            videos.push((id, time(field(1))));
        }
        if videos.len() >= MAX_PLAYLIST {
            break;
        }
    }
    (!videos.is_empty()).then_some((name, videos))
}

/// A playlist's name from its file's: `Road trip-videos.csv` is "Road
/// trip" (the `-videos` is in the account's language).
fn playlist_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match stem.rsplit_once('-') {
        Some((name, suffix)) if !name.is_empty() && suffix.chars().all(char::is_alphabetic) => name,
        _ => &stem,
    };
    let name = one_line(name, MAX_TITLE);
    if name.is_empty() {
        "Imported playlist".into()
    } else {
        name
    }
}

/// A Takeout time: `2024-05-01T12:34:56+00:00`, or with `Z`.
fn time(text: &str) -> Option<i64> {
    let at = chrono::DateTime::parse_from_rfc3339(text).ok()?.timestamp();
    (0..4_102_444_800).contains(&at).then_some(at)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Watch {
    header: String,
    title: String,
    #[serde(rename = "titleUrl")]
    title_url: String,
    #[serde(deserialize_with = "crate::ytdlp::lenient")]
    subtitles: Vec<Subtitle>,
    #[serde(deserialize_with = "crate::ytdlp::lenient")]
    details: Vec<serde::de::IgnoredAny>,
    time: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Subtitle {
    name: String,
    url: String,
}

/// The videos in a watch history, each once (when it was last watched),
/// the newest first. Ads (they have `details`) and YouTube Music are left
/// out, and so is anything not a video link. Read an entry at a time, never
/// as one tree of the whole file (years of history is hundreds of MB), and
/// only up to `MAX_HISTORY` videos: the rest is skipped over.
fn history(data: &[u8]) -> Vec<(Video, i64)> {
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = Vec<(Video, i64)>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a list of what was watched")
        }

        fn visit_seq<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut seen = HashSet::new();
            let mut watched = Vec::new();
            while watched.len() < MAX_HISTORY {
                let Some(entry) = entries.next_element::<serde_json::Value>()? else {
                    return Ok(watched);
                };
                // One that doesn't fit is passed over, as `lenient` does.
                if let Ok(entry) = serde_json::from_value(entry)
                    && let Some(found) = watch(entry, &mut seen)
                {
                    watched.push(found);
                }
            }
            while entries.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(watched)
        }
    }
    let mut json = serde_json::Deserializer::from_slice(data);
    serde::Deserializer::deserialize_seq(&mut json, Entries)
        .and_then(|watched| json.end().map(|()| watched))
        .unwrap_or_default()
}

/// The video an entry of a watch history says was watched, and when: none
/// for an ad, YouTube Music, anything not a video link, or a video `seen`
/// already (watched again later).
fn watch(entry: Watch, seen: &mut HashSet<VideoId>) -> Option<(Video, i64)> {
    if entry.header != "YouTube" || !entry.details.is_empty() {
        return None;
    }
    let (Some(Link::Video { id, short, .. }), Some(at)) =
        (links::parse(&entry.title_url), time(&entry.time))
    else {
        return None;
    };
    if !seen.insert(id.clone()) {
        return None;
    }
    let channel = entry.subtitles.first();
    // "Watched <title>", the verb in the account's language: only the
    // English one is known, so another's title is looked up later. A
    // removed video's "title" is its link.
    let title = entry
        .title
        .strip_prefix("Watched ")
        .map(|t| one_line(t, MAX_TITLE))
        .filter(|t| !t.is_empty() && !t.starts_with("https://"));
    let mut video = Video::placeholder(id, short);
    if let Some(title) = title {
        video.title = title;
    }
    if let Some(channel) = channel {
        video.channel_id = ChannelId::from_url(&channel.url);
        video.channel = one_line(&channel.name, MAX_CHANNEL);
    }
    Some((video, at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_read_by_position_whatever_the_headers_language() {
        let csv = "ID des chaînes,URL des chaînes,Titres des chaînes\n\
                   UC7EVSn5inapL20oPSwAwEUg,http://www.youtube.com/channel/UC7EVSn5inapL20oPSwAwEUg,BekBrace\n\
                   UCsBjURrPoezykLs9EqgamOA,http://www.youtube.com/channel/UCsBjURrPoezykLs9EqgamOA,\"Fireship, the \"\"real\"\" one\"\n\
                   \n\
                   not-an-id,https://evil.example/,Evil\n\
                   UC7EVSn5inapL20oPSwAwEUg,http://www.youtube.com/channel/UC7EVSn5inapL20oPSwAwEUg,Again\n";
        let channels = subscriptions(csv.as_bytes());
        let names: Vec<_> = channels.iter().map(|(_, name)| name.as_str()).collect();
        assert_eq!(names, ["BekBrace", "Fireship, the \"real\" one"]);
    }

    #[test]
    fn names_are_cleaned_and_kept_to_one_line() {
        let csv = "UC7EVSn5inapL20oPSwAwEUg,,\"Bek\u{1b}[31m\nBrace\u{202E}\"\n";
        assert_eq!(subscriptions(csv.as_bytes())[0].1, "Bek[31m Brace");
    }

    #[test]
    fn your_own_channel_and_uploads_arent_subscriptions_or_a_playlist() {
        let channel = "Channel ID,Channel Title (Original),Channel Visibility,Channel Country\n\
                       UC7EVSn5inapL20oPSwAwEUg,Me,Public,FR\n";
        assert!(subscriptions(channel.as_bytes()).is_empty());
        assert!(playlist(channel.as_bytes()).is_none());
        let uploads = "Video ID,Approx Duration (ms),Video Audio Language,Video Title\n\
                       dQw4w9WgXcQ,212000,en,Mine\n";
        assert!(playlist(uploads.as_bytes()).is_none());
    }

    #[test]
    fn a_playlists_videos_in_either_kind_of_export() {
        let new = "Video ID,Playlist Video Creation Timestamp\n\
                   dQw4w9WgXcQ,2024-03-01T12:00:00+00:00\n\
                   9bZkp7q19f0,not a time\n\
                   dQw4w9WgXcQ,2024-03-02T12:00:00+00:00\n\
                   -oops,2024-03-01T12:00:00+00:00\n";
        let (name, videos) = playlist(new.as_bytes()).unwrap();
        assert_eq!(name, None);
        let ids: Vec<_> = videos.iter().map(|(id, at)| (id.as_str(), *at)).collect();
        assert_eq!(
            ids,
            [("dQw4w9WgXcQ", Some(1_709_294_400)), ("9bZkp7q19f0", None)]
        );

        let old = "Playlist Id,Channel Id,Time Created,Time Updated,Title,Description,Visibility\n\
                   PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf,UC7EVSn5inapL20oPSwAwEUg,2020-01-01,2020-01-02,Road trip,,Private\n\
                   \n\
                   Video Id,Time Added\n\
                   dQw4w9WgXcQ,2020-01-01 10:00:00 UTC\n";
        let (name, videos) = playlist(old.as_bytes()).unwrap();
        assert_eq!(name.as_deref(), Some("Road trip"));
        assert_eq!(videos.len(), 1);
    }

    #[test]
    fn a_playlist_is_named_after_its_file_and_watch_later_known_in_other_languages() {
        assert_eq!(
            playlist_name(Path::new("/t/Road trip-videos.csv")),
            "Road trip"
        );
        assert_eq!(playlist_name(Path::new("/t/Lo-fi-Videos.csv")), "Lo-fi");
        assert_eq!(playlist_name(Path::new("/t/plain.csv")), "plain");
        let dir = temp("names");
        let path = dir.join("Später ansehen-Videos.csv");
        std::fs::write(&path, "dQw4w9WgXcQ,2024-03-01T12:00:00+00:00\n").unwrap();
        let Takeout::Playlist { watch_later, .. } = read(&path).unwrap() else {
            panic!("a playlist");
        };
        assert!(watch_later);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn watch_history_keeps_videos_not_ads_or_music_each_once() {
        let json = r#"[
          {"header":"YouTube","title":"Watched Rust in 100 Seconds","titleUrl":"https://www.youtube.com/watch?v=5C_HPTJg5ek",
           "subtitles":[{"name":"Fireship","url":"https://www.youtube.com/channel/UCsBjURrPoezykLs9EqgamOA"}],
           "time":"2024-05-02T10:00:00.123Z","products":["YouTube"],"activityControls":["YouTube watch history"]},
          {"header":"YouTube","title":"Watched An ad","titleUrl":"https://www.youtube.com/watch?v=aaaaaaaaaaa",
           "details":[{"name":"From Google Ads"}],"time":"2024-05-02T09:00:00Z"},
          {"header":"YouTube Music","title":"Watched A song","titleUrl":"https://music.youtube.com/watch?v=bbbbbbbbbbb","time":"2024-05-02T08:00:00Z"},
          {"header":"YouTube","title":"Hai guardato Un video","titleUrl":"https://www.youtube.com/watch?v=ccccccccccc","time":"2024-05-01T08:00:00Z"},
          {"header":"YouTube","title":"Watched https://www.youtube.com/watch?v=ddddddddddd","titleUrl":"https://www.youtube.com/watch?v=ddddddddddd","time":"2024-04-01T08:00:00Z"},
          {"header":"YouTube","title":"Watched Rust in 100 Seconds","titleUrl":"https://www.youtube.com/watch?v=5C_HPTJg5ek","time":"2024-01-01T08:00:00Z"},
          {"header":"YouTube","title":"Watched a video that has been removed","time":"2024-01-01T08:00:00Z"},
          "not an entry",
          {"header":"YouTube","title":"Searched for rust","titleUrl":"https://www.youtube.com/results?search_query=rust","time":"2024-01-01T08:00:00Z"}
        ]"#;
        let watched = history(json.as_bytes());
        let shown: Vec<_> = watched
            .iter()
            .map(|(v, at)| (v.id.as_str(), v.title.as_str(), v.needs_details(), *at))
            .collect();
        assert_eq!(
            shown,
            [
                ("5C_HPTJg5ek", "Rust in 100 Seconds", false, 1_714_644_000),
                ("ccccccccccc", "youtu.be/ccccccccccc", true, 1_714_550_400),
                ("ddddddddddd", "youtu.be/ddddddddddd", true, 1_711_958_400),
            ]
        );
        let (first, _) = &watched[0];
        assert_eq!(first.channel, "Fireship");
        assert_eq!(
            first.channel_id.as_ref().map(ChannelId::as_str),
            Some("UCsBjURrPoezykLs9EqgamOA")
        );
    }

    #[test]
    fn watch_history_stops_at_its_most_and_skips_the_rest() {
        let entry = |i: usize| {
            format!(
                r#"{{"header":"YouTube","title":"Watched {i}","titleUrl":"https://www.youtube.com/watch?v={i:011}","time":"2024-05-02T10:00:00Z"}}"#
            )
        };
        let entries: Vec<String> = (0..MAX_HISTORY + 2).map(entry).collect();
        let json = format!(
            "[{}, \"not an entry\", {{\"header\": 1}}]",
            entries.join(",")
        );
        let watched = history(json.as_bytes());
        assert_eq!(watched.len(), MAX_HISTORY);
        assert_eq!(watched[0].0.title, "0");
        assert!(history(b"[1, 2").is_empty(), "not JSON");
        assert!(history(b"{}").is_empty(), "not a list");
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tuitube-takeout-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_takeout_folder_is_searched_for_what_it_holds() {
        let dir = temp("folder");
        let youtube = dir.join("Takeout").join("YouTube and YouTube Music");
        for sub in ["subscriptions", "playlists", "history", "channels"] {
            std::fs::create_dir_all(youtube.join(sub)).unwrap();
        }
        std::fs::write(
            youtube.join("subscriptions").join("subscriptions.csv"),
            "Channel Id,Channel Url,Channel Title\n\
             UC7EVSn5inapL20oPSwAwEUg,http://www.youtube.com/channel/UC7EVSn5inapL20oPSwAwEUg,BekBrace\n",
        )
        .unwrap();
        std::fs::write(
            youtube.join("playlists").join("Watch later-videos.csv"),
            "Video ID,Playlist Video Creation Timestamp\ndQw4w9WgXcQ,2024-03-01T12:00:00+00:00\n",
        )
        .unwrap();
        std::fs::write(
            youtube.join("playlists").join("playlists.csv"),
            "Playlist ID,Add new videos to top,Playlist Title (Original)\n\
             PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf,False,Road trip\n",
        )
        .unwrap();
        std::fs::write(
            youtube.join("history").join("watch-history.html"),
            "<html>Watched <a href=\"https://www.youtube.com/watch?v=dQw4w9WgXcQ\">A video</a>",
        )
        .unwrap();
        // Takeout's own index page isn't a history.
        std::fs::write(dir.join("Takeout").join("archive_browser.html"), "<html>").unwrap();
        std::fs::write(
            youtube.join("channels").join("channel.csv"),
            "Channel ID,Channel Title (Original),Channel Visibility,Country\n\
             UC7EVSn5inapL20oPSwAwEUg,Me,Public,FR\n",
        )
        .unwrap();
        let found: Vec<Takeout> = read_folder(&dir)
            .unwrap()
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.contains(&Takeout::HistoryPage));
        assert!(
            found
                .iter()
                .any(|t| matches!(t, Takeout::Subscriptions(c) if c.len() == 1))
        );
        assert!(found
            .iter()
            .any(|t| matches!(t, Takeout::Playlist { watch_later: true, videos, .. } if videos.len() == 1)));

        let mut store = Store::in_memory();
        let imported = save(&mut store, found, true).unwrap();
        assert_eq!(
            imported.summary(),
            "Imported 1 subscription (1 new) and 1 video to Watch later. For your history, \
             export it again as JSON (History: “Multiple formats”)"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn devices_and_pipes_are_refused_without_reading_them() {
        let dir = temp("dev");
        let link = dir.join("subscriptions.csv");
        std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
        let error = read(&link).unwrap_err().to_string();
        assert!(error.contains("isn't a file"), "{error}");
        // In a folder, a link isn't followed at all.
        assert!(read_folder(&dir).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_without_any_of_it_is_an_error() {
        let dir = temp("none");
        let path = dir.join("subscriptions.csv");
        std::fs::write(&path, "just,some,text\n").unwrap();
        assert!(read(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
