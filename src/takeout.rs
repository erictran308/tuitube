//! Subscriptions from Google Takeout: `subscriptions.csv`, in
//! `Takeout/YouTube and YouTube Music/subscriptions/`. Three columns:
//! channel id, channel URL, channel title. The header is in the account's
//! language, so rows are read by position, and a row counts only if its id
//! is a channel id.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config;
use crate::ids::ChannelId;
use crate::video::{MAX_CHANNEL, one_line};

/// The biggest file read: a Takeout file of 5,000 subscriptions is about
/// 500 KB.
const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// The most channels imported: YouTube itself stops at 2,000 subscriptions.
pub const MAX_CHANNELS: usize = 5_000;

/// The channels in a Takeout `subscriptions.csv`, with their names.
pub fn read(path: &Path) -> Result<Vec<(ChannelId, String)>> {
    let shown = config::shown(path);
    if config::maybe_remote(path) {
        bail!(
            "{shown} is on another computer, or may be (on Windows, use a path with a drive letter)"
        );
    }
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
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut data)
        .with_context(|| format!("cannot read {shown}"))?;
    if data.len() as u64 > MAX_BYTES {
        bail!("{shown} is too big to be a Takeout subscriptions file");
    }
    let channels = parse(&data);
    if channels.len() > MAX_CHANNELS {
        bail!("{shown} lists more than {MAX_CHANNELS} channels");
    }
    if channels.is_empty() {
        bail!(
            "{shown} lists no channels: it should be subscriptions.csv from Google Takeout \
             (Channel Id, Channel Url, Channel Title)"
        );
    }
    Ok(channels)
}

fn parse(data: &[u8]) -> Vec<(ChannelId, String)> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(data);
    let mut channels: Vec<(ChannelId, String)> = Vec::new();
    let mut seen = HashSet::new();
    for record in reader.records().flatten() {
        let field = |i| record.get(i).unwrap_or("").trim();
        let id = ChannelId::parse(field(0)).or_else(|| ChannelId::from_url(field(1)));
        let Some(id) = id else {
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
        let channels = parse(csv.as_bytes());
        let names: Vec<_> = channels.iter().map(|(_, name)| name.as_str()).collect();
        assert_eq!(names, ["BekBrace", "Fireship, the \"real\" one"]);
    }

    #[test]
    fn names_are_cleaned_and_kept_to_one_line() {
        let csv = "UC7EVSn5inapL20oPSwAwEUg,,\"Bek\u{1b}[31m\nBrace\u{202E}\"\n";
        assert_eq!(parse(csv.as_bytes())[0].1, "Bek[31m Brace");
    }

    #[cfg(unix)]
    #[test]
    fn devices_and_pipes_are_refused_without_reading_them() {
        let dir = std::env::temp_dir().join(format!("tuitube-takeout-dev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("subscriptions.csv");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
        let error = read(&link).unwrap_err().to_string();
        assert!(error.contains("isn't a file"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_without_channels_is_an_error() {
        let dir = std::env::temp_dir().join(format!("tuitube-takeout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("subscriptions.csv");
        std::fs::write(&path, "just,some,text\n").unwrap();
        assert!(read(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
