//! Everything tuitube asks YouTube itself goes through yt-dlp, run as a
//! separate program for each request: searches, channels' video lists and
//! the stream URLs mpv plays. yt-dlp keeps up with YouTube's changes within
//! days; tuitube carries none of that code (see reports/).
//!
//! Every run: an absolute path, a cleaned environment, a private working
//! directory, `--ignore-config` and `--no-plugin-dirs` (a `yt-dlp.conf` in
//! some folder, or a plugin, can't change what it does), `--no-mark-watched`
//! (nothing goes into a YouTube watch history), Deno named by its path, and
//! URLs built from checked ids after `--`. Its JSON is read leniently: every
//! field may be missing, and unknown ones are ignored.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;

use crate::ids::{ChannelId, VideoId, image_url_allowed};
use crate::text;
use crate::tools::{self, Tools};
use crate::video::{self, MAX_CHANNEL, MAX_DESCRIPTION, MAX_TITLE, Video};

/// A request taking longer than this is stopped.
const TIMEOUT: Duration = Duration::from_secs(90);
/// The most JSON read from one run. A video's full description with every
/// format is under 1 MB; a 150-result search under 2 MB.
const MAX_OUTPUT: usize = 32 * 1024 * 1024;
/// Longest search kept, in characters.
pub const MAX_QUERY: usize = 200;

#[derive(Clone)]
pub struct YtDlp {
    path: PathBuf,
    deno: Option<PathBuf>,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    work_dir: PathBuf,
    cache_dir: PathBuf,
    /// Runs for what you asked for: a search, a channel, a video to play.
    interactive: Arc<Semaphore>,
    /// Runs nobody waits on: channel photos.
    background: Arc<Semaphore>,
}

/// A tab of a channel's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    /// Uploads.
    Videos,
    /// Live streams and premieres: a feed lists them, the Videos tab doesn't.
    Streams,
}

/// A channel's page: its name, photo and newest videos.
#[derive(Debug)]
pub struct ChannelPage {
    pub id: ChannelId,
    pub title: String,
    pub avatar: Option<String>,
    pub videos: Vec<Video>,
}

/// What mpv is given to play a video.
#[derive(Clone, Debug, PartialEq)]
pub struct Streams {
    pub video: String,
    /// The sound, when it's a stream of its own.
    pub audio: Option<String>,
    pub user_agent: Option<String>,
    pub duration: Option<f64>,
}

/// How much of a video to play.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Quality {
    /// Picture no taller than this many pixels, and sound.
    Video(u32),
    AudioOnly,
}

impl Quality {
    /// yt-dlp's format choice. AV1 is left out: few computers decode it in
    /// hardware, and yt-dlp picks it first at the highest sizes.
    fn format(self) -> String {
        match self {
            Quality::Video(h) => {
                format!("bv*[height<={h}][vcodec!^=av01]+ba/bv*[height<={h}]+ba/b[height<={h}]/b")
            }
            Quality::AudioOnly => "ba[ext=m4a]/ba/b".into(),
        }
    }
}

impl YtDlp {
    /// `None` if yt-dlp isn't installed.
    pub fn new(tools: &Tools, data_dir: &std::path::Path) -> Result<Option<Self>> {
        let Some(path) = tools.yt_dlp.clone() else {
            return Ok(None);
        };
        let work_dir = data_dir.join("work");
        let cache_dir = data_dir.join("cache").join("yt-dlp");
        crate::config::private_dir(&work_dir)?;
        crate::config::private_dir(&cache_dir)?;
        Ok(Some(Self {
            path,
            deno: tools.deno.clone(),
            env: tools::child_env(tools),
            work_dir,
            cache_dir,
            interactive: Arc::new(Semaphore::new(2)),
            background: Arc::new(Semaphore::new(2)),
        }))
    }

