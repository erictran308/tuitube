//! The one tmux setting tuitube changes, put back on the way out.
//!
//! Asking the terminal which image protocol it speaks (ratatui-image's
//! `Picker`) runs `tmux set -p allow-passthrough on` whenever TERM or
//! TERM_PROGRAM says tmux, whatever the `images` setting. Left on, anything
//! printed in that pane later could send escape codes past tmux to the outer
//! terminal. So the pane's own value is read before the picker is built and
//! put back when tuitube ends: normally, after `--check`, after `--demo`, and
//! from the panic hook. tmux is found as an absolute path, like every
//! program tuitube starts, and gets a cleaned environment.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::tools;

struct Saved {
    tmux: PathBuf,
    socket: String,
    pane: String,
    value: Option<String>,
}

static SAVED: OnceLock<Saved> = OnceLock::new();

/// The server's socket and the pane tmux names for this process: only these
/// can be put back. Without them (a tmux TERM forwarded over ssh) there's no
/// telling which pane the image library changed.
fn target(env: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let set = |name: &str| env(name).filter(|v| !v.is_empty());
    let in_tmux = set("TERM").is_some_and(|t| t.starts_with("tmux"))
        || set("TERM_PROGRAM").is_some_and(|p| p == "tmux");
    let socket = set("TMUX")?.split(',').next()?.to_string();
    let pane = set("TMUX_PANE")?;
    (in_tmux && socket.starts_with('/') && pane.starts_with('%')).then_some((socket, pane))
}

fn tmux(path: &PathBuf, socket: &str) -> Command {
    let mut command = Command::new(path);
    command
        .env_clear()
        .envs(tools::child_env(&tools::Tools::default()))
        .arg("-S")
        .arg(socket)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Remembers the pane's allow-passthrough. Call before any image `Picker`
/// is built.
pub fn save() {
    let Some((socket, pane)) = target(|name| std::env::var(name).ok()) else {
        return;
    };
    let Some(path) = tools::on_path("tmux") else {
        return;
    };
    let Ok(out) = tmux(&path, &socket)
        .args(["show-options", "-pqv", "-t", &pane, "allow-passthrough"])
        .output()
    else {
        return;
    };
    if out.status.success() {
        let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let value = (!value.is_empty()).then_some(value);
        let _ = SAVED.set(Saved {
            tmux: path,
            socket,
            pane,
            value,
        });
    }
}

fn restore_args(pane: &str, value: Option<&str>) -> Vec<String> {
    let mut args = vec!["set-option".to_string(), "-p".into()];
    if value.is_none() {
        args.push("-u".into());
    }
    args.extend(["-t".into(), pane.into(), "allow-passthrough".into()]);
    args.extend(value.map(String::from));
    args
}

/// Puts the pane's own value back, or unsets it. Harmless to call twice.
pub fn restore() {
    if let Some(saved) = SAVED.get() {
        let _ = tmux(&saved.tmux, &saved.socket)
            .args(restore_args(&saved.pane, saved.value.as_deref()))
            .stdout(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn only_the_pane_tmux_names_is_touched() {
        let pane = [
            ("TERM_PROGRAM", "tmux"),
            ("TMUX", "/tmp/tmux-1000/default,1234,0"),
            ("TMUX_PANE", "%3"),
        ];
        assert_eq!(
            target(env(&pane)),
            Some(("/tmp/tmux-1000/default".into(), "%3".into()))
        );
        // A tmux TERM forwarded over ssh: no pane to put back.
        assert_eq!(target(env(&[("TERM", "tmux-256color")])), None);
        assert_eq!(target(env(&[("TERM", "xterm-256color")])), None);
        let odd = [
            ("TERM", "tmux-256color"),
            ("TMUX", "relative,1,0"),
            ("TMUX_PANE", "%3"),
        ];
        assert_eq!(target(env(&odd)), None);
    }

    #[test]
    fn the_old_value_is_put_back_or_unset() {
        assert_eq!(
            restore_args("%3", Some("off")),
            ["set-option", "-p", "-t", "%3", "allow-passthrough", "off"]
        );
        assert_eq!(
            restore_args("%3", None),
            ["set-option", "-p", "-u", "-t", "%3", "allow-passthrough"]
        );
    }
}
