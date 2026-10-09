//! Playing a video: mpv, run as a separate program, given the stream URLs
//! tuitube resolved. Video opens in mpv's own window; sound only plays with
//! no window, controlled from tuitube's player bar.
//!
//! mpv starts with `--no-config` (none of the user's mpv.conf, input.conf
//! or scripts), `--terminal=no` (it never touches tuitube's terminal, and
//! never prints the signed stream URLs), `--ytdl=no` (it doesn't run a
//! second yt-dlp of its own), and the URLs after `--`. On Unix it's
//! controlled over one end of a socket pair handed to it as file descriptor
//! 3 (`--input-ipc-client=fd://3`): there's no socket file another program
//! could connect to, and mpv quits when tuitube's end closes. mpv's control
//! protocol can run programs, so it must never be reachable by anyone else.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::tools::{self, Tools};
use crate::ytdlp::Streams;

/// Something mpv said, for the playback numbered `play`.
#[derive(Debug)]
pub struct PlayerEvent {
    pub play: u64,
    pub kind: PlayerEventKind,
}

#[derive(Debug, PartialEq)]
pub enum PlayerEventKind {
    Position(f64),
    Duration(f64),
    Paused(bool),
    /// mpv exited, after the video ended or its window was closed.
    Exited,
}

pub struct Player {
    pub play: u64,
    #[cfg(unix)]
    control: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    child_id: Option<u32>,
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
            .kill_on_drop(false);
        if let Some(home) = dirs::home_dir() {
            command.current_dir(home);
        }

