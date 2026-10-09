use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

use crate::text;

/// Settings read from the environment, or from `.env` in the working
/// directory ([`load_dotenv`]).
pub struct Config {
    /// Where your subscriptions, watch history, cached thumbnails and
    /// `settings.toml` are kept.
    pub data_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        let data_dir = data_dir()?;
        let shown = shown(&data_dir);
        std::fs::create_dir_all(&data_dir).with_context(|| format!("cannot create {shown}"))?;
        // It holds what you watch: only for this user, whatever the umask or
        // the folder it's in allow.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("cannot protect {shown}"))?;
        }
        // The default folder is in your own home; one set elsewhere must be
        // somewhere nobody else can swap it out.
        #[cfg(unix)]
        let data_dir = if var("TT_DATA_DIR").is_some() {
            private_place(&data_dir)?;
            std::fs::canonicalize(&data_dir).with_context(|| format!("cannot read {shown}"))?
        } else {
            data_dir
        };

        Ok(Self { data_dir })
    }
}

/// `TT_DATA_DIR`, else the platform's place for app data: `~/Library/Application
/// Support/tuitube` on macOS, `~/.local/share/tuitube` on Linux, `%LOCALAPPDATA%\tuitube`
/// on Windows. The same wherever the command is run from.
pub fn data_dir() -> Result<PathBuf> {
    data_dir_from(var("TT_DATA_DIR"))
}

fn data_dir_from(set: Option<String>) -> Result<PathBuf> {
    if let Some(dir) = set {
        let dir = PathBuf::from(dir);
        // On Windows even creating a folder on another machine hands that
        // server your login hash. A relative path on Windows would sit under
        // the working directory, whose permissions aren't checked.
        let elsewhere = on_another_machine(&dir)
            || (cfg!(windows) && matches!(dir.as_os_str().as_encoded_bytes(), [b'/' | b'\\', ..]))
            || (cfg!(windows) && !dir.is_absolute());
        if elsewhere {
            bail!("TT_DATA_DIR must be a folder on this computer, with a drive letter on Windows");
        }
        return Ok(dir);
    }
    let base = dirs::data_local_dir().context("no home directory found; set TT_DATA_DIR")?;
    Ok(base.join("tuitube"))
}

/// Whether `path` names a file on another machine: `\\server\share\…` (or
/// `//server/share/…`) on Windows. Looking at such a path connects to that
/// server and hands it the user's Windows login hash.
pub fn on_another_machine(path: &Path) -> bool {
    matches!(
        path.as_os_str().as_encoded_bytes(),
        [b'/' | b'\\', b'/' | b'\\', ..]
    )
}

/// A path as it can be printed: it may come from the environment.
pub fn shown(path: &Path) -> String {
    text::clean(&path.display().to_string())
}

/// Checks that only you or the system can change the folders above `dir`:
/// otherwise another account could rename the data folder away and put its
/// own in its place. A link as the folder itself must be yours too. Folders
/// anyone may write to, like `/tmp`, are fine when only an entry's owner can
/// move it (the sticky bit).
#[cfg(unix)]
fn private_place(dir: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata(dir)?.uid();
    // SAFETY: getgid can't fail and touches no memory.
    let gid = unsafe { libc::getgid() };
    let unsafe_place = |what: &Path| {
        anyhow::anyhow!(
            "{} can be changed by other users, so it's no place for your data; \
             set TT_DATA_DIR somewhere in your home folder",
            shown(what)
        )
    };
    if std::fs::symlink_metadata(dir)?.uid() != uid {
        return Err(unsafe_place(dir));
    }
    for folder in std::fs::canonicalize(dir)?.ancestors().skip(1) {
        let meta = std::fs::metadata(folder)?;
        let mode = meta.mode();
        // Your own group is only yours on Linux (user private groups); on
        // macOS every account is in `staff`.
        let group_is_others = cfg!(target_os = "macos") || meta.gid() != gid;
        let others_write = mode & 0o002 != 0 || (mode & 0o020 != 0 && group_is_others);
        let sticky = mode & 0o1000 != 0;
        let owner_ok = meta.uid() == uid || meta.uid() == 0;
        if !owner_ok || (others_write && !(sticky && meta.uid() == 0)) {
            return Err(unsafe_place(folder));
        }
    }
    Ok(())
}

/// Creates `dir` (and its parents) readable only by you.
pub fn private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", shown(dir)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot protect {}", shown(dir)))?;
    }
    Ok(())
}

/// `TT_*` settings from `.env` in the current directory, for development.
static DOTENV: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Reads `.env` from the current directory, not its parents, and keeps only
/// its `TT_*` keys, without touching the process environment. So a `.env` in
/// some untrusted folder can't set `LD_PRELOAD` or `PATH` for the programs
/// tuitube starts. Only development builds read it.
pub fn load_dotenv() {
    if !cfg!(debug_assertions) {
        let _ = DOTENV.set(HashMap::new());
        return;
    }
    let vars = dotenvy::from_path_iter(".env")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|(key, _)| key.starts_with("TT_"))
        .collect();
    let _ = DOTENV.set(vars);
}

/// A variable's trimmed value from the environment, else from `.env`, or
/// `None` if it's unset or blank.
pub fn var(name: &str) -> Option<String> {
    let value = std::env::var(name)
        .ok()
        .or_else(|| DOTENV.get()?.get(name).cloned())?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_data_folder_on_another_machine_is_refused() {
        for dir in ["//evil.example/s/tt", "\\\\evil.example\\s\\tt"] {
            assert!(data_dir_from(Some(dir.into())).is_err(), "{dir}");
        }
        let local = data_dir_from(Some("./.tuitube".into())).unwrap();
        assert_eq!(local, PathBuf::from("./.tuitube"));
    }

    #[cfg(unix)]
    #[test]
    fn a_data_folder_others_could_swap_out_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid can't fail and touches no memory.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let parent = std::env::temp_dir().join(format!("tuitube-place-{}", std::process::id()));
        let dir = parent.join("tt");
        std::fs::create_dir_all(&dir).unwrap();
        let mode = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        mode(&parent, 0o777);
        assert!(private_place(&dir).is_err(), "anyone could rename it");
        mode(&parent, 0o1777);
        assert!(private_place(&dir).is_err(), "sticky, but not the system's");
        mode(&parent, 0o755);
        private_place(&dir).unwrap();
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn paths_are_printed_without_control_characters() {
        assert_eq!(shown(Path::new("/tmp/a\u{1b}]0;x\u{7}b")), "/tmp/a]0;xb");
    }
}
