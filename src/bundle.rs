//! On macOS, mpv runs from inside an app bundle tuitube makes,
//! `<data>/player/<key>/tuitube.app`, so notch apps show what it plays.
//!
//! mpv publishes Now Playing itself (title, position, ⏯ ⏭ ⏮), and macOS takes
//! it from any process. But a bare program has no bundle id: macOS makes one
//! up from its signature (`mpv-5555…`), `NSRunningApplication` has none, and
//! notch apps that pick a player by its app (Vorssaint) leave it out. Run
//! from a bundle that its signature names, mpv is "tuitube" to all of them.
//!
//! The bundle holds a copy of mpv: macOS finds the bundle from the path the
//! program ran from, so a link wouldn't do, and signing a hard link would
//! change Homebrew's own file. Homebrew's mpv loads its libraries from
//! absolute paths, so the copy runs as is; one that doesn't (`--version`
//! fails) isn't used. The bundle is made again whenever the mpv found
//! changes ([`key`]), as `brew upgrade` rebuilds it: under a temporary name,
//! signed, tried, then renamed into place, so a half-made one never runs.
//! mpv acts the same in it: its own bundle mode needs `MPVBUNDLE=true`,
//! which tuitube's cleaned environment never has.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

use crate::config;
use crate::tools::{self, Tools};

/// Signs the copy, ad hoc, so its signature names the bundle. A system
/// program, by absolute path.
const CODESIGN: &str = "/usr/bin/codesign";

/// The bundle's description: its id (what notch apps and Control Center know
/// the player by) and name, and mpv as its program.
const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>io.github.erictran308.tuitube</string>
	<key>CFBundleName</key>
	<string>tuitube</string>
	<key>CFBundleDisplayName</key>
	<string>tuitube</string>
	<key>CFBundleExecutable</key>
	<string>mpv</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
</dict>
</plist>
"#;

/// A bundle left half-made (tuitube stopped while making it) is deleted
/// once it's this old: younger, another tuitube may still be making it.
const LEFT_FOR: Duration = Duration::from_secs(3600);

/// On macOS, points `tools.mpv` at the copy in tuitube's bundle, made first
/// if it isn't there yet. Why it couldn't, if it couldn't: mpv then runs as
/// itself, which plays the same.
pub fn put_mpv_in(tools: &mut Tools, data_dir: &Path) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mpv = tools.mpv.clone()?;
    match program(data_dir, &mpv) {
        Ok(program) => {
            tools.mpv = Some(program);
            None
        }
        Err(e) => Some(crate::text::clean(&format!("{e:#}"))),
    }
}