        #[cfg(unix)]
        {
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
            let mut child = command.spawn().context("cannot start mpv")?;
            drop(theirs);
            let child_id = child.id();
            ours.set_nonblocking(true)?;
            let stream = tokio::net::UnixStream::from_std(ours)?;
            let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
            spawn_ipc(play, stream, control_rx, tx.clone());
            tokio::spawn(async move {
                let _ = child.wait().await;
                let _ = tx.send(crate::app::AppEvent::Player(PlayerEvent {
                    play,
                    kind: PlayerEventKind::Exited,
                }));
            });
            let player = Self {
                play,
                control: Some(control_tx),
                child_id,
            };
            for (id, property) in [(1, "time-pos"), (2, "duration"), (3, "pause")] {
                player.send(&format!(
                    r#"{{"command":["observe_property",{id},"{property}"]}}"#
                ));
            }
            Ok(player)
        }
        #[cfg(not(unix))]
        {
            let mut child = command.spawn().context("cannot start mpv")?;
            let child_id = child.id();
            tokio::spawn(async move {
                let _ = child.wait().await;
                let _ = tx.send(crate::app::AppEvent::Player(PlayerEvent {
                    play,
                    kind: PlayerEventKind::Exited,
                }));
            });
            Ok(Self { play, child_id })
        }
    }

    /// Whether tuitube can pause, seek and follow the playback (not on
    /// Windows yet).
    pub fn controllable(&self) -> bool {
        cfg!(unix)
    }

    fn send(&self, line: &str) {
        #[cfg(unix)]
        if let Some(control) = &self.control {
            let _ = control.send(format!("{line}\n"));
        }
        #[cfg(not(unix))]
        let _ = line;
    }

    pub fn toggle_pause(&self) {
        self.send(r#"{"command":["cycle","pause"]}"#);
    }

    pub fn seek(&self, seconds: i32) {
        self.send(&format!(r#"{{"command":["seek",{seconds},"relative"]}}"#));
    }

    /// Stops playback: mpv quits.
    pub fn stop(&mut self) {
        self.send(r#"{"command":["quit"]}"#);
        #[cfg(unix)]
        {
            // Closing our end makes mpv quit even if it missed the command.
            self.control = None;
        }
        #[cfg(not(unix))]
        if let Some(id) = self.child_id.take() {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &id.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.child_id;
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

/// mpv's arguments: options, then `--` and the picture's URL. The sound's
/// URL goes in `--audio-file`, which takes it as a value, not an option.
fn args(start: &Start) -> Vec<String> {
    let mut args: Vec<String> = [
        "--no-config",
        "--load-scripts=no",
        "--terminal=no",
        "--ytdl=no",
        "--idle=no",
        "--keep-open=no",
        "--save-position-on-quit=no",
        "--resume-playback=no",
        "--hwdec=auto-safe",
    ]
    .map(String::from)
    .into();
    #[cfg(unix)]
    args.push("--input-ipc-client=fd://3".into());
    if start.audio_only {
        args.push("--no-video".into());
        args.push("--force-window=no".into());
    } else {
        args.push("--force-window=immediate".into());
    }
    let title = crate::video::one_line(start.title, 150);
    args.push(format!("--force-media-title={title}"));
    // The window title expands `${…}` properties; `$$` is a plain `$`.
    args.push(format!("--title={} — tuitube", title.replace('$', "$$")));
    if let Some(ua) = &start.streams.user_agent {
        args.push(format!("--user-agent={ua}"));
    }
    if let Some(at) = start.start_at.filter(|s| s.is_finite() && *s > 0.0) {
        args.push(format!("--start={at:.0}"));
    }
    if let Some(audio) = &start.streams.audio {
        args.push(format!("--audio-file={audio}"));
    }
    args.push("--".into());
    args.push(start.streams.video.clone());
    args
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

#[derive(Deserialize)]
struct IpcMessage {
    event: Option<String>,
    name: Option<String>,
    data: Option<serde_json::Value>,
}

/// One of mpv's messages, if it's one tuitube follows.
fn parse_event(line: &str) -> Option<PlayerEventKind> {
    let message: IpcMessage = serde_json::from_str(line).ok()?;
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
            audio: Some("https://rr1.googlevideo.com/videoplayback?a=1".into()),
            user_agent: Some("Mozilla/5.0".into()),
            duration: Some(10.0),
        }
    }

    #[test]
    fn mpv_ignores_the_users_config_and_gets_the_url_after_a_double_dash() {
        let tools = Tools::default();
        let streams = streams();
        let start = Start {
            mpv: Path::new("/usr/bin/mpv"),
            tools: &tools,
            streams: &streams,
            title: "--script=/tmp/x.lua\nevil",
            start_at: Some(42.4),
            audio_only: false,
        };
        let args = args(&start);
        assert_eq!(args[0], "--no-config");
        assert!(args.contains(&"--terminal=no".to_string()));
        assert!(args.contains(&"--ytdl=no".to_string()));
        assert!(args.contains(&"--start=42".to_string()));
        assert!(args.contains(&"--force-media-title=--script=/tmp/x.lua evil".to_string()));
        let dashes = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(dashes, args.len() - 2);
        assert_eq!(args.last().unwrap(), &streams.video);
        assert!(!args.iter().any(|a| a.starts_with("--script=")));
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
}

/// With the real mpv: `cargo test -- --ignored live`.
#[cfg(all(test, unix))]
mod live {
    use super::*;
    use crate::app::AppEvent;

    #[tokio::test]
    #[ignore = "runs mpv"]
    async fn live_mpv_reports_its_position_over_the_private_channel_and_stops() {
        let tools = Tools::find(None, None, None);
        let mpv = tools.mpv.clone().expect("mpv is installed");
        // Three seconds of silence, made by mpv itself: no network, no sound.
        let streams = Streams {
            video: "av://lavfi:anullsrc=d=3".into(),
            audio: None,
            user_agent: None,
            duration: None,
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let start = Start {
            mpv: &mpv,
            tools: &tools,
            streams: &streams,
            title: "silence",
            start_at: None,
            audio_only: true,
        };
        let mut player = Player::start(7, start, tx).unwrap();
        let mut positions = 0;
        let mut exited = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let AppEvent::Player(event) = event else {
                continue;
            };
            assert_eq!(event.play, 7);
            match event.kind {
                PlayerEventKind::Position(_) => {
                    positions += 1;
                    if positions == 3 {
                        player.stop();
                    }
                }
                PlayerEventKind::Exited => {
                    exited = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(positions >= 1, "no position over fd 3");
        assert!(exited, "mpv didn't quit");
    }
}
