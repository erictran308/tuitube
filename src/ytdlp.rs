//! Everything tuitube asks YouTube itself goes through yt-dlp, run as a
//! separate program for each request: searches, channels' video lists,
//! Mixes and the stream URLs mpv plays. yt-dlp keeps up with YouTube's changes within
//! days; tuitube carries none of that code (see reports/).
//!
//! Every run: an absolute path, a cleaned environment, a private working
//! directory, `--ignore-config` and `--no-plugin-dirs` (a `yt-dlp.conf` in
//! some folder, or a plugin, can't change what it does), `--no-mark-watched`
//! (nothing goes into a YouTube watch history), only Deno, named by its
//! path, for JavaScript, and URLs built from checked ids. The URL or search
//! goes in on stdin (`--batch-file -`), not on the command line, which every
//! user of the computer can read (`ps`). Its JSON is read leniently: every
//! field may be missing, unknown ones are ignored, and an entry that doesn't
//! fit is skipped rather than failing the list.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use crate::chapters::{self, Chapter};
use crate::ids::{ChannelId, ChannelName, PlaylistId, VideoId, image_url_allowed};
use crate::text;
use crate::tools::{self, Tools};
use crate::video::{self, MAX_CHANNEL, MAX_CHAPTER, MAX_DESCRIPTION, MAX_TITLE, Video};

/// A request taking longer than this is stopped.
const TIMEOUT: Duration = Duration::from_secs(90);
/// CPU seconds yt-dlp, and each program it starts (Deno running YouTube's
/// player script), may use before the system ends it: a backstop for when
/// tuitube itself is gone and can't end them.
#[cfg(unix)]
const MAX_CPU_SECONDS: libc::rlim_t = 120;
/// The most JSON read from one run. A video's full description with every
/// format is under 1 MB; a 150-result search under 2 MB.
const MAX_OUTPUT: usize = 32 * 1024 * 1024;
/// The latest date believed, in Unix seconds (the year 2100): anything later,
/// or before 1970, is a mistake or a trick.
const MAX_TIMESTAMP: f64 = 4_102_444_800.0;
/// Longest search kept, in characters.
pub const MAX_QUERY: usize = 200;
/// Videos asked about at once by [`YtDlp::details`]: YouTube's lists made
/// of ids take 50.
pub const MAX_DETAILS: usize = 50;

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
    /// Its chapters, in order; none for most videos.
    pub chapters: Vec<Chapter>,
    /// What plays, for the player bar: `1080p60`, or `128 kbps` for sound
    /// only.
    pub quality: Option<String>,
    /// The video, as YouTube says now: its title, channel, date, views and
    /// whole description, from the same answer.
    pub info: Option<Box<Video>>,
    /// Its subtitles: the uploader's, then YouTube's automatic captions.
    pub captions: Vec<Caption>,
}

/// A comment on a video, as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub author: String,
    /// Its text, on its lines ([`video::lines`]).
    pub text: String,
    pub likes: Option<u64>,
    /// When it was written, in Unix seconds (YouTube's "3 days ago",
    /// worked out).
    pub at: Option<i64>,
    pub pinned: bool,
    /// Written by the video's channel.
    pub by_uploader: bool,
}

/// Comments asked for: YouTube's top ones, without replies.
pub const MAX_COMMENTS: usize = 100;
/// Longest comment kept, in characters: YouTube allows 10,000.
const MAX_COMMENT: usize = 2_000;

#[derive(Deserialize, Default)]
#[serde(default)]
struct WithComments {
    #[serde(deserialize_with = "lenient")]
    comments: Vec<RawComment>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawComment {
    text: Option<String>,
    author: Option<String>,
    like_count: Option<f64>,
    timestamp: Option<f64>,
    is_pinned: Option<bool>,
    author_is_uploader: Option<bool>,
    parent: Option<String>,
}

/// A subtitle track, as WebVTT on YouTube's own server.
#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    /// YouTube's language code: `en`, `pt-BR`, `en-orig` (an automatic
    /// caption in the language spoken).
    pub lang: String,
    /// Its name, as YouTube gives it: "English", "English (Original)".
    pub name: String,
    pub url: String,
    /// Made by YouTube's speech recognition (or translated by it).
    pub auto: bool,
}

/// The most subtitle tracks kept: YouTube translates its automatic captions
/// into about 160 languages.
const MAX_CAPTIONS: usize = 400;

/// Comments as shown: top-level ones with words, each cleaned.
fn comments(raw: Vec<RawComment>) -> Vec<Comment> {
    raw.into_iter()
        .filter(|c| c.parent.as_deref().is_none_or(|p| p == "root"))
        .filter_map(|c| {
            let text = video::lines(c.text.as_deref()?, MAX_COMMENT);
            (!text.is_empty()).then(|| Comment {
                author: video::one_line(c.author.as_deref().unwrap_or(""), MAX_CHANNEL),
                text,
                likes: c
                    .like_count
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .map(|n| n as u64),
                at: c
                    .timestamp
                    .filter(|t| (0.0..=MAX_TIMESTAMP).contains(t))
                    .map(|t| t as i64),
                pinned: c.is_pinned.unwrap_or(false),
                by_uploader: c.author_is_uploader.unwrap_or(false),
            })
        })
        .take(MAX_COMMENTS)
        .collect()
}