    /// The arguments every run starts with.
    fn base_args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "--ignore-config",
            "--no-plugin-dirs",
            "--no-mark-watched",
            "--no-progress",
            "--no-color",
            "--no-cookies-from-browser",
            "--no-cookies",
            // Only the JavaScript solver yt-dlp ships with, never one it
            // would download from GitHub at run time.
            "--no-remote-components",
            "--dump-single-json",
            "--cache-dir",
        ]
        .map(String::from)
        .into();
        args.push(self.cache_dir.to_string_lossy().into_owned());
        if let Some(deno) = &self.deno {
            args.push("--no-js-runtimes".into());
            args.push("--js-runtimes".into());
            args.push(format!("deno:{}", deno.display()));
        }
        args
    }

    /// Runs yt-dlp with the base arguments, `args`, then `--` and `target`,
    /// and reads its JSON.
    async fn run<T: for<'de> Deserialize<'de>>(
        &self,
        args: &[&str],
        target: &str,
        background: bool,
    ) -> Result<T> {
        let limit = if background {
            &self.background
        } else {
            &self.interactive
        };
        let _permit = limit.acquire().await?;
        let mut command = tokio::process::Command::new(&self.path);
        command
            .args(self.base_args())
            .args(args)
            .arg("--")
            .arg(target)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().context("cannot start yt-dlp")?;
        let mut stdout = child.stdout.take().context("no output")?;
        let mut stderr = child.stderr.take().context("no output")?;
        let work = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let read_out = async {
                let mut chunk = [0u8; 64 * 1024];
                loop {
                    let n = stdout.read(&mut chunk).await?;
                    if n == 0 {
                        break;
                    }
                    if out.len() + n > MAX_OUTPUT {
                        bail!("yt-dlp said too much");
                    }
                    out.extend_from_slice(&chunk[..n]);
                }
                Ok(())
            };
            let read_err = async {
                // Only the end matters: the error is on the last lines.
                let mut chunk = [0u8; 8 * 1024];
                loop {
                    let n = stderr.read(&mut chunk).await?;
                    if n == 0 {
                        break;
                    }
                    err.extend_from_slice(&chunk[..n]);
                    if err.len() > 64 * 1024 {
                        err.drain(..err.len() - 16 * 1024);
                    }
                }
                Ok::<_, anyhow::Error>(())
            };
            let (a, b) = tokio::join!(read_out, read_err);
            a?;
            b?;
            let status = child.wait().await?;
            Ok::<_, anyhow::Error>((status, out, err))
        };
        let (status, out, err) = tokio::time::timeout(TIMEOUT, work)
            .await
            .context("yt-dlp took too long")??;
        if !status.success() || out.is_empty() {
            bail!("{}", error_line(&String::from_utf8_lossy(&err)));
        }
        serde_json::from_slice(&out).context("yt-dlp's answer couldn't be read")
    }

    /// The first `count` results for `query`.
    pub async fn search(&self, query: &str, count: usize) -> Result<Vec<Video>> {
        let query = video::one_line(query, MAX_QUERY);
        if query.is_empty() {
            bail!("nothing to search for");
        }
        let target = format!("ytsearch{count}:{query}");
        let list: Playlist = self.run(&["--flat-playlist"], &target, false).await?;
        Ok(list
            .entries
            .into_iter()
            .filter_map(|e| e.video(None, None))
            .collect())
    }

    /// The channel's name, photo and `count` newest videos.
    pub async fn channel(
        &self,
        id: &ChannelId,
        tab: Tab,
        count: usize,
        background: bool,
    ) -> Result<ChannelPage> {
        let items = format!("1:{count}");
        let url = match tab {
            Tab::Videos => id.videos_url(),
            Tab::Streams => id.streams_url(),
        };
        let list: Playlist = self
            .run(
                &["--flat-playlist", "--playlist-items", &items],
                &url,
                background,
            )
            .await?;
        let title = video::one_line(
            list.channel
                .as_deref()
                .or(list.uploader.as_deref())
                .unwrap_or(""),
            MAX_CHANNEL,
        );
        let avatar = list
            .thumbnails
            .iter()
            .filter(|t| t.width.is_some() && t.width == t.height)
            .filter(|t| image_url_allowed(&t.url))
            .min_by_key(|t| t.width)
            .map(|t| small_avatar(&t.url));
        let videos = list
            .entries
            .into_iter()
            .filter_map(|e| e.video(Some(id), Some(&title)))
            .collect();
        Ok(ChannelPage {
            id: id.clone(),
            title,
            avatar,
            videos,
        })
    }

    /// The stream URLs for `id` at `quality`. They work for about six hours,
    /// from this computer's address only.
    pub async fn resolve(&self, id: &VideoId, quality: Quality) -> Result<Streams> {
        let format = quality.format();
        let info: VideoInfo = self
            .run(&["--no-playlist", "-f", &format], &id.url(), false)
            .await?;
        info.streams()
    }
}

