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

use crate::tools::{self, Tools};
use crate::ytdlp::Streams;

/// How long mpv has to quit when asked, before it's killed.
const QUIT_GRACE: Duration = Duration::from_secs(3);

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
    /// The video played to its end: mpv's `end-file` with reason `eof`, not
    /// `quit` (its window closed, or tuitube stopped it) or `error`.
    Ended,
    /// mpv exited, after the video ended or its window was closed.
    Exited,
}

pub struct Player {
    pub play: u64,
    #[cfg(unix)]
    control: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    /// Tells the task that owns mpv's process to end it.
    stop: Option<oneshot::Sender<()>>,
}

pub struct Start<'a> {
    pub mpv: &'a Path,
    pub tools: &'a Tools,
    pub streams: &'a Streams,
    pub title: &'a str,
    pub start_at: Option<f64>,
    pub audio_only: bool,
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
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // If tuitube ends without stopping it, mpv goes too.
            .kill_on_drop(true);
        if let Some(home) = dirs::home_dir() {
            command.current_dir(home);
        }

        #[cfg(unix)]
        let (child, control) = {
            use std::os::fd::AsRawFd;
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
            let child = command.spawn().context("cannot start mpv")?;
            drop(theirs);
            ours.set_nonblocking(true)?;
            let stream = tokio::net::UnixStream::from_std(ours)?;
            let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
            spawn_ipc(play, stream, control_rx, tx.clone());
            for line in commands(&start) {
                let _ = control_tx.send(line);
            }
            (child, control_tx)
        };
        #[cfg(not(unix))]
        let child = command.spawn().context("cannot start mpv")?;

        let (stop_tx, stop_rx) = oneshot::channel();
        tokio::spawn(own(play, child, stop_rx, tx));
        Ok(Self {
            play,
            #[cfg(unix)]
            control: Some(control),
            stop: Some(stop_tx),
        })
    }

    /// A player with no mpv behind it, for the demo's player bar.
    pub fn detached(play: u64) -> Self {
        Self {
            play,
            #[cfg(unix)]
            control: None,
            stop: None,
        }
    }

    /// Whether tuitube can pause, seek and follow the playback (not on
    /// Windows yet).
    pub fn controllable(&self) -> bool {
        cfg!(unix)
    }

    fn send(&self, command: serde_json::Value) {
        #[cfg(unix)]
        if let Some(control) = &self.control {
            let _ = control.send(line(command));
        }
        #[cfg(not(unix))]
        let _ = command;
    }

    pub fn toggle_pause(&self) {
        self.send(serde_json::json!(["cycle", "pause"]));
    }

    pub fn seek(&self, seconds: i32) {
        self.send(serde_json::json!(["seek", seconds, "relative"]));
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
    if start.audio_only {
        args.push("--no-video".into());
        args.push("--force-window=no".into());
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
    args.push("--".into());
    args.push(start.streams.video.clone());
    args
}

/// The options that describe this playback: its title (in mpv's window and
/// media controls), the user agent the streams expect, where to start.
fn settings(start: &Start) -> Vec<(&'static str, String)> {
    let title = crate::video::one_line(start.title, 150);
    let mut settings = vec![
        ("force-media-title", title.clone()),
        // The window title expands `${…}` properties; `$$` is a plain `$`.
        ("title", format!("{} — tuitube", title.replace('$', "$$"))),
    ];
    if let Some(ua) = &start.streams.user_agent {
        settings.push(("user-agent", crate::video::one_line(ua, 300)));
    }
    if let Some(at) = start.start_at.filter(|s| s.is_finite() && *s > 0.0) {
        settings.push(("start", format!("{at:.0}")));
    }
    settings
}

/// One command for mpv's control socket, as a line of JSON: strings are
/// JSON strings, so a title can't break out of its place.
#[cfg_attr(not(unix), allow(dead_code))]
fn line(command: serde_json::Value) -> String {
    format!("{}\n", serde_json::json!({ "command": command }))
}

/// What's sent over the socket to play the video: the properties tuitube
/// follows, the settings (with `set`, which takes the value as is), the
/// sound's URL added to the list of audio files as one item, then the
/// picture's URL.
#[cfg_attr(not(unix), allow(dead_code))]
fn commands(start: &Start) -> Vec<String> {
    let mut commands: Vec<String> = [(1, "time-pos"), (2, "duration"), (3, "pause")]
        .into_iter()
        .map(|(id, property)| line(serde_json::json!(["observe_property", id, property])))
        .collect();
    for (name, value) in settings(start) {
        commands.push(line(serde_json::json!(["set", name, value])));
    }
    if let Some(audio) = &start.streams.audio {
        commands.push(line(serde_json::json!([
            "change-list",
            "audio-files",
            "append",
            audio
        ])));
    }
    commands.push(line(serde_json::json!([
        "loadfile",
        start.streams.video,
        "replace"
    ])));
    commands
}

#[cfg(unix)]
fn spawn_ipc(
    play: u64,
    stream: tokio::net::UnixStream,
    mut control: tokio::sync::mpsc::UnboundedReceiver<String>,
    tx: UnboundedSender<crate::app::AppEvent>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (read, mut write) = stream.into_split();
    tokio::spawn(async move {
        while let Some(line) = control.recv().await {
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
}

/// One of mpv's messages, if it's one tuitube follows.
#[cfg(any(unix, test))]
fn parse_event(line: &str) -> Option<PlayerEventKind> {
    let message: IpcMessage = serde_json::from_str(line).ok()?;
    if message.event.as_deref() == Some("end-file") {
        return (message.reason.as_deref() == Some("eof")).then_some(PlayerEventKind::Ended);
    }
    if message.event.as_deref() != Some("property-change") {
        return None;
    }
    let data = message.data?;
    match message.name.as_deref()? {
        "time-pos" => data.as_f64().map(PlayerEventKind::Position),
        "duration" => data.as_f64().map(PlayerEventKind::Duration),
        "pause" => data.as_bool().map(PlayerEventKind::Paused),
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
        }
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
    fn only_followed_properties_become_events() {
        let pos = r#"{"event":"property-change","id":1,"name":"time-pos","data":12.5}"#;
        assert_eq!(parse_event(pos), Some(PlayerEventKind::Position(12.5)));
        let pause = r#"{"event":"property-change","id":3,"name":"pause","data":true}"#;
        assert_eq!(parse_event(pause), Some(PlayerEventKind::Paused(true)));
        assert_eq!(
            parse_event(r#"{"event":"property-change","name":"time-pos","data":null}"#),
            None
        );
        assert_eq!(parse_event(r#"{"request_id":0,"error":"success"}"#), None);
        assert_eq!(parse_event("not json"), None);
    }

    #[test]
    fn only_playing_to_the_end_counts_as_ended() {
        let end = |reason: &str| {
            parse_event(&format!(
                r#"{{"event":"end-file","reason":"{reason}","playlist_entry_id":1}}"#
            ))
        };
        assert_eq!(end("eof"), Some(PlayerEventKind::Ended));
        for reason in ["quit", "stop", "error", "redirect", "unknown"] {
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
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let start = Start {
            mpv: &mpv,
            tools: &tools,
            streams: &streams,
            title: "silence, with \"quotes\" and ${path}",
            start_at: Some(1.0),
            audio_only: true,
        };
        let mut player = Player::start(7, start, tx).unwrap();
        let mut positions = Vec::new();
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
                    if positions.len() == 3 {
                        player.stop();
                    }
                }
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
        assert!(positions[0] >= 0.9, "started at --start: {positions:?}");
        assert!(exited, "mpv didn't quit");
        assert!(!ended, "stopped isn't played to the end");
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
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let start = Start {
            mpv: &mpv,
            tools: &tools,
            streams: &streams,
            title: "two seconds of silence",
            start_at: None,
            audio_only: true,
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