/// The subtitle tracks in yt-dlp's `subtitles` and `automatic_captions`
/// (language → formats): each language's WebVTT, if its URL is YouTube's.
/// Read from any JSON at all, so odd subtitles never stop a video playing.
fn captions(manual: &serde_json::Value, auto: &serde_json::Value) -> Vec<Caption> {
    let mut found = Vec::new();
    for (tracks, is_auto) in [(manual, false), (auto, true)] {
        let Some(tracks) = tracks.as_object() else {
            continue;
        };
        for (lang, formats) in tracks {
            // A live stream's chat comes as a "subtitle" too.
            let code =
                lang.len() <= 20 && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
            if !code || lang == "live_chat" || found.len() >= MAX_CAPTIONS {
                continue;
            }
            let vtt =
                formats.as_array().into_iter().flatten().find(|f| {
                    f["ext"] == "vtt" && f["url"].as_str().is_some_and(stream_url_allowed)
                });
            let Some(vtt) = vtt else {
                continue;
            };
            found.push(Caption {
                lang: lang.clone(),
                name: video::one_line(vtt["name"].as_str().unwrap_or(lang), 60),
                url: vtt["url"].as_str().unwrap_or_default().to_string(),
                auto: is_auto,
            });
        }
    }
    found
}

/// The subtitles to show for `languages`, in the order wanted: for each,
/// the uploader's in that language (`en`, then `en-GB`), then YouTube's
/// automatic captions of it as spoken (`en-orig`), then translated into it.
pub fn pick_caption<'a>(captions: &'a [Caption], languages: &[String]) -> Option<&'a Caption> {
    let is = |c: &Caption, lang: &str| c.lang.eq_ignore_ascii_case(lang);
    let within = |c: &Caption, lang: &str| {
        c.lang
            .split_once('-')
            .is_some_and(|(base, _)| base.eq_ignore_ascii_case(lang))
    };
    languages.iter().find_map(|lang| {
        let manual = || captions.iter().filter(|c| !c.auto);
        let auto = || captions.iter().filter(|c| c.auto);
        manual()
            .find(|c| is(c, lang))
            .or_else(|| manual().find(|c| within(c, lang) && !c.lang.ends_with("-orig")))
            .or_else(|| auto().find(|c| is(c, &format!("{lang}-orig"))))
            .or_else(|| auto().find(|c| is(c, lang)))
    })
}

/// How much of a video to play.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Quality {
    /// A picture of at most this many lines (YouTube's "1080p"), and sound.
    Video(u32, AudioQuality),
    AudioOnly(AudioQuality),
}

/// How good the sound is: `audio_quality` in the settings. Logged out,
/// YouTube has two kinds: about 130 kbps (Opus, or AAC in m4a) and about
/// 50–70 kbps; the 256 kbps it streams to YouTube Music Premium needs an
/// account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioQuality {
    /// The best there is: about 130 kbps.
    #[default]
    High,
    /// About 70 kbps, to save data; the best there is if a video has none
    /// that small.
    Low,
}

impl AudioQuality {
    pub const ALL: [AudioQuality; 2] = [AudioQuality::High, AudioQuality::Low];

    /// Its name in settings.toml.
    pub fn name(self) -> &'static str {
        match self {
            AudioQuality::High => "high",
            AudioQuality::Low => "low",
        }
    }

    /// yt-dlp's choices of sound, best first. `abr` without `?`: a stream
    /// whose bitrate isn't known (HLS) doesn't count as small.
    fn choices(self) -> &'static [&'static str] {
        match self {
            AudioQuality::High => &["ba"],
            AudioQuality::Low => &["ba[abr<=80]", "ba"],
        }
    }
}

impl Quality {
    /// yt-dlp's format choice. AV1 is left out where there's another: few
    /// computers decode it in hardware, and yt-dlp picks it first at the
    /// highest sizes. An upright video (a Short) is measured across, as
    /// YouTube does: its 1080p is 1080 wide and 1920 tall.
    fn format(self) -> String {
        match self {
            Quality::Video(h, sound) => {
                let pictures = [
                    format!("bv*[aspect_ratio<1][width<={h}][vcodec!^=av01]"),
                    format!("bv*[aspect_ratio<1][width<={h}]"),
                    format!("bv*[height<={h}][vcodec!^=av01]"),
                    format!("bv*[height<={h}]"),
                ];
                let mut choices: Vec<String> = pictures
                    .iter()
                    .flat_map(|p| sound.choices().iter().map(move |s| format!("{p}+{s}")))
                    .collect();
                choices.push(format!("b[height<={h}]"));
                choices.push("b".into());
                choices.join("/")
            }
            Quality::AudioOnly(sound) => [sound.choices(), &["b"]].concat().join("/"),
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
    fn base_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
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
            // No runtime found by name: only the Deno found at start.
            "--no-js-runtimes",
            "--cache-dir",
        ]
        .map(OsString::from)
        .into();
        args.push(self.cache_dir.clone().into_os_string());
        // With no proxy chosen, none at all: not the system's proxy settings,
        // which tuitube's own requests and mpv don't follow. A proxy that is
        // chosen comes in the environment, since it may hold a password.
        if crate::proxy::get().is_none() {
            args.extend(["--proxy".into(), "".into()]);
        }
        if let Some(deno) = &self.deno {
            args.push("--js-runtimes".into());
            let mut runtime = OsString::from("deno:");
            runtime.push(deno);
            args.push(runtime);
        }
        args
    }

    /// Runs yt-dlp with the base arguments and `args`, gives it `target` on
    /// stdin, and reads its JSON.
    async fn run<T: for<'de> Deserialize<'de>>(
        &self,
        args: &[&str],
        target: &str,
        background: bool,
    ) -> Result<T> {
        let mut all = self.base_args();
        all.extend(args.iter().map(OsString::from));
        all.extend(["--batch-file".into(), "-".into()]);
        let out = self.output(all, Some(target), background).await?;
        serde_json::from_slice(&out).context("yt-dlp's answer couldn't be read")
    }