/// yt-dlp's last `ERROR:` line, or its last line, cleaned for the status bar.
fn error_line(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let line = lines
        .iter()
        .rev()
        .find(|l| l.starts_with("ERROR:"))
        .or(lines.last())
        .copied()
        .unwrap_or("yt-dlp failed");
    let line = line.strip_prefix("ERROR: ").unwrap_or(line);
    // "[youtube] dQw4w9WgXcQ: Sign in to confirm…" reads better without the
    // extractor's tag and the id.
    let line = match line.split_once("]: ").or_else(|| line.split_once("] ")) {
        Some((tag, rest)) if tag.starts_with('[') => rest,
        _ => line,
    };
    let line = match line.split_once(": ") {
        Some((id, rest)) if VideoId::parse(id).is_some() => rest,
        _ => line,
    };
    text::first_chars(&text::clean(line), 200).to_string()
}

/// A channel photo URL asked for at 176 px (`=s176-…`) instead of the
/// 900 px yt-dlp lists.
fn small_avatar(url: &str) -> String {
    if let Some(at) = url.rfind("=s") {
        let digits = url[at + 2..].bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            return format!("{}=s176{}", &url[..at], &url[at + 2 + digits..]);
        }
    }
    url.to_string()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Playlist {
    channel: Option<String>,
    uploader: Option<String>,
    thumbnails: Vec<Thumbnail>,
    entries: Vec<Entry>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Thumbnail {
    url: String,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Entry {
    id: Option<String>,
    url: Option<String>,
    title: Option<String>,
    description: Option<String>,
    duration: Option<f64>,
    channel_id: Option<String>,
    channel: Option<String>,
    uploader: Option<String>,
    view_count: Option<f64>,
    timestamp: Option<f64>,
    release_timestamp: Option<f64>,
    live_status: Option<String>,
    thumbnails: Vec<Thumbnail>,
}

impl Entry {
    /// The entry as a video; `None` for one that isn't (a channel or a
    /// playlist in search results) or whose id isn't a video id.
    fn video(self, channel: Option<&ChannelId>, channel_title: Option<&str>) -> Option<Video> {
        let id = VideoId::parse(self.id.as_deref()?)?;
        let url = self.url.as_deref().unwrap_or("");
        let short = url.starts_with("https://www.youtube.com/shorts/");
        let channel_id = self
            .channel_id
            .as_deref()
            .and_then(ChannelId::parse)
            .or_else(|| channel.cloned());
        let name = self
            .channel
            .as_deref()
            .or(self.uploader.as_deref())
            .or(channel_title)
            .unwrap_or("");
        // The widest thumbnail up to 720 px: enough for the biggest card.
        let thumbnail = self
            .thumbnails
            .iter()
            .filter(|t| image_url_allowed(&t.url))
            .filter(|t| t.width.is_none_or(|w| w <= 720))
            .max_by_key(|t| t.width.unwrap_or(0))
            .map(|t| t.url.clone());
        let live = self.live_status.as_deref() == Some("is_live");
        let upcoming = self.live_status.as_deref() == Some("is_upcoming");
        Some(Video {
            id,
            title: video::one_line(self.title.as_deref().unwrap_or(""), MAX_TITLE),
            channel_id,
            channel: video::one_line(name, MAX_CHANNEL),
            description: video::one_line(
                self.description.as_deref().unwrap_or(""),
                MAX_DESCRIPTION,
            ),
            published: self.timestamp.or(self.release_timestamp).map(|t| t as i64),
            views: self.view_count.filter(|n| *n >= 0.0).map(|n| n as u64),
            duration: self
                .duration
                .filter(|d| d.is_finite() && *d >= 0.0 && *d < 1e7)
                .map(|d| d as u32),
            short,
            live,
            upcoming,
            thumbnail,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VideoInfo {
    url: Option<String>,
    duration: Option<f64>,
    http_headers: Option<Headers>,
    requested_formats: Vec<Format>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Format {
    url: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    http_headers: Option<Headers>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Headers {
    #[serde(rename = "User-Agent")]
    user_agent: Option<String>,
}

/// Whether `url` is one mpv may be given: https on YouTube's video servers.
/// Checked because it comes from yt-dlp's output, and mpv takes many kinds
/// of URL (`av://`, `edl://`, files…) that a stray one could be.
pub fn stream_url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.contains(['@', ':', '\\'])
        && (host.ends_with(".googlevideo.com") || host.ends_with(".youtube.com"))
}

impl VideoInfo {
    fn streams(self) -> Result<Streams> {
        let has = |codec: &Option<String>| codec.as_deref().is_some_and(|c| c != "none");
        let (video, audio, headers) = match self.requested_formats.as_slice() {
            [] => (self.url, None, self.http_headers),
            formats => {
                let video = formats.iter().find(|f| has(&f.vcodec));
                let audio = formats.iter().find(|f| has(&f.acodec) && !has(&f.vcodec));
                match (video, audio) {
                    (Some(v), audio) => (
                        v.url.clone(),
                        audio.and_then(|a| a.url.clone()),
                        v.http_headers.clone(),
                    ),
                    (None, Some(a)) => (a.url.clone(), None, a.http_headers.clone()),
                    (None, None) => (
                        formats[0].url.clone(),
                        None,
                        formats[0].http_headers.clone(),
                    ),
                }
            }
        };
        let video = video.context("yt-dlp found nothing to play")?;
        if !stream_url_allowed(&video) || audio.as_deref().is_some_and(|a| !stream_url_allowed(a)) {
            bail!("yt-dlp gave a stream that isn't on YouTube's servers");
        }
        let user_agent = headers
            .and_then(|h| h.user_agent)
            .map(|ua| text::first_chars(&text::clean(&ua), 300).to_string());
        Ok(Streams {
            video,
            audio,
            user_agent,
            duration: self.duration.filter(|d| d.is_finite() && *d > 0.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_entries_become_videos_and_others_are_skipped() {
        let json = r#"{"entries": [
            {"id": "rQ_J9WH6CGk", "url": "https://www.youtube.com/watch?v=rQ_J9WH6CGk",
             "title": "Rust \u001b[31mcourse", "description": "Duration: 3 hours",
             "duration": 11104, "channel_id": "UC7EVSn5inapL20oPSwAwEUg", "channel": "BekBrace",
             "view_count": 638955, "thumbnails": [
                {"url": "https://i.ytimg.com/vi/rQ_J9WH6CGk/hq720.jpg?a=1", "width": 360, "height": 202},
                {"url": "https://i.ytimg.com/vi/rQ_J9WH6CGk/hq720.jpg?a=2", "width": 720, "height": 404},
                {"url": "https://evil.example/x.jpg", "width": 700, "height": 404}]},
            {"id": "UCxxxxxxxxxxxxxxxxxxxxxx", "url": "https://www.youtube.com/channel/UCx"},
            {"id": "-abcdefghij", "url": "https://www.youtube.com/shorts/-abcdefghij", "title": "s"}
        ]}"#;
        let list: Playlist = serde_json::from_str(json).unwrap();
        let videos: Vec<Video> = list
            .entries
            .into_iter()
            .filter_map(|e| e.video(None, None))
            .collect();
        assert_eq!(videos.len(), 2, "the channel result isn't a video");
        let v = &videos[0];
        assert_eq!(v.title, "Rust [31mcourse");
        assert_eq!(v.duration, Some(11104));
        assert_eq!(v.views, Some(638955));
        assert_eq!(
            v.thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/rQ_J9WH6CGk/hq720.jpg?a=2")
        );
        assert!(videos[1].short);
        assert_eq!(videos[1].id.as_str(), "-abcdefghij");
    }

    #[test]
    fn streams_are_split_into_picture_and_sound_and_must_be_youtubes() {
        let json = r#"{"duration": 212.0, "requested_formats": [
            {"url": "https://rr1---sn-abc.googlevideo.com/videoplayback?x=1", "vcodec": "vp09", "acodec": "none",
             "http_headers": {"User-Agent": "Mozilla/5.0"}},
            {"url": "https://rr1---sn-abc.googlevideo.com/videoplayback?x=2", "vcodec": "none", "acodec": "opus"}
        ]}"#;
        let info: VideoInfo = serde_json::from_str(json).unwrap();
        let streams = info.streams().unwrap();
        assert!(streams.video.ends_with("x=1"));
        assert!(streams.audio.unwrap().ends_with("x=2"));
        assert_eq!(streams.user_agent.as_deref(), Some("Mozilla/5.0"));
        assert_eq!(streams.duration, Some(212.0));

        for bad in [
            "file:///etc/passwd",
            "edl://x",
            "https://evil.example/v",
            "--script=x",
            "https://x.googlevideo.com@evil.example/",
        ] {
            let info = VideoInfo {
                url: Some(bad.into()),
                ..Default::default()
            };
            assert!(info.streams().is_err(), "{bad}");
        }
    }

    #[test]
    fn errors_are_shortened_for_the_status_bar() {
        let stderr = "WARNING: something\nERROR: [youtube] dQw4w9WgXcQ: Sign in to confirm you're not a bot\n";
        assert_eq!(error_line(stderr), "Sign in to confirm you're not a bot");
        assert_eq!(error_line("weird\u{1b}[2J"), "weird[2J");
    }

    #[test]
    fn channel_photos_are_asked_for_small() {
        assert_eq!(
            small_avatar("https://yt3.googleusercontent.com/abc=s900-c-k-c0x00ffffff-no-rj"),
            "https://yt3.googleusercontent.com/abc=s176-c-k-c0x00ffffff-no-rj"
        );
        assert_eq!(
            small_avatar("https://yt3.ggpht.com/abc"),
            "https://yt3.ggpht.com/abc"
        );
    }
}

/// Against YouTube itself, logged out: `cargo test -- --ignored live`.
#[cfg(test)]
mod live {
    use super::*;

    fn yt() -> YtDlp {
        let tools = Tools::find(None, None, None);
        let dir = std::env::temp_dir().join(format!("tuitube-live-{}", std::process::id()));
        YtDlp::new(&tools, &dir)
            .unwrap()
            .expect("yt-dlp is installed")
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_search_channel_and_resolve() {
        let yt = yt();
        let found = yt.search("rust programming", 5).await.unwrap();
        assert!(!found.is_empty());
        let first = &found[0];
        eprintln!("search: {first:?}");
        assert!(first.channel_id.is_some() && !first.title.is_empty());

        let channel = first.channel_id.clone().unwrap();
        let page = yt.channel(&channel, Tab::Videos, 3, false).await.unwrap();
        eprintln!(
            "channel: {} {:?} {} videos",
            page.title,
            page.avatar,
            page.videos.len()
        );
        assert!(!page.videos.is_empty());
        assert!(page.avatar.as_deref().is_some_and(image_url_allowed));

        let streams = yt.resolve(&first.id, Quality::Video(720)).await.unwrap();
        eprintln!(
            "streams: audio {} duration {:?}",
            streams.audio.is_some(),
            streams.duration
        );
        assert!(stream_url_allowed(&streams.video));
        let sound = yt.resolve(&first.id, Quality::AudioOnly).await.unwrap();
        assert!(sound.audio.is_none());
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_feed() {
        let channel = ChannelId::parse("UC7EVSn5inapL20oPSwAwEUg").unwrap();
        let limit = Arc::new(Semaphore::new(1));
        let feed = crate::feed::fetch(&crate::feed::client(), &limit, &channel)
            .await
            .unwrap();
        eprintln!("feed: {} with {} videos", feed.channel, feed.videos.len());
        assert!(!feed.videos.is_empty());
    }
}
