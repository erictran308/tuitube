//! Subscriptions from Google Takeout: `subscriptions.csv`, in
//! `Takeout/YouTube and YouTube Music/subscriptions/`. Three columns:
//! channel id, channel URL, channel title. The header is in the account's
//! language, so rows are read by position, and a row counts only if its id
//! is a channel id.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config;
use crate::ids::ChannelId;
use crate::video::{MAX_CHANNEL, one_line};

/// The biggest file read: a Takeout file of 5,000 subscriptions is about
/// 500 KB.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The channels in a Takeout `subscriptions.csv`, with their names.
pub fn read(path: &Path) -> Result<Vec<(ChannelId, String)>> {
    let shown = config::shown(path);
    if config::on_another_machine(path) {
        bail!("{shown} is on another computer");
    }
    let size = std::fs::metadata(path)
        .with_context(|| format!("cannot read {shown}"))?
        .len();
    if size > MAX_BYTES {
        bail!("{shown} is too big to be a Takeout subscriptions file");
    }
    let data = std::fs::read(path).with_context(|| format!("cannot read {shown}"))?;
    let channels = parse(&data);
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
    for record in reader.records().flatten() {
        let field = |i| record.get(i).unwrap_or("").trim();
        let id = ChannelId::parse(field(0)).or_else(|| ChannelId::from_url(field(1)));
        let Some(id) = id else {
            continue;
        };
        if channels.iter().any(|(seen, _)| *seen == id) {
            continue;
        }
        channels.push((id, one_line(field(2), MAX_CHANNEL)));
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