    /// Runs yt-dlp with `args` and `target` (if any) on stdin, and returns
    /// what it wrote, or its error.
    async fn output(
        &self,
        args: Vec<OsString>,
        target: Option<&str>,
        background: bool,
    ) -> Result<Vec<u8>> {
        let limit = if background {
            &self.background
        } else {
            &self.interactive
        };
        let _permit = limit.acquire().await?;
        let mut command = tokio::process::Command::new(&self.path);
        command
            .args(args)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.work_dir)
            .stdin(if target.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // yt-dlp in a process group of its own, which Run kills as a whole,
        // and a CPU limit that it and everything it starts inherit.
        #[cfg(unix)]
        {
            command.process_group(0);
            // SAFETY: setrlimit is async-signal-safe; nothing else runs
            // between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    let limit = libc::rlimit {
                        rlim_cur: MAX_CPU_SECONDS,
                        rlim_max: MAX_CPU_SECONDS + 10,
                    };
                    if libc::setrlimit(libc::RLIMIT_CPU, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut run = Run({
            let _starting = tools::starting();
            command.spawn().context("cannot start yt-dlp")?
        });
        let child = &mut run.0;
        // One line: yt-dlp reads URLs until the end of its input. Targets
        // start with `https://` or `ytsearch`, never with a comment's `#`.
        if let Some(target) = target {
            let mut stdin = child.stdin.take().context("no input")?;
            stdin.write_all(format!("{target}\n").as_bytes()).await?;
        }
        let mut stdout = child.stdout.take().context("no output")?;
        let mut stderr = child.stderr.take().context("no output")?;
        let work = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            // The buffers on the heap: in the future, they'd make every
            // request's future 75 KB, and a debug build's stack can't hold
            // a few of those.
            let read_out = async {
                let mut chunk = vec![0u8; 64 * 1024];
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
                let mut chunk = vec![0u8; 8 * 1024];
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
            // Too much output ends the run at once: the child is dropped,
            // and killed, without waiting for it to finish writing.
            tokio::try_join!(read_out, read_err)?;
            let status = child.wait().await?;
            Ok::<_, anyhow::Error>((status, out, err))
        };
        let (status, out, err) = tokio::time::timeout(TIMEOUT, work)
            .await
            .context("yt-dlp took too long")??;
        if !status.success() || out.is_empty() {
            bail!("{}", error_line(&String::from_utf8_lossy(&err)));
        }
        Ok(out)
    }
}

/// A running yt-dlp. yt-dlp waits for the Deno it starts with no time limit
/// of its own, and killing yt-dlp alone would leave Deno running YouTube's
/// script: so when a run ends before yt-dlp was waited for (the timeout,
/// too much output, or the request dropped when tuitube quits), its whole
/// process group is killed.
struct Run(tokio::process::Child);

impl Drop for Run {
    fn drop(&mut self) {
        // Some only until yt-dlp has been reaped, so the group is this run's.
        #[cfg(unix)]
        if let Some(pid) = self.0.id()
            && let Ok(pid) = libc::pid_t::try_from(pid)
        {
            // SAFETY: a plain system call on the group this run created.
            unsafe { libc::killpg(pid, libc::SIGKILL) };
        }
    }
}

impl YtDlp {
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

    /// The channel's name, photo and `count` newest videos. With `dated`,
    /// each video's date is worked out from what the page says ("3 days
    /// ago"): rough, but all there is without a feed.
    pub async fn channel(
        &self,
        id: &ChannelId,
        tab: Tab,
        count: usize,
        background: bool,
        dated: bool,
    ) -> Result<ChannelPage> {
        let url = match tab {
            Tab::Videos => id.videos_url(),
            Tab::Streams => id.streams_url(),
        };
        self.page(&url, Some(id), count, background, dated).await
    }

    /// A channel named by a link (`@handle`): its id, name, photo and
    /// `count` newest videos.
    pub async fn named_channel(&self, name: &ChannelName, count: usize) -> Result<ChannelPage> {
        self.page(&name.videos_url(), None, count, false, false)
            .await
    }

