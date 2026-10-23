//! Playing a video: mpv, run as a separate program, given the stream URLs
//! tuitube resolved. Video opens in mpv's own window; sound only plays with
//! no window, controlled from tuitube's player bar.
//!
//! mpv starts with `--no-config` (none of the user's mpv.conf, input.conf
//! or scripts), `--terminal=no` (it never touches tuitube's terminal, and
//! never prints the signed stream URLs), `--ytdl=no` (it doesn't run a
//! second yt-dlp of its own) and `--tls-verify=yes` (mpv's own default
//! accepts any certificate). On Unix it's controlled over one end of a
//! socket pair handed to it as file descriptor 3
//! (`--input-ipc-client=fd://3`): there's no socket file another program
//! could connect to, and mpv quits when tuitube's end closes. mpv's control
//! protocol can run programs, so it must never be reachable by anyone else.
//!
//! On Unix, mpv starts idle with nothing about the video on its command
//! line, which every user of the computer can read (`ps`): the title, the
//! user agent and the URLs go over the socket. Windows has no socket yet,
//! so they're arguments there, after `--`; other users can't read another
//! user's command line on Windows.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::chapters::Chapter;
use crate::tools::{self, Tools};
use crate::visualizer::{self, Levels};
use crate::ytdlp::Streams;

/// How long mpv has to quit when asked, before it's killed.
const QUIT_GRACE: Duration = Duration::from_secs(3);

/// What mpv keeps of a stream of sound alone: ahead of where it plays
/// (about 17 minutes at 130 kbps), and behind it, to go back without
/// fetching it again.
const SOUND_AHEAD: &str = "16MiB";
const SOUND_BEHIND: &str = "8MiB";

/// The `request_id` of the visualizer's `af add`, so its answer says
/// whether the filter went in, and of each ask for its numbers. Other
/// commands go without one (mpv answers them as 0, and they're ignored).
const FILTER_ADDED: u64 = 1;
#[cfg_attr(not(unix), allow(dead_code))]
const SPECTRUM: u64 = 2;

/// Something mpv said, for the playback numbered `play`.
#[derive(Debug)]
pub struct PlayerEvent {
    pub play: u64,
    pub kind: PlayerEventKind,
}

#[derive(Debug, PartialEq)]
#[cfg_attr(not(unix), allow(dead_code))]
pub enum PlayerEventKind {
    Position(f64),
    Duration(f64),
    Paused(bool),
    /// mpv's volume, in percent: tuitube's, or changed in mpv's window.
    Volume(f64),
    Muted(bool),
    /// mpv started playing, or went on after a seek: its sound is set up,
    /// and a filter can go in.
    Restarted,
    /// The visualizer's filter went in (`true`), or couldn't (`false`: an
    /// FFmpeg without one of its filters).
    Visualizer(bool),
    /// The visualizer's numbers: each band's loudness.
    Spectrum(Levels),
    /// The video played to its end: mpv's `end-file` with reason `eof`, not
    /// `quit` (its window closed, or tuitube stopped it) or `error`.
    Ended,
    /// mpv couldn't play the video's streams: `end-file` with reason
    /// `error`. Now and then YouTube refuses the addresses yt-dlp was given
    /// (HTTP 403); new ones play.
    Failed,
    /// mpv exited, after the video ended or its window was closed.
    Exited,
    /// The system's ⏭ or ⏮ (a media key, Control Center, a notch app),
    /// passed on by mpv ([`REMOTE_KEYS`]).
    Remote(Remote),
}

/// What the system's ⏭ and ⏮ ask for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Remote {
    Next,
    Previous,
}

/// mpv publishes Now Playing for macOS (and Windows) itself, and maps the
/// system's ⏭ and ⏮ to its keys `NEXT` and `PREV`: by default `playlist-next`
/// and `playlist-prev`, which do nothing, since mpv plays one video. Bound to
/// a `script-message` instead, they come back to tuitube on the socket as a
/// `client-message` with these words.
#[cfg_attr(not(unix), allow(dead_code))]
const REMOTE_KEYS: [(&str, &str, Remote); 2] = [
    ("NEXT", "next", Remote::Next),
    ("PREV", "previous", Remote::Previous),
];

/// tuitube fills in the system's Now Playing that mpv publishes, with the
/// thumbnail ([`Player::add_cover`]) and the channel ([`file_to_load`]):
/// macOS's. Windows' takes them too, but tuitube can't give mpv anything
/// there yet (no socket), and mpv publishes none on Linux.
pub const NOW_PLAYING: bool = cfg!(target_os = "macos");

/// The first word of tuitube's `script-message`s.
#[cfg_attr(not(unix), allow(dead_code))]
const MESSAGE_FROM: &str = "tuitube";

pub struct Player {
    pub play: u64,
    /// tuitube can pause, seek and follow the playback: over mpv's socket,
    /// which Windows doesn't have yet. A field, not `cfg!(unix)` where it's
    /// asked, so tests check both ways on any system
    /// ([`Player::set_controllable`]).
    controllable: bool,
    #[cfg(unix)]
    control: Option<tokio::sync::mpsc::UnboundedSender<Control>>,
    /// Tells the task that owns mpv's process to end it.
    stop: Option<oneshot::Sender<()>>,
}

pub struct Start<'a> {
    /// mpv, or pepper-play, which takes mpv's options and socket
    /// (`pepper`).
    pub mpv: &'a Path,
    pub tools: &'a Tools,
    pub streams: &'a Streams,
    pub title: &'a str,
    pub start_at: Option<f64>,
    pub audio_only: bool,
    /// The subtitles' URL (WebVTT on YouTube's server), if any, and
    /// whether they show at first.
    pub subtitles: Option<&'a str>,
    pub subtitles_shown: bool,
    /// How loud, in percent (0 to 100), and whether muted.
    pub volume: u32,
    pub muted: bool,
    /// Starts paused: the headphones were unplugged since you last played
    /// something.
    pub paused: bool,
    /// Played over and over (repeat one).
    pub looped: bool,
    /// The channel, shown under the title in Now Playing ([`file_to_load`]);
    /// none where tuitube doesn't fill that in.
    pub artist: Option<&'a str>,
    /// In the system's Now Playing, with the media keys (`now_playing`).
    pub now_playing: bool,
    /// The picture plays inside Pepper, through pepper-play (`mpv`).
    pub pepper: Option<Pepper<'a>>,
}

/// What pepper-play needs to play into Pepper ([`crate::pepper`]). Pepper
/// tells it the box's size itself.
pub struct Pepper<'a> {
    /// Pepper's port, from `PEPPER_PORT`: pepper-play gets it in its
    /// environment, which otherwise has only `tools::child_env`'s.
    pub port: &'a std::ffi::OsStr,
    /// The token the picture's box is placed with: over the socket only.
    pub token: &'a str,
}