/// mpv in the bundle for `mpv`, made if it isn't there.
fn program(data_dir: &Path, mpv: &Path) -> Result<PathBuf> {
    let dir = data_dir.join("player");
    config::private_dir(&dir)?;
    let key = key(mpv)?;
    let ready = dir.join(&key);
    let program = executable(&ready);
    if tools::usable(&program) {
        return Ok(program);
    }
    let building = dir.join(format!(".{key}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&building);
    let made = build(&building, mpv).and_then(|()| {
        // Only ever made by a rename: whatever is there is left over.
        let _ = std::fs::remove_dir_all(&ready);
        std::fs::rename(&building, &ready)
            .with_context(|| format!("cannot move it to {}", config::shown(&ready)))
    });
    if let Err(e) = made {
        let _ = std::fs::remove_dir_all(&building);
        return Err(e);
    }
    prune(&dir, &key);
    Ok(program)
}

/// The program in the bundle made in `dir`.
fn executable(dir: &Path) -> PathBuf {
    dir.join("tuitube.app/Contents/MacOS/mpv")
}

/// Names the bundle for this mpv: where it really is (Homebrew's `bin/mpv`
/// is a link to a folder named for the version), its size and time, and the
/// bundle's description. Any change makes a new one.
fn key(mpv: &Path) -> Result<String> {
    let real = std::fs::canonicalize(mpv)
        .with_context(|| format!("cannot find {}", config::shown(mpv)))?;
    let meta = std::fs::metadata(&real)
        .with_context(|| format!("cannot read {}", config::shown(&real)))?;
    let changed = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .unwrap_or_default();
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(real.to_string_lossy().as_bytes());
    hash.update(&meta.len().to_le_bytes());
    hash.update(&changed.as_nanos().to_le_bytes());
    hash.update(INFO_PLIST.as_bytes());
    let digest = hash.finish();
    Ok(digest.as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Whether `name` is a bundle's folder ([`key`]'s 16 hex digits).
fn is_key(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Makes the bundle in `dir`: its description, a copy of `mpv`, signed,
/// then tried.
fn build(dir: &Path, mpv: &Path) -> Result<()> {
    let app = dir.join("tuitube.app");
    let program = executable(dir);
    let folder = program.parent().context("no folder")?;
    std::fs::create_dir_all(folder)
        .with_context(|| format!("cannot create {}", config::shown(folder)))?;
    std::fs::write(app.join("Contents/Info.plist"), INFO_PLIST)
        .context("cannot write its Info.plist")?;
    std::fs::copy(mpv, &program).with_context(|| format!("cannot copy {}", config::shown(mpv)))?;
    let mut sign = Command::new(CODESIGN);
    sign.args(["--force", "--sign", "-", "--"]).arg(&app);
    run(&mut sign).context("codesign failed")?;
    run(Command::new(&program).args(["--no-config", "--version"]))
        .context("the copy of mpv doesn't run")?;
    Ok(())
}

/// Runs `command` with nothing from tuitube's environment; its first line
/// of errors if it fails.
fn run(command: &mut Command) -> Result<()> {
    let out = command
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .context("cannot start it")?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        bail!("{}", crate::text::clean(said.lines().next().unwrap_or("")));
    }
    Ok(())
}

/// Deletes the bundles made for other mpvs, and half-made ones left over.
fn prune(dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > LEFT_FOR);
        let other = is_key(name) && name != keep;
        let left =
            name.starts_with('.') && is_key(name[1..].split('.').next().unwrap_or("")) && old;
        if meta.is_dir() && (other || left) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "io.github.erictran308.tuitube";

    #[test]
    fn the_description_names_the_bundle() {
        assert!(INFO_PLIST.contains(&format!("<string>{ID}</string>")));
        assert!(INFO_PLIST.contains("<key>CFBundleExecutable</key>\n\t<string>mpv</string>"));
        assert!(is_key("0123456789abcdef"));
        assert!(!is_key("0123456789abcde"));
        assert!(!is_key("0123456789abcdeg"));
    }

    #[test]
    fn elsewhere_mpv_runs_as_itself() {
        if cfg!(target_os = "macos") {
            return;
        }
        let mpv = PathBuf::from("/usr/bin/mpv");
        let mut tools = Tools {
            mpv: Some(mpv.clone()),
            ..Tools::default()
        };
        assert_eq!(put_mpv_in(&mut tools, Path::new("/nowhere")), None);
        assert_eq!(tools.mpv, Some(mpv));
    }

    /// A folder for a test, in the user's own temporary folder (macOS gives
    /// each user one), deleted at the end.
    #[cfg(target_os = "macos")]
    fn data_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tuitube-bundle-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        config::private_dir(&dir).unwrap();
        dir
    }

    /// A stand-in for mpv: a script that answers `--version`, or doesn't.
    #[cfg(target_os = "macos")]
    fn fake_mpv(dir: &Path, works: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("mpv");
        let status = if works { 0 } else { 3 };
        std::fs::write(&path, format!("#!/bin/sh\necho 'mpv v0'\nexit {status}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(target_os = "macos")]
    fn signed_as(app: &Path) -> String {
        let out = Command::new(CODESIGN).arg("-dv").arg(app).output().unwrap();
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        said.lines()
            .find_map(|l| l.strip_prefix("Identifier="))
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn mpv_runs_from_a_signed_bundle_made_again_when_mpv_changes() {
        let data = data_dir("made");
        let source = data_dir("source");
        let mpv = fake_mpv(&source, true);
        let mut tools = Tools {
            mpv: Some(mpv.clone()),
            ..Tools::default()
        };
        assert_eq!(put_mpv_in(&mut tools, &data), None);
        let program = tools.mpv.clone().unwrap();
        assert!(program.starts_with(data.join("player")));
        assert!(program.ends_with("tuitube.app/Contents/MacOS/mpv"));
        let app = program.ancestors().nth(3).unwrap().to_path_buf();
        assert_eq!(signed_as(&app), ID);
        let made = std::fs::metadata(&program).unwrap().modified().unwrap();

        // Found again: the same copy, not made again.
        assert_eq!(program, super::program(&data, &mpv).unwrap());
        assert_eq!(
            std::fs::metadata(&program).unwrap().modified().unwrap(),
            made
        );

        // mpv changed (brew upgrade): a new bundle, the old one deleted.
        std::fs::write(&mpv, "#!/bin/sh\necho 'mpv v1'\n").unwrap();
        let newer = super::program(&data, &mpv).unwrap();
        assert_ne!(newer, program);
        assert!(newer.exists() && !program.exists());
        let left: Vec<_> = std::fs::read_dir(data.join("player")).unwrap().collect();
        assert_eq!(left.len(), 1, "only the bundle in use");

        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&source);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn an_mpv_whose_copy_doesnt_run_runs_as_itself() {
        let data = data_dir("broken");
        let source = data_dir("broken-source");
        let mpv = fake_mpv(&source, false);
        let mut tools = Tools {
            mpv: Some(mpv.clone()),
            ..Tools::default()
        };
        let why = put_mpv_in(&mut tools, &data).expect("a reason");
        assert!(why.contains("doesn't run"), "{why}");
        assert_eq!(tools.mpv, Some(mpv));
        let left: Vec<_> = std::fs::read_dir(data.join("player")).unwrap().collect();
        assert!(left.is_empty(), "nothing half-made kept");

        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&source);
    }
}