    /// A channel's tab at `url`. Its id is `id` if known, else the one the
    /// page gives.
    async fn page(
        &self,
        url: &str,
        id: Option<&ChannelId>,
        count: usize,
        background: bool,
        dated: bool,
    ) -> Result<ChannelPage> {
        let items = format!("1:{count}");
        let mut args = vec!["--flat-playlist", "--playlist-items", &items];
        if dated {
            args.extend(["--extractor-args", "youtubetab:approximate_date"]);
        }
        let list: Playlist = self.run(&args, url, background).await?;
        let id = match id {
            Some(id) => id.clone(),
            None => list
                .channel_id
                .as_deref()
                .and_then(ChannelId::parse)
                .context("that isn't a channel")?,
        };
        let id = &id;
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

    /// YouTube's Mix of `id`: up to `count` similar videos, `id` first.
    pub async fn mix(&self, id: &VideoId, count: usize) -> Result<Vec<Video>> {
        // Capped: yt-dlp would otherwise page through over a thousand.
        let items = format!("1:{count}");
        let list: Playlist = self
            .run(
                &[
                    "--flat-playlist",
                    "--yes-playlist",
                    "--playlist-items",
                    &items,
                ],
                &id.mix_url(),
                false,
            )
            .await?;
        mix_videos(list)
    }

    /// A playlist's title and its first `count` videos.
    pub async fn playlist(&self, id: &PlaylistId, count: usize) -> Result<(String, Vec<Video>)> {
        let items = format!("1:{count}");
        let list: Playlist = self
            .run(
                &[
                    "--flat-playlist",
                    "--yes-playlist",
                    "--playlist-items",
                    &items,
                ],
                &id.url(),
                false,
            )
            .await?;
        let title = video::one_line(list.title.as_deref().unwrap_or(""), MAX_TITLE);
        let videos = list
            .entries
            .into_iter()
            .filter_map(|e| e.video(None, None))
            .collect();
        Ok((title, videos))
    }

    /// What YouTube says of videos known only by their ids (a Takeout
    /// list's): their titles, channels and lengths, up to 50 in one request
    /// for a list YouTube makes of them (`watch_videos`), without asking
    /// for any of them to play. Ones that are private or gone aren't there.
    pub async fn details(&self, ids: &[VideoId]) -> Result<Vec<Video>> {
        let ids: Vec<&str> = ids.iter().take(MAX_DETAILS).map(VideoId::as_str).collect();
        let url = format!(
            "https://www.youtube.com/watch_videos?video_ids={}",
            ids.join(",")
        );
        let items = format!("1:{MAX_DETAILS}");
        let list: Playlist = self
            .run(
                &[
                    "--flat-playlist",
                    "--yes-playlist",
                    "--playlist-items",
                    &items,
                ],
                &url,
                true,
            )
            .await?;
        Ok(list
            .entries
            .into_iter()
            .filter_map(|e| e.video(None, None))
            .filter(|v| ids.contains(&v.id.as_str()))
            .collect())
    }

    /// The video's top comments, [`MAX_COMMENTS`] of them, without
    /// replies. yt-dlp reads the video's page for them, the same request
    /// as playing it, so they're asked for only on a key.
    pub async fn comments(&self, id: &VideoId) -> Result<Vec<Comment>> {
        let wanted = format!("youtube:max_comments={MAX_COMMENTS},all,0;comment_sort=top");
        let found: WithComments = self
            .run(
                &[
                    "--no-playlist",
                    "--write-comments",
                    "--ignore-no-formats-error",
                    "--extractor-args",
                    &wanted,
                ],
                &id.url(),
                false,
            )
            .await?;
        Ok(comments(found.comments))
    }

    /// yt-dlp's version: the day it was released, as `2026.08.19`.
    pub async fn version(&self) -> Result<String> {
        let args = ["--ignore-config", "--no-plugin-dirs", "--version"];
        let out = self
            .output(args.map(OsString::from).into(), None, true)
            .await?;
        let out = String::from_utf8_lossy(&out);
        Ok(text::first_chars(&text::clean(out.trim()), 40).to_string())
    }

    /// The stream URLs for `id` at `quality`. They work for about six hours,
    /// from this computer's address only.
    pub async fn resolve(&self, id: &VideoId, quality: Quality) -> Result<Streams> {
        let format = quality.format();
        let mut args = self.base_args();
        args.extend(["--no-playlist", "-f", &format, "--batch-file", "-"].map(OsString::from));
        let out = self.output(args, Some(&id.url()), false).await?;
        let info: VideoInfo =
            serde_json::from_slice(&out).context("yt-dlp's answer couldn't be read")?;
        let mut streams = info.streams()?;
        // The same answer, read again for what the video is.
        streams.info = serde_json::from_slice::<Entry>(&out)
            .ok()
            .and_then(|e| e.video(None, None))
            .filter(|v| v.id == *id)
            .map(Box::new);
        Ok(streams)
    }
}

/// A Mix's videos, each once: a Mix repeats some. A video with no Mix comes
/// back from yt-dlp as the video alone, with no entries.
fn mix_videos(list: Playlist) -> Result<Vec<Video>> {
    let mut seen = HashSet::new();
    let videos: Vec<Video> = list
        .entries
        .into_iter()
        .filter_map(|e| e.video(None, None))
        .filter(|v| seen.insert(v.id.clone()))
        .collect();
    if videos.is_empty() {
        bail!("without an account, YouTube makes them only for music videos");
    }
    Ok(videos)
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
    // yt-dlp's advice is for its own command line: cookies (tuitube never
    // signs in), `-t sleep`, `yt-dlp -U`. What happened comes before it.
    let line = ADVICE
        .iter()
        .filter_map(|advice| line.find(advice))
        .min()
        .map_or(line, |at| line[..at].trim_end_matches(['.', ';', ',', ' ']));
    text::first_chars(&text::clean(line), 200).to_string()
}

/// Where yt-dlp's advice starts in an error.
const ADVICE: [&str; 5] = [
    " Use --cookies",
    " It is recommended to use",
    "; please report this issue",
    " Confirm you are on the latest version",
    " Also see ",
];

/// yt-dlp older than this many days is worth updating when something fails:
/// YouTube changes something that breaks it six to eight times a year.
const OLD_AFTER_DAYS: i64 = 60;

/// How many days ago yt-dlp `version` (`2026.08.19`, or a nightly's
/// `2026.08.19.232419`) came out; `None` if it isn't a date.
fn days_old(version: &str, today: chrono::NaiveDate) -> Option<i64> {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().ok());
    let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
    let released = chrono::NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?;
    Some((today - released).num_days()).filter(|&days| days >= 0)
}

/// What to say when yt-dlp `version`, at `path`, is old enough that
/// updating it may fix what failed; `None` while it's recent.
pub fn old_notice(version: &str, path: &Path, today: chrono::NaiveDate) -> Option<String> {
    let days = days_old(version, today).filter(|&days| days > OLD_AFTER_DAYS)?;
    let now = crate::app::now();
    Some(format!(
        "yt-dlp came out {} ({version}), and YouTube breaks old ones: {}",
        video::age(now.saturating_sub(days * 86_400), now),
        update_hint(path)
    ))
}

/// How to update yt-dlp, from where it is installed. tuitube never updates
/// it itself: `yt-dlp -U` would change a program it runs.
fn update_hint(path: &Path) -> &'static str {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let real = real.to_string_lossy();
    if real.contains("/Cellar/")
        || real.starts_with("/opt/homebrew/")
        || real.starts_with("/home/linuxbrew/")
    {
        "brew upgrade yt-dlp"
    } else if real.contains("pipx") {
        "pipx upgrade yt-dlp"
    } else if real.contains("WinGet") {
        "winget upgrade yt-dlp.yt-dlp"
    } else if real.starts_with("/usr/") {
        "update it with your package manager"
    } else {
        "update it the way you installed it"
    }
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

/// A list from yt-dlp's JSON, keeping the items that read and skipping the
/// rest: one odd entry (a negative width, a `null`) doesn't lose the list,
/// and something else where a list goes is no list, not an unreadable
/// answer (a video with odd chapters still plays).
pub(crate) fn lenient<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let serde_json::Value::Array(items) = serde_json::Value::deserialize(deserializer)? else {
        return Ok(Vec::new());
    };
    Ok(items
        .into_iter()
        .filter_map(|item| serde_json::from_value(item).ok())
        .collect())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Playlist {
    title: Option<String>,
    channel_id: Option<String>,
    channel: Option<String>,
    uploader: Option<String>,
    #[serde(deserialize_with = "lenient")]
    thumbnails: Vec<Thumbnail>,
    #[serde(deserialize_with = "lenient")]
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
    #[serde(deserialize_with = "lenient")]
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
            description: video::lines(self.description.as_deref().unwrap_or(""), MAX_DESCRIPTION),
            published: self
                .timestamp
                .or(self.release_timestamp)
                .filter(|t| (0.0..=MAX_TIMESTAMP).contains(t))
                .map(|t| t as i64),
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
    #[serde(deserialize_with = "lenient")]
    requested_formats: Vec<Format>,
    #[serde(deserialize_with = "lenient")]
    chapters: Vec<RawChapter>,
    subtitles: serde_json::Value,
    automatic_captions: serde_json::Value,
    // The format chosen, when it's one stream (`requested_formats` is
    // empty then).
    vcodec: Option<String>,
    width: Option<f64>,
    height: Option<f64>,
    fps: Option<f64>,
    abr: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Format {
    url: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    http_headers: Option<Headers>,
    width: Option<f64>,
    height: Option<f64>,
    fps: Option<f64>,
    /// Kilobits a second of sound.
    abr: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawChapter {
    start_time: Option<f64>,
    title: Option<String>,
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
        let (video, audio, headers, quality) = match self.requested_formats.as_slice() {
            [] => {
                let quality = if has(&self.vcodec) {
                    picture_label(self.width, self.height, self.fps)
                } else {
                    sound_label(self.abr)
                };
                (self.url, None, self.http_headers, quality)
            }
            formats => {
                let video = formats.iter().find(|f| has(&f.vcodec));
                let audio = formats.iter().find(|f| has(&f.acodec) && !has(&f.vcodec));
                match (video, audio) {
                    (Some(v), audio) => (
                        v.url.clone(),
                        audio.and_then(|a| a.url.clone()),
                        v.http_headers.clone(),
                        picture_label(v.width, v.height, v.fps),
                    ),
                    (None, Some(a)) => (
                        a.url.clone(),
                        None,
                        a.http_headers.clone(),
                        sound_label(a.abr),
                    ),
                    (None, None) => (
                        formats[0].url.clone(),
                        None,
                        formats[0].http_headers.clone(),
                        None,
                    ),
                }
            }
        };
        let video = video.context("yt-dlp found nothing to play")?;
        if !stream_url_allowed(&video) || audio.as_deref().is_some_and(|a| !stream_url_allowed(a)) {
            bail!("yt-dlp gave a stream that isn't on YouTube's servers");
        }
        // On one line: a line break in it would start another HTTP header.
        let user_agent = headers
            .and_then(|h| h.user_agent)
            .map(|ua| video::one_line(&ua, 300));
        let duration = self.duration.filter(|d| d.is_finite() && *d > 0.0);
        let chapters = self
            .chapters
            .into_iter()
            .filter_map(|c| {
                Some(Chapter {
                    start: c.start_time?,
                    title: video::one_line(c.title.as_deref().unwrap_or(""), MAX_CHAPTER),
                })
            })
            .collect();
        Ok(Streams {
            video,
            audio,
            user_agent,
            duration,
            chapters: chapters::tidy(chapters, duration),
            quality,
            info: None,
            captions: captions(&self.subtitles, &self.automatic_captions),
        })
    }
}

/// A sane size, rate or bitrate from yt-dlp's JSON.
fn plausible(n: Option<f64>) -> Option<f64> {
    n.filter(|n| n.is_finite() && *n > 0.0 && *n < 100_000.0)
}

/// A picture's quality as YouTube writes it: its lines across the narrower
/// side (an upright Short's 1080p is 1080 wide), and its frame rate if it's
/// more than 30.
fn picture_label(width: Option<f64>, height: Option<f64>, fps: Option<f64>) -> Option<String> {
    let lines = match (plausible(width), plausible(height)) {
        (Some(w), Some(h)) => w.min(h),
        (None, Some(h)) => h,
        _ => return None,
    };
    let fps = plausible(fps)
        .filter(|f| *f > 31.0)
        .map_or(String::new(), |f| format!("{:.0}", f));
    Some(format!("{lines:.0}p{fps}"))
}

/// Sound's quality: its bitrate.
fn sound_label(kbps: Option<f64>) -> Option<String> {
    plausible(kbps).map(|k| format!("{k:.0} kbps"))
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
        assert!(streams.chapters.is_empty());

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
    fn chapters_and_the_quality_played_come_with_the_streams() {
        let json = r#"{"duration": 600.0, "requested_formats": [
            {"url": "https://rr1.googlevideo.com/v?x=1", "vcodec": "vp9", "acodec": "none",
             "width": 1080, "height": 1920, "fps": 59.94},
            {"url": "https://rr1.googlevideo.com/v?x=2", "vcodec": "none", "acodec": "opus", "abr": 121.4}
        ], "chapters": [
            {"start_time": 120.0, "end_time": 600.0, "title": "Two \u001b]52;c;aGk=\u0007\nlines"},
            {"start_time": 0, "end_time": 120.0, "title": "Intro"},
            {"start_time": "soon", "title": "not a time"},
            {"title": "no start"},
            null,
            {"start_time": 9999.0, "title": "after the end"},
            {"start_time": 300.5}
        ]}"#;
        let info: VideoInfo = serde_json::from_str(json).unwrap();
        let streams = info.streams().unwrap();
        assert_eq!(streams.quality.as_deref(), Some("1080p60"), "upright");
        let chapters: Vec<(f64, &str)> = streams
            .chapters
            .iter()
            .map(|c| (c.start, c.title.as_str()))
            .collect();
        assert_eq!(
            chapters,
            [(0.0, "Intro"), (120.0, "Two ]52;c;aGk= lines"), (300.5, "")],
            "in order, cleaned, the odd ones dropped"
        );

        // Sound only: its bitrate.
        let json = r#"{"url": "https://rr1.googlevideo.com/v?x=2", "vcodec": "none",
            "acodec": "opus", "abr": 67.8, "chapters": "none"}"#;
        let streams = serde_json::from_str::<VideoInfo>(json)
            .unwrap()
            .streams()
            .unwrap();
        assert_eq!(streams.quality.as_deref(), Some("68 kbps"));
        assert!(streams.chapters.is_empty(), "a list that isn't one");