impl Player {
    pub fn start(
        play: u64,
        start: Start,
        tx: UnboundedSender<crate::app::AppEvent>,
    ) -> Result<Self> {
        let mut command = tokio::process::Command::new(start.mpv);
        command
            .args(args(&start))
            .env_clear()
            .envs(tools::child_env(start.tools))
            .envs(allocator_env(&start))
            .envs(start.pepper.as_ref().map(|p| ("PEPPER_PORT", p.port)))
            .envs(video_log(&start).map(|_| ("PEPPER_PLAY_DEBUG", "1")))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(video_log(&start).map_or_else(Stdio::null, Stdio::from))
            // If tuitube ends without stopping it, mpv goes too.
            .kill_on_drop(true);
        if let Some(home) = dirs::home_dir() {
            command.current_dir(home);
        }

        #[cfg(unix)]
        let (child, control) = {
            use std::os::fd::AsRawFd;
            let starting = tools::starting();
            let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
            let fd = theirs.as_raw_fd();
            // SAFETY: only dup2 or fcntl runs between fork and exec, both
            // async-signal-safe. dup2 also clears close-on-exec on fd 3.
            unsafe {
                command.pre_exec(move || {
                    let ok = if fd == 3 {
                        libc::fcntl(3, libc::F_SETFD, 0) != -1
                    } else {
                        libc::dup2(fd, 3) != -1
                    };
                    if !ok {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().with_context(|| cannot_start(&start))?;
            drop(starting);
            drop(theirs);
            ours.set_nonblocking(true)?;
            let stream = tokio::net::UnixStream::from_std(ours)?;
            let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
            spawn_ipc(play, stream, control_rx, tx.clone());
            for line in commands(&start) {
                let _ = control_tx.send(Control::Line(line));
            }
            (child, control_tx)
        };
        #[cfg(not(unix))]
        let child = {
            let _starting = tools::starting();
            command.spawn().with_context(|| cannot_start(&start))?
        };

        let (stop_tx, stop_rx) = oneshot::channel();
        tokio::spawn(own(play, child, stop_rx, tx));
        Ok(Self {
            play,
            controllable: cfg!(unix),
            #[cfg(unix)]
            control: Some(control),
            stop: Some(stop_tx),
        })
    }

    /// A player with no mpv behind it, for the demo's player bar.
    pub fn detached(play: u64) -> Self {
        Self {
            play,
            // In tests, as on Unix on every system: a test of Windows'
            // playback says so (`set_controllable`), so no test passes on
            // one system and fails on another.
            controllable: cfg!(unix) || cfg!(test),
            #[cfg(unix)]
            control: None,
            stop: None,
        }
    }

    /// A player with a pretend mpv behind it, which takes what it's sent
    /// and says nothing, for tests of what's asked of mpv.
    #[cfg(test)]
    pub fn pretend(play: u64) -> Self {
        let (stop, _) = oneshot::channel();
        let mut player = Self::detached(play);
        player.stop = Some(stop);
        player
    }

    /// Whether an mpv plays it: not the demo's.
    pub fn has_mpv(&self) -> bool {
        self.stop.is_some()
    }

    /// Whether tuitube can pause, seek and follow the playback (not on
    /// Windows yet).
    pub fn controllable(&self) -> bool {
        self.controllable
    }

    /// Plays as on Unix (`true`) or as on Windows (`false`), whichever
    /// system runs the test.
    #[cfg(test)]
    pub fn set_controllable(&mut self, controllable: bool) {
        self.controllable = controllable;
    }

    fn send(&self, command: serde_json::Value) {
        #[cfg(unix)]
        self.control(Control::Line(line(command)));
        #[cfg(not(unix))]
        let _ = command;
    }

    /// A command whose answer tuitube reads, by its number.
    fn send_numbered(&self, command: serde_json::Value, id: u64) {
        #[cfg(unix)]
        self.control(Control::Line(line_numbered(command, id)));
        #[cfg(not(unix))]
        let _ = (command, id);
    }

    #[cfg(unix)]
    fn control(&self, message: Control) {
        if let Some(control) = &self.control {
            let _ = control.send(message);
        }
    }

    pub fn toggle_pause(&self) {
        self.send(serde_json::json!(["cycle", "pause"]));
    }

    /// Pauses, if it plays: unlike [`Player::toggle_pause`], never plays.
    pub fn pause(&self) {
        self.send(serde_json::json!(["set_property", "pause", true]));
    }

    /// mpv's window fills the screen, or goes back.
    pub fn toggle_fullscreen(&self) {
        self.send(serde_json::json!(["cycle", "fullscreen"]));
    }

    pub fn seek(&self, seconds: i32) {
        self.send(serde_json::json!(["seek", seconds, "relative"]));
    }

    /// Goes to `seconds` from the start, to the frame (a SponsorBlock skip
    /// lands where the segment ends, not at the keyframe before).
    pub fn seek_to(&self, seconds: f64) {
        if seconds.is_finite() && seconds >= 0.0 {
            self.send(serde_json::json!(["seek", seconds, "absolute+exact"]));
        }
    }

    /// Shows or hides the subtitles.
    pub fn show_subtitles(&self, shown: bool) {
        self.send(serde_json::json!(["set_property", "sub-visibility", shown]));
    }

    /// Sets the volume, in percent.
    pub fn set_volume(&self, percent: u32) {
        self.send(serde_json::json!([
            "set_property",
            "volume",
            percent.min(100)
        ]));
    }

    pub fn set_muted(&self, muted: bool) {
        self.send(serde_json::json!(["set_property", "mute", muted]));
    }

    /// Plays the video again from its start each time it ends (repeat
    /// one), or lets it end.
    pub fn set_looped(&self, looped: bool) {
        let value = if looped { "inf" } else { "no" };
        self.send(serde_json::json!(["set_property", "loop-file", value]));
    }

    /// Puts the visualizer's filter into the sound playing. mpv says whether
    /// it went in ([`PlayerEventKind::Visualizer`]); if it didn't, the sound
    /// plays on as it was. Sent once the sound plays: a filter mpv can't
    /// make before then stops the sound altogether.
    pub fn add_visualizer(&self) {
        let filter = visualizer::filter();
        self.send_numbered(serde_json::json!(["af", "add", filter]), FILTER_ADDED);
    }

    /// Takes the visualizer's filter out of the sound, and stops asking
    /// for its numbers.
    pub fn remove_visualizer(&self) {
        self.follow_spectrum(false);
        let label = format!("@{}", visualizer::LABEL);
        self.send(serde_json::json!(["af", "remove", label]));
    }

    /// Asks mpv for the visualizer's numbers every [`visualizer::EVERY`]
    /// (each comes back as [`PlayerEventKind::Spectrum`]), or stops.
    pub fn follow_spectrum(&self, on: bool) {
        #[cfg(unix)]
        self.control(Control::Spectrum(on));
        #[cfg(not(unix))]
        let _ = on;
    }

    /// Gives mpv the video's chapters, for the marks on its window's seek
    /// bar and its own chapter keys. mpv keeps only the ones before the
    /// length it knows, so this waits for mpv to say the length. Titles go
    /// as JSON strings, as the video's does.
    pub fn set_chapters(&self, chapters: &[Chapter]) {
        self.send(chapters_command(chapters));
    }

    /// Puts the video playing in Now Playing, with the media keys, or takes
    /// it out: at once, as mpv's controls go in or out.
    pub fn set_now_playing(&self, on: bool) {
        let on = if on { "yes" } else { "no" };
        self.send(serde_json::json!(["set", "input-media-keys", on]));
    }

    /// Adds the Now Playing picture, a file tuitube wrote: as album art,
    /// listed but not shown in place of the video. mpv's Now Playing (macOS)
    /// takes the first such track's file. Only once mpv plays: before, there
    /// is no file to add it to.
    pub fn add_cover(&self, path: &Path) {
        if let Some(command) = cover_command(path) {
            self.send(command);
        }
    }

    /// Stops playback: mpv is asked to quit, and killed if it hasn't
    /// within `QUIT_GRACE`.
    pub fn stop(&mut self) {
        self.send(serde_json::json!(["quit"]));
        #[cfg(unix)]
        {
            // Closing our end makes mpv quit even if it missed the command.
            self.control = None;
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Owns mpv's process: waits for it to end, or for `stop`, then gives it
/// `QUIT_GRACE` before killing it through its handle (never by process id,
/// which another program may have by then). Says when it's gone.
async fn own(
    play: u64,
    mut child: tokio::process::Child,
    stop: oneshot::Receiver<()>,
    tx: UnboundedSender<crate::app::AppEvent>,
) {
    tokio::select! {
        _ = child.wait() => {}
        // Asked to stop, or the Player is gone.
        _ = stop => {
            // Windows has no way to ask mpv to quit yet.
            let grace = if cfg!(unix) { QUIT_GRACE } else { Duration::ZERO };
            if tokio::time::timeout(grace, child.wait()).await.is_err() {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    }
    let _ = tx.send(crate::app::AppEvent::Player(PlayerEvent {
        play,
        kind: PlayerEventKind::Exited,
    }));
}

/// For finding out why pepper-play stopped: with `TT_VIDEO_LOG` set to a
/// file, what it and mpv say goes there (mpv's messages from verbose up),
/// not nowhere. Never for mpv. It holds the stream addresses, so it's made
/// readable by you alone, and a link there isn't followed.
fn video_log(start: &Start) -> Option<std::fs::File> {
    start.pepper.as_ref()?;
    let path = crate::config::var("TT_VIDEO_LOG")?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).ok()
}

fn cannot_start(start: &Start) -> String {
    let name = if start.pepper.is_some() {
        "pepper-play"
    } else {
        "mpv"
    };
    format!("cannot start {name}")
}

/// pepper-play's own message, as mpv passes any message to the program it
/// runs in: its arguments are strings.
#[cfg_attr(not(unix), allow(dead_code))]
fn video_token_command(token: &str) -> serde_json::Value {
    serde_json::json!(["script-message-to", "main", "pepper-token", token])
}

/// How mpv's memory is kept. Playing sound alone (not a picture), mpv 0.41
/// on macOS frees large buffers that macOS's allocator keeps for reuse but
/// never reuses: measured, mpv grew to over 800 MiB in a minute of sound
/// (380 MiB of a YouTube stream), and kept growing. Without that cache it
/// stays at 116 MiB, for the same CPU.
fn allocator_env(start: &Start) -> Option<(&'static str, &'static str)> {
    (start.audio_only && cfg!(target_os = "macos")).then_some(("MallocLargeCache", "0"))
}

/// mpv's arguments. Nothing about the video is in them on Unix; on Windows
/// the title, user agent and start go in options, the sound's URL in
/// `--audio-file=` and the picture's after `--`.
fn args(start: &Start) -> Vec<String> {
    let mut args: Vec<String> = [
        "--no-config",
        "--load-scripts=no",
        "--terminal=no",
        "--ytdl=no",
        "--tls-verify=yes",
        "--keep-open=no",
        "--save-position-on-quit=no",
        "--resume-playback=no",
        "--hwdec=auto-safe",
    ]
    .map(String::from)
    .into();
    // On macOS, mpv's CoreAudio output doesn't open for 48 kHz sound in
    // planar float, what YouTube's Opus and AAC decode to, and mpv falls
    // back to AVFoundation, which holds 2 s of sound: a volume change was
    // heard that late. The same float, interleaved, opens it.
    if cfg!(target_os = "macos") {
        args.push("--audio-format=float".into());
    }
    // mpv puts what plays in macOS's Now Playing, and takes the media keys,
    // unless told not to.
    if NOW_PLAYING && !start.now_playing {
        args.push("--input-media-keys=no".into());
    }
    if start.audio_only {
        args.push("--no-video".into());
        args.push("--force-window=no".into());
        // mpv reads ahead until 150 MiB, and keeps 50 MiB behind: hours of
        // sound alone, held in memory. A quarter of an hour ahead and some
        // minutes behind are plenty.
        args.push(format!("--demuxer-max-bytes={SOUND_AHEAD}"));
        args.push(format!("--demuxer-max-back-bytes={SOUND_BEHIND}"));
    } else {
        args.push("--force-window=immediate".into());
    }
    if cfg!(unix) {
        // Idle until the file comes over the socket, then quit after it.
        args.push("--idle=once".into());
        args.push("--input-ipc-client=fd://3".into());
        return args;
    }
    args.push("--idle=no".into());
    for (name, value) in settings(start) {
        args.push(format!("--{name}={value}"));
    }
    if let Some(audio) = &start.streams.audio {
        args.push(format!("--audio-file={audio}"));
    }
    if let Some(subtitles) = start.subtitles {
        args.push(format!("--sub-file={subtitles}"));
    }
    args.push("--".into());
    args.push(start.streams.video.clone());
    args
}

/// The options that describe this playback: its title (in mpv's window and
/// media controls), the user agent the streams expect, how a refused stream
/// is tried again, where to start, how loud, whether it plays over and over.
fn settings(start: &Start) -> Vec<(&'static str, String)> {
    let title = crate::video::one_line(start.title, 150);
    let mut settings = vec![
        ("force-media-title", title.clone()),
        // The window title expands `${…}` properties; `$$` is a plain `$`.
        ("title", format!("{} — tuitube", title.replace('$', "$$"))),
        ("volume", start.volume.min(100).to_string()),
        ("mute", if start.muted { "yes" } else { "no" }.into()),
        // YouTube refuses some stream addresses for a few seconds after
        // yt-dlp gets them (measured, yt-dlp 2026.10.21: its TV client's,
        // HTTP 403 for 2–4 s, then fine), and tuitube starts at once. mpv
        // doesn't fail the video then: a refused picture plays as sound
        // alone, refused sound as a silent picture. With this FFmpeg tries a
        // 403 again after 0, 1, 3 and 7 s (mpv's `reconnect_delay_max`), on
        // every open, seek and reconnect of the streams mpv opens itself (not
        // a live stream's HLS segments: FFmpeg doesn't pass it on to them).
        // The cost: an address that stays refused (expired, or a sound or
        // subtitle URL refused for good) fails 11 s later, not at once.
        ("stream-lavf-o", "reconnect_on_http_error=403".into()),
    ];
    // Before the properties are observed, as the volume: mpv's first word
    // on it is tuitube's.
    if start.paused {
        settings.push(("pause", "yes".into()));
    }
    if let Some(ua) = &start.streams.user_agent {
        settings.push(("user-agent", crate::video::one_line(ua, 300)));
    }
    if let Some(at) = start.start_at.filter(|s| s.is_finite() && *s > 0.0) {
        settings.push(("start", format!("{at:.0}")));
    }
    if start.subtitles.is_some() {
        let shown = if start.subtitles_shown { "yes" } else { "no" };
        settings.push(("sub-visibility", shown.into()));
    }
    // From its start again at its end, with no end-file in between.
    if start.looped {
        settings.push(("loop-file", "inf".into()));
    }
    settings
}

/// mpv's command adding the picture at `path` as album art, not selected
/// (`auto`). A path tuitube made, absolute, so mpv never takes it for a URL;
/// none if it isn't text.
fn cover_command(path: &Path) -> Option<serde_json::Value> {
    let path = path.to_str().filter(|_| path.is_absolute())?;
    Some(serde_json::json!({
        "name": "video-add",
        "url": path,
        "flags": "auto",
        "albumart": true,
    }))
}

/// mpv's command setting its list of chapters to `chapters`.
fn chapters_command(chapters: &[Chapter]) -> serde_json::Value {
    let list: Vec<serde_json::Value> = chapters
        .iter()
        .map(|c| serde_json::json!({ "title": c.title, "time": c.start }))
        .collect();
    serde_json::json!(["set_property", "chapter-list", list])
}

/// One command for mpv's control socket, as a line of JSON: strings are
/// JSON strings, so a title can't break out of its place.
#[cfg_attr(not(unix), allow(dead_code))]
fn line(command: serde_json::Value) -> String {
    format!("{}\n", serde_json::json!({ "command": command }))
}

/// A command whose answer tuitube reads: mpv answers with its `request_id`.
#[cfg_attr(not(unix), allow(dead_code))]
fn line_numbered(command: serde_json::Value, id: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({ "command": command, "request_id": id })
    )
}

/// What goes to the task writing to mpv's socket.
#[cfg(unix)]
enum Control {
    /// A line of JSON for mpv.
    Line(String),
    /// Ask for the visualizer's numbers every `visualizer::EVERY`, or stop.
    Spectrum(bool),
}

/// What's sent over the socket to play the video: the settings (with
/// `set`, which takes the value as is), the properties tuitube follows
/// (after the settings, so the volume mpv first says is tuitube's), the
/// sound's URL added to the list of audio files as one item (and the
/// subtitles' to the subtitle files), then the picture's URL.
#[cfg_attr(not(unix), allow(dead_code))]
fn commands(start: &Start) -> Vec<String> {
    let mut commands: Vec<String> = settings(start)
        .into_iter()
        .map(|(name, value)| line(serde_json::json!(["set", name, value])))
        .collect();
    commands.extend(
        [
            (1, "time-pos"),
            (2, "duration"),
            (3, "pause"),
            (4, "volume"),
            (5, "mute"),
        ]
        .into_iter()
        .map(|(id, property)| line(serde_json::json!(["observe_property", id, property]))),
    );
    commands.extend(REMOTE_KEYS.iter().map(|(key, word, _)| {
        line(serde_json::json!([
            "keybind",
            key,
            format!("script-message {MESSAGE_FROM} {word}")
        ]))
    }));
    // Before the video loads, so its first frame has somewhere to go.
    if let Some(pepper) = &start.pepper {
        commands.push(line(video_token_command(pepper.token)));
    }
    if let Some(audio) = &start.streams.audio {
        commands.push(line(serde_json::json!([
            "change-list",
            "audio-files",
            "append",
            audio
        ])));
    }
    // Loaded with the video, as one item of the list: mpv selects it.
    if let Some(subtitles) = start.subtitles {
        commands.push(line(serde_json::json!([
            "change-list",
            "sub-files",
            "append",
            subtitles
        ])));
    }
    commands.push(line(serde_json::json!([
        "loadfile",
        file_to_load(&start.streams.video, start.artist),
        "replace"
    ])));
    commands
}

/// What mpv loads: the video's URL, or with an artist, an EDL of that URL
/// alone with the artist as its tag. mpv's Now Playing takes the artist only
/// from the file's tags, which nothing else can set. The EDL is the shape
/// mpv's own ytdl_hook gives every YouTube stream (`!no_clip`, the stream as
/// it is; `!no_chapters`, none of mpv's own); measured on YouTube's streams,
/// it plays, seeks, filters and ends as the bare URL. Each text goes in after
/// `%n%`, its length in bytes, so nothing in it can end its field.
#[cfg_attr(not(unix), allow(dead_code))]
fn file_to_load(url: &str, artist: Option<&str>) -> String {
    let Some(artist) = artist else {
        return url.to_string();
    };
    let field = |text: &str| format!("%{}%{text}", text.len());
    let artist = crate::video::one_line(artist, 150);
    format!(
        "edl://!no_clip;!no_chapters;!global_tags,artist={};{};",
        field(&artist),
        field(url)
    )
}

#[cfg(unix)]
fn spawn_ipc(
    play: u64,
    stream: tokio::net::UnixStream,
    mut control: tokio::sync::mpsc::UnboundedReceiver<Control>,
    tx: UnboundedSender<crate::app::AppEvent>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (read, mut write) = stream.into_split();
    tokio::spawn(async move {
        let ask = line_numbered(
            serde_json::json!(["get_property", visualizer::property()]),
            SPECTRUM,
        );
        let mut every = tokio::time::interval(visualizer::EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut following = false;
        loop {
            let line = tokio::select! {
                message = control.recv() => match message {
                    Some(Control::Line(line)) => line,
                    Some(Control::Spectrum(on)) => {
                        following = on;
                        continue;
                    }
                    None => break,
                },
                _ = every.tick(), if following => ask.clone(),
            };
            if write.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
        // Dropping the writer closes our end: mpv quits.
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.len() > 64 * 1024 {
                continue;
            }
            if let Some(kind) = parse_event(&line) {
                let _ = tx.send(crate::app::AppEvent::Player(PlayerEvent { play, kind }));
            }
        }
    });
}

#[derive(serde::Deserialize)]
#[cfg(any(unix, test))]
struct IpcMessage {
    event: Option<String>,
    name: Option<String>,
    data: Option<serde_json::Value>,
    reason: Option<String>,
    request_id: Option<u64>,
    error: Option<String>,
    /// A `client-message`'s words. Any JSON: a message that isn't tuitube's
    /// is left alone, not a reason to drop the line.
    args: Option<serde_json::Value>,
}

/// The ⏭ or ⏮ in a `client-message`, if it's one of tuitube's own.
#[cfg(any(unix, test))]
fn remote(args: &serde_json::Value) -> Option<Remote> {
    let [from, word] = args.as_array()?.as_slice() else {
        return None;
    };
    if from != MESSAGE_FROM {
        return None;
    }
    REMOTE_KEYS
        .iter()
        .find(|(_, w, _)| word == *w)
        .map(|(_, _, remote)| *remote)
}

/// One of mpv's messages, if it's one tuitube follows.
#[cfg(any(unix, test))]
fn parse_event(line: &str) -> Option<PlayerEventKind> {
    let message: IpcMessage = serde_json::from_str(line).ok()?;
    match message.request_id {
        Some(FILTER_ADDED) => {
            let added = message.error.as_deref() == Some("success");
            return Some(PlayerEventKind::Visualizer(added));
        }
        Some(SPECTRUM) => {
            return visualizer::levels(message.data.as_ref()?).map(PlayerEventKind::Spectrum);
        }
        _ => {}
    }
    match message.event.as_deref() {
        Some("end-file") => {
            return match message.reason.as_deref() {
                Some("eof") => Some(PlayerEventKind::Ended),
                Some("error") => Some(PlayerEventKind::Failed),
                _ => None,
            };
        }
        Some("playback-restart") => return Some(PlayerEventKind::Restarted),
        Some("client-message") => {
            return remote(message.args.as_ref()?).map(PlayerEventKind::Remote);
        }
        _ => {}
    }
    if message.event.as_deref() != Some("property-change") {
        return None;
    }
    let data = message.data?;
    match message.name.as_deref()? {
        "time-pos" => data.as_f64().map(PlayerEventKind::Position),
        "duration" => data.as_f64().map(PlayerEventKind::Duration),
        "pause" => data.as_bool().map(PlayerEventKind::Paused),
        "volume" => data.as_f64().map(PlayerEventKind::Volume),
        "mute" => data.as_bool().map(PlayerEventKind::Muted),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streams() -> Streams {
        Streams {
            video: "https://rr1.googlevideo.com/videoplayback?v=1".into(),
            audio: Some("https://rr1.googlevideo.com/videoplayback?a=1,b=2".into()),
            user_agent: Some("Mozilla/5.0\nX-Injected: yes".into()),
            duration: Some(10.0),
            chapters: Vec::new(),
            quality: None,
            info: None,
            captions: Vec::new(),
        }
    }

    fn start<'a>(tools: &'a Tools, streams: &'a Streams, title: &'a str) -> Start<'a> {
        Start {
            mpv: Path::new("/usr/bin/mpv"),
            tools,
            streams,
            title,
            start_at: Some(42.4),
            audio_only: false,
            subtitles: None,
            subtitles_shown: false,
            volume: 80,
            muted: false,
            paused: false,
            looped: false,
            artist: None,
            now_playing: true,
            pepper: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn in_pepper_the_token_goes_over_the_socket_before_the_video() {
        let (tools, streams) = (Tools::default(), streams());
        let port = std::ffi::OsString::from("io.github.erictran308.pepper.1");
        let mut start = start(&tools, &streams, "A video");
        start.pepper = Some(Pepper {
            port: &port,
            token: "abc123",
        });
        let commands = commands(&start);
        let at = |needle: &str| commands.iter().position(|c| c.contains(needle)).unwrap();
        assert!(commands[at("pepper-token")].contains("abc123"));
        assert!(at("pepper-token") < at("loadfile"));
        assert!(!commands.iter().any(|c| c.contains("pepper-size")));
        assert!(!args(&start).iter().any(|a| a.contains("abc123")));
        assert_eq!(cannot_start(&start), "cannot start pepper-play");
    }

    #[cfg(unix)]
    #[test]
    fn nothing_about_the_video_is_on_mpvs_command_line() {
        let (tools, streams) = (Tools::default(), streams());
        let args = args(&start(&tools, &streams, "My secret video"));
        assert_eq!(args[0], "--no-config");
        for flag in [
            "--terminal=no",
            "--ytdl=no",
            "--tls-verify=yes",
            "--idle=once",
        ] {
            assert!(args.contains(&flag.to_string()), "{flag}");
        }
        let all = args.join(" ");
        assert!(!all.contains("secret"), "{all}");
        assert!(!all.contains("googlevideo"), "{all}");
        assert!(!all.contains("Mozilla"), "{all}");
    }

    #[test]
    fn the_socket_gets_the_video_as_json_strings() {
        let (tools, streams) = (Tools::default(), streams());
        let title = "\"]}{\"command\":[\"run\",\"sh\"]} ${path} --script=x";
        let lines = commands(&start(&tools, &streams, title));
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
            assert!(parsed["command"].is_array(), "{line}");
            assert!(!line.trim_end().contains('\n'));
        }
        let set = |name: &str| {
            lines
                .iter()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .find(|v| v["command"][0] == "set" && v["command"][1] == name)
                .map(|v| v["command"][2].as_str().unwrap().to_string())
        };
        assert_eq!(
            set("force-media-title").unwrap(),
            title,
            "kept whole, as one string"
        );
        assert!(
            set("title").unwrap().contains("$${path}"),
            "no property expansion"
        );
        assert_eq!(
            set("user-agent").unwrap(),
            "Mozilla/5.0 X-Injected: yes",
            "one line"
        );
        assert_eq!(set("start").unwrap(), "42");
        assert_eq!(set("volume").unwrap(), "80");
        assert_eq!(set("mute").unwrap(), "no");
        assert_eq!(set("pause"), None, "plays");
        let first_followed = lines
            .iter()
            .position(|l| l.contains("observe_property"))
            .unwrap();
        let last_set = lines.iter().rposition(|l| l.contains("\"set\"")).unwrap();
        assert!(last_set < first_followed, "mpv first says tuitube's volume");
        // Paused from its start: set with the volume, before mpv says.
        let mut held = start(&tools, &streams, title);
        held.paused = true;
        let held = commands(&held);
        let pause = held
            .iter()
            .position(|l| l.contains(r#"["set","pause","yes"]"#));
        let followed = held.iter().position(|l| l.contains("observe_property"));
        assert!(pause.expect("paused") < followed.unwrap());
        let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        assert_eq!(last["command"][0], "loadfile");
        assert_eq!(last["command"][1], streams.video.as_str());
        let audio: serde_json::Value = serde_json::from_str(&lines[lines.len() - 2]).unwrap();
        assert_eq!(
            audio["command"][2], "append",
            "one item: commas don't split it"
        );
    }

    #[test]
    fn a_stream_refused_at_first_is_tried_again() {
        let (tools, streams) = (Tools::default(), streams());
        let start = start(&tools, &streams, "A video");
        let retry = r#"["set","stream-lavf-o","reconnect_on_http_error=403"]"#;
        let lines = commands(&start);
        let at = |needle: &str| lines.iter().position(|l| l.contains(needle));
        assert!(at(retry).expect("set") < at("loadfile").unwrap());
        // Windows's arguments come from the same settings.
        let setting = ("stream-lavf-o", "reconnect_on_http_error=403".to_string());
        assert!(settings(&start).contains(&setting));
    }

    #[test]
    fn macos_plays_through_coreaudio_where_volume_changes_at_once() {
        let (tools, streams) = (Tools::default(), streams());
        let args = args(&start(&tools, &streams, "A video"));
        let float = args.iter().any(|a| a == "--audio-format=float");
        assert_eq!(float, cfg!(target_os = "macos"));
    }

    #[test]
    fn mpv_stays_out_of_now_playing_when_its_turned_off() {
        let (tools, streams) = (Tools::default(), streams());
        let mut start = start(&tools, &streams, "A video");
        let out = |start: &Start| args(start).iter().any(|a| a == "--input-media-keys=no");
        assert!(!out(&start), "on: mpv's own way");
        start.now_playing = false;
        assert_eq!(out(&start), NOW_PLAYING, "off, where tuitube fills it in");
    }

    #[test]
    fn sound_alone_on_macos_gives_back_what_mpv_frees() {
        let (tools, streams) = (Tools::default(), streams());
        let mut start = start(&tools, &streams, "A video");
        assert_eq!(allocator_env(&start), None, "a picture doesn't grow");
        start.audio_only = true;
        assert_eq!(
            allocator_env(&start).is_some(),
            cfg!(target_os = "macos"),
            "macOS's allocator"
        );
    }

    #[test]
    fn repeat_one_has_mpv_play_the_video_over_and_over() {
        let (tools, streams) = (Tools::default(), streams());
        let mut start = start(&tools, &streams, "A video");
        let loops = |start: &Start| settings(start).contains(&("loop-file", "inf".into()));
        assert!(!loops(&start), "it ends unless asked");
        start.looped = true;
        assert!(loops(&start));
        let set = r#"{"command":["set","loop-file","inf"]}"#;
        assert!(commands(&start).iter().any(|l| l.trim_end() == set));
    }

    #[test]
    fn subtitles_go_in_with_the_video_shown_or_not() {
        let (tools, streams) = (Tools::default(), streams());
        let mut start = start(&tools, &streams, "A video");
        let vtt = "https://www.youtube.com/api/timedtext?v=x&lang=en,fr&fmt=vtt";
        start.subtitles = Some(vtt);
        let lines: Vec<serde_json::Value> = commands(&start)
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let sub = lines
            .iter()
            .find(|v| v["command"][1] == "sub-files")
            .expect("the subtitles");
        assert_eq!(
            sub["command"],
            serde_json::json!(["change-list", "sub-files", "append", vtt]),
            "one item: the comma doesn't split it"
        );
        let shown = lines
            .iter()
            .find(|v| v["command"][0] == "set" && v["command"][1] == "sub-visibility")
            .unwrap();
        assert_eq!(shown["command"][2], "no");
        assert_eq!(lines.last().unwrap()["command"][0], "loadfile");
        start.subtitles = None;
        let none = commands(&start).join("");
        assert!(!none.contains("sub-"), "{none}");
    }

    #[test]
    fn chapters_go_to_mpv_as_one_command_of_json_strings() {
        let chapters = [
            Chapter {
                start: 0.0,
                title: "Intro".into(),
            },
            Chapter {
                start: 83.5,
                title: "\"]}\n{\"command\":[\"run\",\"sh\"]} ${path}".into(),
            },
        ];
        let sent = line(chapters_command(&chapters));
        assert_eq!(sent.matches('\n').count(), 1, "one line: {sent}");
        let parsed: serde_json::Value = serde_json::from_str(sent.trim_end()).unwrap();
        let command = &parsed["command"];
        assert_eq!(command[0], "set_property");
        assert_eq!(command[1], "chapter-list");
        assert_eq!(command[2][1]["time"], 83.5);
        assert_eq!(
            command[2][1]["title"],
            chapters[1].title.as_str(),
            "kept whole"
        );
        assert_eq!(command.as_array().unwrap().len(), 3);
    }

    #[test]
    fn only_followed_properties_become_events() {
        let pos = r#"{"event":"property-change","id":1,"name":"time-pos","data":12.5}"#;
        assert_eq!(parse_event(pos), Some(PlayerEventKind::Position(12.5)));
        let pause = r#"{"event":"property-change","id":3,"name":"pause","data":true}"#;
        assert_eq!(parse_event(pause), Some(PlayerEventKind::Paused(true)));
        let volume = r#"{"event":"property-change","id":4,"name":"volume","data":45.0}"#;
        assert_eq!(parse_event(volume), Some(PlayerEventKind::Volume(45.0)));
        let mute = r#"{"event":"property-change","id":5,"name":"mute","data":false}"#;
        assert_eq!(parse_event(mute), Some(PlayerEventKind::Muted(false)));
        assert_eq!(
            parse_event(r#"{"event":"property-change","name":"time-pos","data":null}"#),
            None
        );
        assert_eq!(parse_event(r#"{"request_id":0,"error":"success"}"#), None);
        assert_eq!(parse_event("not json"), None);
    }

    #[test]
    fn the_systems_next_and_previous_come_back_from_mpv() {
        let tools = Tools::default();
        let streams = streams();
        let lines = commands(&start(&tools, &streams, "a title"));
        let sent: Vec<serde_json::Value> = lines
            .iter()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l.trim_end()).unwrap()["command"].clone()
            })
            .collect();
        let bound: Vec<_> = sent.iter().filter(|c| c[0] == "keybind").collect();
        assert_eq!(bound.len(), 2);
        assert_eq!(bound[0][1], "NEXT");
        assert_eq!(bound[0][2], "script-message tuitube next");
        assert_eq!(bound[1][1], "PREV");
        assert_eq!(bound[1][2], "script-message tuitube previous");
        let at = |name: &str| sent.iter().position(|c| c[0] == name).unwrap();
        assert!(at("keybind") < at("loadfile"), "bound before the video");

        let message =
            |args: &str| parse_event(&format!(r#"{{"event":"client-message","args":{args}}}"#));
        assert_eq!(
            message(r#"["tuitube","next"]"#),
            Some(PlayerEventKind::Remote(Remote::Next))
        );
        assert_eq!(
            message(r#"["tuitube","previous"]"#),
            Some(PlayerEventKind::Remote(Remote::Previous))
        );
        // Only tuitube's own two words.
        for other in [
            r#"["someone","next"]"#,
            r#"["tuitube","quit"]"#,
            r#"["tuitube","next","more"]"#,
            r#"["tuitube"]"#,
            r#"[1,2]"#,
            r#""tuitube next""#,
            "null",
        ] {
            assert_eq!(message(other), None, "{other}");
        }
        assert_eq!(parse_event(r#"{"event":"client-message"}"#), None);
        // Odd words in a message don't stop the rest being read.
        let pos = r#"{"event":"property-change","name":"time-pos","data":3.0,"args":{"x":1}}"#;
        assert_eq!(parse_event(pos), Some(PlayerEventKind::Position(3.0)));
    }

    #[test]
    fn the_channel_goes_in_as_the_files_artist_tag() {
        let url = "https://rr1.googlevideo.com/videoplayback?v=1&x=a,b;c%2C";
        assert_eq!(file_to_load(url, None), url, "no artist: the URL as it is");
        let edl = file_to_load(url, Some("Kênh; a,b %3%x"));
        // Each text after its length in bytes ("ê" is two).
        assert_eq!(
            edl,
            format!(
                "edl://!no_clip;!no_chapters;!global_tags,artist=%15%Kênh; a,b %3%x;%{}%{url};",
                url.len()
            )
        );
        // Shown on one line, whatever the channel's name holds.
        let edl = file_to_load(url, Some("two\nlines"));
        assert!(edl.contains("artist=%9%two lines;"), "{edl}");

        let tools = Tools::default();
        let streams = streams();
        let mut start = start(&tools, &streams, "a title");
        start.artist = Some("A channel");
        let lines = commands(&start);
        let load = lines.iter().find(|l| l.contains("loadfile")).unwrap();
        let load: serde_json::Value = serde_json::from_str(load.trim_end()).unwrap();
        assert_eq!(
            load["command"][1],
            file_to_load(&streams.video, Some("A channel"))
        );
    }

    #[test]
    fn the_cover_goes_to_mpv_as_album_art_not_shown() {
        let path = std::env::temp_dir().join("p-dQw4w9WgXcQ.jpg");
        let command = cover_command(&path).unwrap();
        assert_eq!(command["name"], "video-add");
        assert_eq!(command["url"], path.to_str().unwrap());
        assert_eq!(command["flags"], "auto", "not selected");
        assert_eq!(command["albumart"], true);
        let sent = line(command);
        assert_eq!(sent.matches('\n').count(), 1, "one line: {sent}");
        assert_eq!(
            cover_command(Path::new("p-dQw4w9WgXcQ.jpg")),
            None,
            "a relative path, which mpv could read as a URL"
        );
    }

    #[test]
    fn mpv_answers_about_the_visualizer_become_events() {
        let added = r#"{"request_id":1,"error":"success","data":null}"#;
        assert_eq!(parse_event(added), Some(PlayerEventKind::Visualizer(true)));
        let refused = r#"{"request_id":1,"error":"error running command"}"#;
        assert_eq!(
            parse_event(refused),
            Some(PlayerEventKind::Visualizer(false))
        );
        let numbers = r#"{"request_id":2,"error":"success","data":{"lavfi.astats.3.RMS_level":"-20.5","lavfi.astats.4.RMS_level":"-inf"}}"#;
        let Some(PlayerEventKind::Spectrum(levels)) = parse_event(numbers) else {
            panic!("no spectrum");
        };
        assert_eq!(levels[0], -20.5);
        assert!(levels[1] < -100.0, "silence");
        // Not there yet (the filter isn't in): nothing.
        let missing = r#"{"request_id":2,"error":"property unavailable"}"#;
        assert_eq!(parse_event(missing), None);
        assert_eq!(
            parse_event(r#"{"request_id":2,"error":"success","data":"odd"}"#),
            None
        );
        assert_eq!(
            parse_event(r#"{"event":"playback-restart"}"#),
            Some(PlayerEventKind::Restarted)
        );
        // Other commands' answers.
        assert_eq!(parse_event(r#"{"request_id":0,"error":"error"}"#), None);
    }

    #[test]
    fn the_visualizers_commands_are_json_mpv_answers_by_number() {
        let add = line_numbered(
            serde_json::json!(["af", "add", visualizer::filter()]),
            FILTER_ADDED,
        );
        let parsed: serde_json::Value = serde_json::from_str(add.trim_end()).unwrap();
        assert_eq!(parsed["request_id"], 1);
        assert_eq!(parsed["command"][2], visualizer::filter().as_str());
        assert_eq!(add.matches('\n').count(), 1, "one line");
    }

    #[test]
    fn sound_alone_keeps_minutes_in_memory_not_hours() {
        let (tools, streams) = (Tools::default(), streams());
        let mut start = start(&tools, &streams, "A video");
        let cache = |start: &Start| {
            args(start)
                .iter()
                .filter(|a| a.starts_with("--demuxer-max"))
                .cloned()
                .collect::<Vec<_>>()
        };
        assert!(cache(&start).is_empty(), "a video keeps mpv's own");
        start.audio_only = true;
        assert_eq!(
            cache(&start),
            ["--demuxer-max-bytes=16MiB", "--demuxer-max-back-bytes=8MiB"]
        );
        assert!(args(&start).contains(&"--no-video".to_string()));
    }

    #[test]
    fn only_playing_to_the_end_counts_as_ended() {
        let end = |reason: &str| {
            parse_event(&format!(
                r#"{{"event":"end-file","reason":"{reason}","playlist_entry_id":1}}"#
            ))
        };
        assert_eq!(end("eof"), Some(PlayerEventKind::Ended));
        assert_eq!(end("error"), Some(PlayerEventKind::Failed));
        for reason in ["quit", "stop", "redirect", "unknown"] {
            assert_eq!(end(reason), None, "{reason}");
        }
        assert_eq!(parse_event(r#"{"event":"end-file"}"#), None);
    }
}

/// With the real mpv: `cargo test -- --ignored live`.
#[cfg(all(test, unix))]
mod live {
    use super::*;
    use crate::app::AppEvent;

    #[tokio::test]
    #[ignore = "runs mpv"]
    async fn live_mpv_plays_what_the_socket_sends_and_stops() {
        let tools = Tools::find(None, None, None);
        let mpv = tools.mpv.clone().expect("mpv is installed");
        // Silence made by mpv itself, with a second silent sound added the
        // way a stream's separate sound is: no network, no sound.
        let streams = Streams {
            video: "av://lavfi:anullsrc=d=5".into(),
            audio: Some("av://lavfi:anullsrc=d=5".into()),
            user_agent: Some("tuitube-test".into()),
            duration: None,
            chapters: Vec::new(),
            quality: None,
            info: None,
            captions: Vec::new(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let start = Start {
            mpv: &mpv,
            tools: &tools,
            streams: &streams,
            title: "silence, with \"quotes\" and ${path}",
            start_at: Some(1.0),
            audio_only: true,
            subtitles: None,
            subtitles_shown: false,
            volume: 40,
            muted: true,
            paused: false,
            looped: false,
            artist: None,
            // Not in Now Playing: a test doesn't take over the notch.
            now_playing: false,
            pepper: None,
        };
        let mut player = Player::start(7, start, tx).unwrap();
        let mut positions = Vec::new();
        let mut volumes = Vec::new();
        let mut muted = Vec::new();
        let mut exited = false;
        let mut ended = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let AppEvent::Player(event) = event else {
                continue;
            };
            assert_eq!(event.play, 7);
            match event.kind {
                PlayerEventKind::Position(at) => {
                    positions.push(at);
                    if positions.len() == 1 {
                        player.set_volume(65);
                        player.set_muted(false);
                    }
                    if positions.len() == 3 {
                        player.stop();
                    }
                }
                PlayerEventKind::Volume(volume) => volumes.push(volume),
                PlayerEventKind::Muted(m) => muted.push(m),
                PlayerEventKind::Ended => ended = true,
                PlayerEventKind::Exited => {
                    exited = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(
            !positions.is_empty(),
            "nothing played: the socket's file didn't load"
        );
        assert_eq!(volumes, [40.0, 65.0], "never mpv's own 100");
        assert_eq!(muted, [true, false]);
        assert!(positions[0] >= 0.9, "started at --start: {positions:?}");
        assert!(exited, "mpv didn't quit");
        assert!(!ended, "stopped isn't played to the end");
    }

    #[tokio::test]
    #[ignore = "runs mpv"]
    async fn live_mpv_seeks_to_where_a_skipped_segment_ends() {
        let tools = Tools::find(None, None, None);
        let mpv = tools.mpv.clone().expect("mpv is installed");
        let streams = Streams {
            video: "av://lavfi:anullsrc=d=10".into(),
            audio: None,
            user_agent: None,
            duration: None,
            chapters: Vec::new(),
            quality: None,
            info: None,
            captions: Vec::new(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let start = Start {
            mpv: &mpv,
            tools: &tools,
            streams: &streams,
            title: "ten seconds of silence",
            start_at: None,
            audio_only: true,
            subtitles: None,
            subtitles_shown: false,
            volume: 100,
            muted: false,
            paused: false,
            looped: false,
            artist: None,
            now_playing: false,
            pepper: None,
        };
        let mut player = Player::start(9, start, tx).unwrap();
        let mut sought = None;
        let mut landed = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let AppEvent::Player(event) = event else {
                continue;
            };
            match event.kind {
                PlayerEventKind::Position(at) if sought.is_none() && at < 1.0 => {
                    player.seek_to(6.5);
                    sought = Some(std::time::Instant::now());
                }
                PlayerEventKind::Position(at) if at >= 6.5 && landed.is_none() => {
                    landed = sought.map(|t| (at, t.elapsed()));
                    player.stop();
                }
                PlayerEventKind::Exited => break,
                _ => {}
            }
        }
        let (at, after) = landed.expect("the seek didn't happen");
        // Played there in real time, it would take 5.5 s or more.
        assert!(after < Duration::from_secs(3), "{after:?}");
        assert!(at < 8.0, "landed at {at}");
    }

    #[tokio::test]
    #[ignore = "runs mpv"]
    async fn live_mpv_measures_the_sound_for_the_visualizer() {
        let tools = Tools::find(None, None, None);
        let mpv = tools.mpv.clone().expect("mpv is installed");
        // A low hum made by mpv itself, muted: mpv mutes after the filter,
        // which measures it all the same.
        let streams = Streams {
            video: "av://lavfi:sine=f=100:d=8".into(),
            audio: None,
            user_agent: None,
            duration: None,
            chapters: Vec::new(),
            quality: None,
            info: None,
            captions: Vec::new(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let start = Start {
            mpv: &mpv,
            tools: &tools,
            streams: &streams,
            title: "a hum",
            start_at: None,
            audio_only: true,
            subtitles: None,
            subtitles_shown: false,
            volume: 100,
            muted: true,
            paused: false,
            looped: false,
            artist: None,
            now_playing: false,
            pepper: None,
        };
        let mut player = Player::start(6, start, tx).unwrap();
        let mut added = None;
        let mut spectra: Vec<Levels> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let AppEvent::Player(event) = event else {
                continue;
            };
            match event.kind {
                PlayerEventKind::Restarted if added.is_none() => {
                    added = Some(false);
                    player.add_visualizer();
                }
                PlayerEventKind::Visualizer(ok) => {
                    added = Some(ok);
                    assert!(ok, "the filter didn't go in");
                    player.follow_spectrum(true);
                }
                PlayerEventKind::Spectrum(levels) => {
                    spectra.push(levels);
                    if spectra.len() == 10 {
                        player.remove_visualizer();
                        player.stop();
                    }
                }
                PlayerEventKind::Exited => break,
                _ => {}
            }
        }
        assert_eq!(added, Some(true));
        assert!(spectra.len() >= 10, "{} answers", spectra.len());
        let last = spectra.last().unwrap();
        let loudest = (0..visualizer::BANDS)
            .max_by(|&a, &b| last[a].total_cmp(&last[b]))
            .unwrap();
        // 100 Hz: between the 85 Hz and 115 Hz bands.
        assert!(matches!(loudest, 3 | 4), "{loudest}: {last:?}");
        assert!(last[loudest] > -60.0, "{last:?}");
        assert!(last[19] < last[loudest] - 40.0, "the treble: {last:?}");
    }

    #[tokio::test]
    #[ignore = "runs mpv"]
    async fn live_mpv_plays_the_edl_with_the_artist_as_the_bare_file() {
        let tools = Tools::find(None, None, None);
        let mpv = tools.mpv.clone().expect("mpv is installed");
        // mpv opens no `av://` silence inside an EDL (a special protocol is
        // unsafe there), so it writes four seconds of it to a file first.
        let dir = std::env::temp_dir().join(format!("tuitube-edl-{}", std::process::id()));
        crate::config::private_dir(&dir).unwrap();
        let file = dir.join("silence.mka");
        let made = std::process::Command::new(&mpv)
            .args(["--no-config", "--terminal=no", "--oac=flac"])
            .arg(format!("--o={}", file.display()))
            .arg("av://lavfi:anullsrc=d=4")
            .status()
            .unwrap();
        assert!(made.success(), "mpv wrote the silence");
        let streams = Streams {
            video: file.to_str().unwrap().into(),
            audio: None,
            user_agent: None,
            duration: None,
            chapters: Vec::new(),
            quality: None,
            info: None,
            captions: Vec::new(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let start = Start {
            mpv: &mpv,
            tools: &tools,
            streams: &streams,
            title: "four seconds of silence",
            start_at: Some(1.0),
            audio_only: true,
            subtitles: None,
            subtitles_shown: false,
            volume: 0,
            muted: true,
            paused: false,
            looped: false,
            artist: Some("a channel; with, %3% Kênh"),
            now_playing: false,
            pepper: None,
        };
        let _player = Player::start(9, start, tx).unwrap();
        let (mut positions, mut kinds) = (Vec::new(), Vec::new());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if let AppEvent::Player(event) = event {
                match event.kind {
                    PlayerEventKind::Position(at) => positions.push(at),
                    PlayerEventKind::Ended | PlayerEventKind::Exited => kinds.push(event.kind),
                    _ => {}
                }
                if kinds.last() == Some(&PlayerEventKind::Exited) {
                    break;
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            positions.first().is_some_and(|&at| at >= 0.9),
            "started at --start: {positions:?}"
        );
        assert_eq!(
            kinds,
            [PlayerEventKind::Ended, PlayerEventKind::Exited],
            "played to its end, then quit by itself"
        );
    }

    #[tokio::test]
    #[ignore = "runs mpv"]
    async fn live_mpv_says_when_a_video_plays_to_its_end() {
        let tools = Tools::find(None, None, None);
        let mpv = tools.mpv.clone().expect("mpv is installed");
        let streams = Streams {
            video: "av://lavfi:anullsrc=d=2".into(),
            audio: None,
            user_agent: None,
            duration: None,
            chapters: Vec::new(),
            quality: None,
            info: None,
            captions: Vec::new(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let start = Start {
            mpv: &mpv,
            tools: &tools,
            streams: &streams,
            title: "two seconds of silence",
            start_at: None,
            audio_only: true,
            subtitles: None,
            subtitles_shown: false,
            volume: 100,
            muted: false,
            paused: false,
            looped: false,
            artist: None,
            now_playing: false,
            pepper: None,
        };
        let _player = Player::start(8, start, tx).unwrap();
        let mut kinds = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if let AppEvent::Player(event) = event {
                match event.kind {
                    PlayerEventKind::Ended | PlayerEventKind::Exited => kinds.push(event.kind),
                    _ => {}
                }
                if kinds.last() == Some(&PlayerEventKind::Exited) {
                    break;
                }
            }
        }
        assert_eq!(
            kinds,
            [PlayerEventKind::Ended, PlayerEventKind::Exited],
            "ended, then quit by itself"
        );
    }
}
