//! The programs tuitube runs, yt-dlp, mpv and Deno, found once at start as
//! absolute paths, and the environment they get.
//!
//! A program is never named bare: `Command::new("yt-dlp")` would search the
//! working directory on some systems, and an empty or relative `PATH` entry
//! everywhere, so a `yt-dlp` planted in a downloaded folder would run.
//! Children also get a cleaned environment, so nothing set for tuitube's
//! shell (`PYTHONPATH`, `LD_PRELOAD`, `YTDLP_*`…) changes what they do.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::config;

#[derive(Clone, Debug, Default)]
pub struct Tools {
    pub yt_dlp: Option<PathBuf>,
    pub mpv: Option<PathBuf>,
    pub deno: Option<PathBuf>,
}

/// Folders looked in after `PATH`, where package managers put programs
/// that a GUI-started shell may not have on its `PATH`.
#[cfg(unix)]
const EXTRA_DIRS: [&str; 4] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/run/current-system/sw/bin",
];
#[cfg(not(unix))]
const EXTRA_DIRS: [&str; 0] = [];

impl Tools {
    /// Each program from the settings (or `TT_YT_DLP`, `TT_MPV`, `TT_DENO`)
    /// if given, else from `PATH`.
    pub fn find(yt_dlp: Option<&Path>, mpv: Option<&Path>, deno: Option<&Path>) -> Self {
        let pick = |set: Option<&Path>, var: &str, name: &str| {
            let set = config::var(var)
                .map(PathBuf::from)
                .or(set.map(Path::to_path_buf));
            match set {
                Some(path) => usable(&path).then_some(path),
                None => on_path(name),
            }
        };
        Self {
            yt_dlp: pick(yt_dlp, "TT_YT_DLP", "yt-dlp"),
            mpv: pick(mpv, "TT_MPV", "mpv"),
            deno: pick(deno, "TT_DENO", "deno"),
        }
    }

    /// What's missing, as an install hint for the status bar.
    pub fn missing(&self) -> Option<String> {
        let missing: Vec<&str> = [
            ("yt-dlp", self.yt_dlp.is_none()),
            ("deno", self.deno.is_none()),
            ("mpv", self.mpv.is_none()),
        ]
        .into_iter()
        .filter_map(|(name, gone)| gone.then_some(name))
        .collect();
        if missing.is_empty() {
            return None;
        }
        let list = missing.join(" ");
        let how = if cfg!(target_os = "macos") {
            format!("brew install {list}")
        } else if cfg!(windows) {
            let ids: Vec<&str> = missing
                .iter()
                .map(|name| match *name {
                    "yt-dlp" => "yt-dlp.yt-dlp",
                    "deno" => "DenoLand.Deno",
                    _ => "shinchiro.mpv",
                })
                .collect();
            format!("winget install {}", ids.join(" "))
        } else {
            format!("install {list} with your package manager")
        };
        Some(format!("Not found: {list}. {how}"))
    }
}

/// An absolute path to an existing file that this user can run.
fn usable(path: &Path) -> bool {
    if !path.is_absolute() || config::on_another_machine(path) {
        return false;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    meta.is_file()
}

/// `name` in the absolute folders on `PATH`, then in [`EXTRA_DIRS`].
pub fn on_path(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(EXTRA_DIRS.iter().map(PathBuf::from))
        .filter(|dir| dir.is_absolute() && !config::on_another_machine(dir))
        .map(|dir| dir.join(&exe))
        .find(|candidate| usable(candidate))
}

/// Variables passed on to children: what a program needs to find its home,
/// its temporary folder, its language and (for mpv) the screen and sound;
/// where the system's certificates are, for checking TLS; and the proxy, so
/// yt-dlp and mpv take the same way as tuitube's own requests (reqwest uses
/// these too): a proxy that hid only the feeds would show your address with
/// every video you play.
const KEPT: [&str; 32] = [
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TZ",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "XAUTHORITY",
    "DBUS_SESSION_BUS_ADDRESS",
    "PULSE_SERVER",
    "SYSTEMROOT",
    "WINDIR",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "LOCALAPPDATA",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NIX_SSL_CERT_FILE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

/// The environment a child starts with: [`KEPT`], a `PATH` of the system's
/// folders and the tools' own, and settings that keep Python from loading
/// anything from the user's own site folder or writing bytecode.
pub fn child_env(tools: &Tools) -> Vec<(OsString, OsString)> {
    child_env_from(tools, |key| std::env::var_os(key))
}

fn child_env_from(
    tools: &Tools,
    var: impl Fn(&str) -> Option<OsString>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = KEPT
        .iter()
        .filter_map(|&key| Some((key.into(), var(key)?)))
        .collect();
    let mut dirs: Vec<PathBuf> = [&tools.yt_dlp, &tools.mpv, &tools.deno]
        .into_iter()
        .flatten()
        .filter_map(|tool| Some(tool.parent()?.to_path_buf()))
        .collect();
    #[cfg(unix)]
    dirs.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
    #[cfg(windows)]
    if let Some(root) = var("SYSTEMROOT") {
        dirs.push(PathBuf::from(&root).join("System32"));
        dirs.push(PathBuf::from(root));
    }
    dirs.dedup();
    if !dirs.is_empty()
        && let Ok(path) = std::env::join_paths(dirs)
    {
        env.push(("PATH".into(), path));
    }
    for (key, value) in [
        ("PYTHONNOUSERSITE", "1"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("PYTHONUTF8", "1"),
        ("YTDLP_NO_PLUGINS", "1"),
        ("DENO_NO_UPDATE_CHECK", "1"),
        // On Windows, programs a child starts by name aren't looked for in
        // its working directory first.
        ("NoDefaultCurrentDirectoryInExePath", "1"),
    ] {
        env.push((key.into(), value.into()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_remote_paths_are_never_used() {
        assert!(!usable(Path::new("yt-dlp")));
        assert!(!usable(Path::new("./yt-dlp")));
        assert!(!usable(Path::new("//server/share/yt-dlp")));
    }

    #[test]
    fn children_get_no_python_path_or_preloads_but_keep_the_proxy() {
        let set = |key: &str| -> Option<OsString> {
            match key {
                "PYTHONPATH" | "LD_PRELOAD" | "DYLD_INSERT_LIBRARIES" | "YTDLP_CONFIG" => {
                    Some("/nonexistent/evil".into())
                }
                "HTTPS_PROXY" | "HOME" | "SSL_CERT_FILE" => Some("kept".into()),
                "PATH" => Some(".:relative:/usr/bin".into()),
                _ => None,
            }
        };
        let env = child_env_from(&Tools::default(), set);
        let keys: Vec<String> = env
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for dropped in [
            "PYTHONPATH",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "YTDLP_CONFIG",
        ] {
            assert!(!keys.contains(&dropped.to_string()), "{dropped}");
        }
        for kept in ["HTTPS_PROXY", "HOME", "SSL_CERT_FILE", "PYTHONNOUSERSITE"] {
            assert!(keys.contains(&kept.to_string()), "{kept}");
        }
        // No PATH at all is fine (Windows, with no system folder found
        // here); one that's there holds only full paths.
        let path = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone());
        for dir in path.iter().flat_map(std::env::split_paths) {
            assert!(dir.is_absolute(), "the caller's PATH isn't used: {dir:?}");
        }
    }
}