        assert_eq!(
            picture_label(Some(1920.0), Some(1080.0), Some(30.0)).as_deref(),
            Some("1080p")
        );
        assert_eq!(
            picture_label(None, Some(720.0), None).as_deref(),
            Some("720p")
        );
        assert_eq!(picture_label(Some(-1.0), Some(f64::NAN), None), None);
        assert_eq!(sound_label(Some(f64::INFINITY)), None);
    }

    #[test]
    fn the_format_follows_the_picture_and_sound_quality() {
        let low = Quality::Video(720, AudioQuality::Low).format();
        let choices: Vec<&str> = low.split('/').collect();
        assert_eq!(
            choices[..2],
            [
                "bv*[aspect_ratio<1][width<=720][vcodec!^=av01]+ba[abr<=80]",
                "bv*[aspect_ratio<1][width<=720][vcodec!^=av01]+ba",
            ],
            "an upright video is measured across; small sound, else any"
        );
        assert!(choices.contains(&"bv*[height<=720][vcodec!^=av01]+ba[abr<=80]"));
        assert_eq!(choices[choices.len() - 2..], ["b[height<=720]", "b"]);
        let high = Quality::Video(1080, AudioQuality::High).format();
        assert!(!high.contains("abr"), "{high}");
        assert_eq!(high.split('/').count(), 6);
        assert_eq!(
            Quality::AudioOnly(AudioQuality::Low).format(),
            "ba[abr<=80]/ba/b"
        );
        assert_eq!(Quality::AudioOnly(AudioQuality::High).format(), "ba/b");
    }

    #[test]
    fn an_odd_entry_is_skipped_not_the_whole_list() {
        let json = r#"{"entries": [
            {"id": "aaaaaaaaaa1", "thumbnails": [{"url": "https://i.ytimg.com/a.jpg", "width": -1}]},
            null,
            {"id": "aaaaaaaaaa2", "release_timestamp": -99999999999999999999, "view_count": "lots"},
            {"id": "aaaaaaaaaa3", "timestamp": 1.0e30}
        ], "thumbnails": null}"#;
        let list: Playlist = serde_json::from_str(json).unwrap();
        let videos: Vec<Video> = list
            .entries
            .into_iter()
            .filter_map(|e| e.video(None, None))
            .collect();
        let ids: Vec<_> = videos.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(
            ids,
            ["aaaaaaaaaa1", "aaaaaaaaaa3"],
            "the second's view count isn't a number"
        );
        assert_eq!(videos[1].published, None, "a date past 2100 isn't believed");
    }

    #[test]
    fn a_mix_lists_each_video_once_and_a_video_without_one_is_an_error() {
        let json = r#"{"_type": "playlist", "id": "RDdQw4w9WgXcQ", "entries": [
            {"id": "dQw4w9WgXcQ", "title": "Seed", "channel_id": "UCuAXFkgsw1L7xaCfnd5JJOw"},
            {"id": "izGwDsrQ1eQ", "title": "Next"},
            {"id": "dQw4w9WgXcQ", "title": "Seed again"},
            {"id": "PLxxxxxxxxxxxxxxxxxx", "title": "not a video"}
        ]}"#;
        let videos = mix_videos(serde_json::from_str(json).unwrap()).unwrap();
        let ids: Vec<_> = videos.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["dQw4w9WgXcQ", "izGwDsrQ1eQ"]);
        assert_eq!(videos[0].title, "Seed", "the first is kept");

        let no_mix = r#"{"_type": "video", "id": "jNQXAC9IVRw", "title": "Me at the zoo"}"#;
        assert!(mix_videos(serde_json::from_str(no_mix).unwrap()).is_err());
    }

    #[test]
    fn errors_are_shortened_for_the_status_bar() {
        let stderr = "WARNING: something\nERROR: [youtube] dQw4w9WgXcQ: Sign in to confirm you're not a bot\n";
        assert_eq!(error_line(stderr), "Sign in to confirm you're not a bot");
        assert_eq!(error_line("weird\u{1b}[2J"), "weird[2J");
        // yt-dlp's advice for its own command line is left out: tuitube
        // never uses cookies, and never runs `yt-dlp -U`.
        let age = "ERROR: [youtube] dQw4w9WgXcQ: Sign in to confirm your age. This video may be inappropriate for some users. Use --cookies-from-browser or --cookies for the authentication. See  https://github.com/yt-dlp/yt-dlp/wiki/FAQ#how-do-i-pass-cookies-to-yt-dlp  for how to manually pass cookies. Also see  https://github.com/yt-dlp/yt-dlp/wiki/Extractors#exporting-youtube-cookies  for tips on effectively exporting YouTube cookies";
        assert_eq!(
            error_line(age),
            "Sign in to confirm your age. This video may be inappropriate for some users"
        );
        let broken = "ERROR: [youtube] dQw4w9WgXcQ: Unable to extract nsig; please report this issue on  https://github.com/yt-dlp/yt-dlp/issues?q= , filling out the appropriate issue template. Confirm you are on the latest version using  yt-dlp -U";
        assert_eq!(error_line(broken), "Unable to extract nsig");
    }

    #[test]
    fn subtitles_are_youtubes_webvtt_and_the_ones_in_your_language_chosen() {
        let vtt = |lang: &str| {
            serde_json::json!([
                {"ext": "json3", "url": format!("https://www.youtube.com/api/timedtext?lang={lang}&fmt=json3")},
                {"ext": "vtt", "url": format!("https://www.youtube.com/api/timedtext?lang={lang}&fmt=vtt"), "name": format!("Name {lang}")}
            ])
        };
        let manual = serde_json::json!({
            "en-GB": vtt("en-GB"),
            "de-DE": vtt("de-DE"),
            "live_chat": [{"ext": "vtt", "url": "https://www.youtube.com/live_chat"}],
            "evil": [{"ext": "vtt", "url": "https://evil.example/x.vtt"}],
            "bad lang!": vtt("x"),
        });
        let auto = serde_json::json!({"fr-orig": vtt("fr"), "en": vtt("en"), "fr": vtt("fr-tr"), "ja": "odd"});
        let tracks = captions(&manual, &auto);
        let langs: Vec<(&str, bool)> = tracks.iter().map(|c| (c.lang.as_str(), c.auto)).collect();
        assert_eq!(
            langs,
            [
                ("de-DE", false),
                ("en-GB", false),
                ("en", true),
                ("fr", true),
                ("fr-orig", true)
            ]
        );
        assert!(tracks.iter().all(|c| c.url.ends_with("fmt=vtt")));
        assert_eq!(tracks[0].name, "Name de-DE");
        let pick = |langs: &[&str]| {
            let langs: Vec<String> = langs.iter().map(|l| l.to_string()).collect();
            pick_caption(&tracks, &langs).map(|c| c.lang.as_str())
        };
        assert_eq!(pick(&["en"]), Some("en-GB"), "the uploader's first");
        assert_eq!(pick(&["fr", "en"]), Some("fr-orig"), "then as spoken");
        assert_eq!(pick(&["ja", "de"]), Some("de-DE"));
        assert_eq!(pick(&["ko"]), None);
        assert!(captions(&serde_json::json!("odd"), &serde_json::Value::Null).is_empty());
    }

    #[test]
    fn comments_are_the_top_ones_cleaned_without_replies() {
        let json = r#"{"comments": [
            {"text": "First!\u001b[2J\n\n\nreally", "author": "@a\u202e", "like_count": 12, "timestamp": 1700000000,
             "is_pinned": true, "author_is_uploader": true, "parent": "root"},
            {"text": "A reply", "author": "@b", "parent": "Ugx123"},
            {"text": "", "author": "@c", "parent": "root"},
            {"text": "No numbers", "author": "@d", "like_count": -5, "timestamp": 1e20},
            "odd"
        ]}"#;
        let found: WithComments = serde_json::from_str(json).unwrap();
        let shown = comments(found.comments);
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].text, "First![2J\n\nreally");
        assert_eq!(shown[0].author, "@a");
        assert!(shown[0].pinned && shown[0].by_uploader);
        assert_eq!(
            (shown[0].likes, shown[0].at),
            (Some(12), Some(1_700_000_000))
        );
        assert_eq!((shown[1].likes, shown[1].at), (None, None));
    }

    #[test]
    fn an_old_yt_dlp_is_worth_updating_and_a_recent_one_isnt() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 11).unwrap();
        assert_eq!(days_old("2026.08.19", today), Some(53));
        assert_eq!(days_old("2026.08.19.232419", today), Some(53));
        assert_eq!(days_old("not a date", today), None);
        assert_eq!(days_old("2027.01.01", today), None, "from the future");
        let brew = Path::new("/opt/homebrew/bin/yt-dlp");
        assert_eq!(old_notice("2026.08.19", brew, today), None);
        let notice = old_notice("2026.06.01", brew, today).unwrap();
        assert!(
            notice.starts_with("yt-dlp came out 4 months ago (2026.06.01)"),
            "{notice}"
        );
        assert!(
            notice.ends_with("old ones: brew upgrade yt-dlp"),
            "{notice}"
        );
        assert_eq!(
            update_hint(Path::new("/home/me/.local/pipx/venvs/yt-dlp/bin/yt-dlp")),
            "pipx upgrade yt-dlp"
        );
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
        let page = yt
            .channel(&channel, Tab::Videos, 3, false, false)
            .await
            .unwrap();
        eprintln!(
            "channel: {} {:?} {} videos",
            page.title,
            page.avatar,
            page.videos.len()
        );
        assert!(!page.videos.is_empty());
        assert!(page.avatar.as_deref().is_some_and(image_url_allowed));
        // Rough dates, when asked for: what fills in for a feed that's down.
        let dated = yt
            .channel(&channel, Tab::Videos, 3, false, true)
            .await
            .unwrap();
        assert!(
            dated.videos.iter().all(|v| v.published.is_some()),
            "{dated:?}"
        );

        let version = yt.version().await.unwrap();
        eprintln!("yt-dlp {version}");
        assert!(days_old(&version, chrono::Local::now().date_naive()).is_some());

        let streams = yt
            .resolve(&first.id, Quality::Video(360, AudioQuality::Low))
            .await
            .unwrap();
        eprintln!(
            "streams: audio {} duration {:?} {:?}, {} chapters",
            streams.audio.is_some(),
            streams.duration,
            streams.quality,
            streams.chapters.len()
        );
        assert!(stream_url_allowed(&streams.video));
        eprintln!("captions: {}", streams.captions.len());
        // The same answer says what the video is, as a card shows it.
        let info = streams.info.as_ref().expect("the video's details");
        assert_eq!(info.id, first.id);
        assert!(!info.title.is_empty() && info.channel_id.is_some() && info.published.is_some());
        assert!(
            streams
                .quality
                .as_deref()
                .is_some_and(|q| q.starts_with("360p")
                    || q.starts_with("240p")
                    || q.starts_with("144p")),
            "{:?}",
            streams.quality
        );
        let sound = yt
            .resolve(&first.id, Quality::AudioOnly(AudioQuality::Low))
            .await
            .unwrap();
        assert!(sound.audio.is_none());
        let kbps = |s: &Streams| {
            s.quality
                .as_deref()
                .and_then(|q| q.strip_suffix(" kbps")?.parse::<u32>().ok())
        };
        let low = kbps(&sound).expect("a bitrate");
        assert!(low <= 80, "{low} kbps");
        let high = yt
            .resolve(&first.id, Quality::AudioOnly(AudioQuality::High))
            .await
            .unwrap();
        assert!(kbps(&high).unwrap() > low);
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_chapters() {
        // BekBrace's Rust course: 22 chapters, set in its description.
        let course = VideoId::parse("rQ_J9WH6CGk").unwrap();
        let streams = yt()
            .resolve(&course, Quality::AudioOnly(AudioQuality::Low))
            .await
            .unwrap();
        let chapters = &streams.chapters;
        eprintln!("chapters: {} from {:?}", chapters.len(), chapters.first());
        assert!(chapters.len() >= 10, "{chapters:?}");
        assert_eq!(chapters[0].start, 0.0);
        assert!(chapters.iter().all(|c| !c.title.is_empty()));
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_links() {
        let yt = yt();
        // YouTube's own channel, by its handle.
        let page = yt
            .named_channel(&ChannelName::handle("YouTube").unwrap(), 3)
            .await
            .unwrap();
        assert_eq!(page.id.as_str(), "UCBR8-60-B28hp2BmDPdntcQ");
        assert!(!page.videos.is_empty());
        // Its uploads, as a playlist.
        let uploads = PlaylistId::parse("UUBR8-60-B28hp2BmDPdntcQ").unwrap();
        let (title, videos) = yt.playlist(&uploads, 5).await.unwrap();
        eprintln!("playlist: {title} {} videos", videos.len());
        assert!(!title.is_empty());
        assert_eq!(videos.len(), 5);
        // What a Takeout list's ids are, in one look.
        let ids: Vec<VideoId> = videos.iter().map(|v| v.id.clone()).collect();
        let details = yt.details(&ids).await.unwrap();
        assert_eq!(details.len(), 5);
        assert!(
            details
                .iter()
                .all(|v| !v.title.is_empty() && v.duration.is_some())
        );
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_comments() {
        let found = yt()
            .comments(&VideoId::parse("dQw4w9WgXcQ").unwrap())
            .await
            .unwrap();
        eprintln!("{} comments, the first: {:?}", found.len(), found.first());
        assert!(found.len() >= 20);
        assert!(found.iter().all(|c| !c.text.is_empty()));
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_subtitles() {
        let streams = yt()
            .resolve(
                &VideoId::parse("dQw4w9WgXcQ").unwrap(),
                Quality::AudioOnly(AudioQuality::Low),
            )
            .await
            .unwrap();
        let wanted = ["en".to_string()];
        let english = pick_caption(&streams.captions, &wanted).expect("English subtitles");
        assert!(!english.auto, "{english:?}");
        assert!(
            streams
                .captions
                .iter()
                .any(|c| c.auto && c.lang == "en-orig")
        );
        assert!(streams.captions.iter().all(|c| stream_url_allowed(&c.url)));
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_mix() {
        let yt = yt();
        let music = VideoId::parse("dQw4w9WgXcQ").unwrap();
        let mix = yt.mix(&music, 20).await.unwrap();
        eprintln!("mix: {} videos, then “{}”", mix.len(), mix[1].title);
        assert_eq!(mix[0].id, music, "the video first");
        assert!(mix.len() >= 10 && mix.len() <= 20, "{}", mix.len());
        // Logged out, YouTube makes Mixes only for music.
        let zoo = VideoId::parse("jNQXAC9IVRw").unwrap();
        assert!(yt.mix(&zoo, 20).await.is_err());
    }

    #[tokio::test]
    #[ignore = "talks to YouTube"]
    async fn live_gone_channel() {
        let nobody = ChannelId::parse("UCxxxxxxxxxxxxxxxxxxxxxx").unwrap();
        let limit = Arc::new(Semaphore::new(1));
        let feed = crate::feed::fetch(&crate::feed::client(), &limit, &nobody).await;
        assert!(feed.unwrap_err().is::<crate::feed::NotFound>());
        let page = yt().channel(&nobody, Tab::Videos, 1, true, false).await;
        let error = format!("{:#}", page.unwrap_err());
        eprintln!("gone: {error}");
        assert!(error.contains("This channel does not exist"), "{error}");
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
