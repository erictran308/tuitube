//! A video as the grid shows it, from a feed, a search or a channel's list,
//! and how its numbers are written.

use crate::ids::{ChannelId, VideoId, image_url_allowed};
use crate::text;

/// Longest title kept, in characters. YouTube allows 100.
pub const MAX_TITLE: usize = 200;
/// Longest channel name kept.
pub const MAX_CHANNEL: usize = 100;
/// Longest description kept: only its start is ever shown.
pub const MAX_DESCRIPTION: usize = 500;

#[derive(Clone, Debug, PartialEq)]
pub struct Video {
    pub id: VideoId,
    pub title: String,
    pub channel_id: Option<ChannelId>,
    pub channel: String,
    /// The start of the description, on one line.
    pub description: String,
    /// When it was published, in Unix seconds.
    pub published: Option<i64>,
    pub views: Option<u64>,
    /// Length in seconds; feeds don't say.
    pub duration: Option<u32>,
    pub short: bool,
    pub live: bool,
    /// A live stream or premiere that hasn't started.
    pub upcoming: bool,
    /// A thumbnail URL on one of YouTube's image hosts; else the one every
    /// video has is used ([`VideoId::thumbnail_url`]).
    pub thumbnail: Option<String>,
}

impl Video {
    /// Where its thumbnail can be fetched, best first: the 1280×720 WebP
    /// most videos have, the one a search named, then the 480×360 every
    /// video has.
    pub fn thumbnail_urls(&self) -> Vec<String> {
        let mut urls = vec![self.id.large_thumbnail_url()];
        urls.extend(self.thumbnail.clone());
        urls.push(self.id.thumbnail_url());
        urls.dedup();
        urls
    }
}

/// Remote text made safe for one line: hidden characters gone, every run of
/// whitespace (line breaks too) one space, at most `max` characters.
pub fn one_line(s: &str, max: usize) -> String {
    let cleaned = text::clean(s);
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    text::first_chars(&joined, max).to_string()
}

/// A thumbnail URL from a feed or yt-dlp, kept only if it's on YouTube's
/// image hosts.
pub fn checked_image_url(url: Option<&str>) -> Option<String> {
    url.filter(|u| image_url_allowed(u)).map(str::to_string)
}

/// `1.2M views`, as YouTube writes them.
pub fn views(n: u64) -> String {
    let short = match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => compact(n as f64 / 1e3, "K"),
        1_000_000..1_000_000_000 => compact(n as f64 / 1e6, "M"),
        _ => compact(n as f64 / 1e9, "B"),
    };
    let unit = if n == 1 { "view" } else { "views" };
    format!("{short} {unit}")
}

/// One decimal under 10 (`1.2K`), none above (`12K`), and never `1.0K`.
fn compact(x: f64, suffix: &str) -> String {
    if x < 10.0 {
        let rounded = (x * 10.0).floor() / 10.0;
        if rounded.fract() == 0.0 {
            format!("{rounded:.0}{suffix}")
        } else {
            format!("{rounded:.1}{suffix}")
        }
    } else {
        format!("{:.0}{suffix}", x.floor())
    }
}

/// `3 days ago`, from Unix seconds `then` and `now`.
pub fn age(then: i64, now: i64) -> String {
    let seconds = now.saturating_sub(then).max(0);
    let (n, unit) = match seconds {
        0..60 => return "just now".into(),
        60..3_600 => (seconds / 60, "minute"),
        3_600..86_400 => (seconds / 3_600, "hour"),
        86_400..604_800 => (seconds / 86_400, "day"),
        604_800..2_629_800 => (seconds / 604_800, "week"),
        2_629_800..31_557_600 => (seconds / 2_629_800, "month"),
        _ => (seconds / 31_557_600, "year"),
    };
    let s = if n == 1 { "" } else { "s" };
    format!("{n} {unit}{s} ago")
}

/// `4:05`, or `1:02:03` for an hour or more.
pub fn duration(seconds: u32) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_are_written_as_youtube_does() {
        assert_eq!(views(1), "1 view");
        assert_eq!(views(999), "999 views");
        assert_eq!(views(1_000), "1K views");
        assert_eq!(views(1_250), "1.2K views");
        assert_eq!(views(45_900), "45K views");
        assert_eq!(views(638_955), "638K views");
        assert_eq!(views(1_999_999), "1.9M views");
        assert_eq!(views(3_100_000_000), "3.1B views");
    }

    #[test]
    fn ages_round_down_to_the_largest_unit() {
        let now = 1_800_000_000;
        assert_eq!(age(now - 30, now), "just now");
        assert_eq!(age(now - 60, now), "1 minute ago");
        assert_eq!(age(now - 7_200, now), "2 hours ago");
        assert_eq!(age(now - 3 * 86_400, now), "3 days ago");
        assert_eq!(age(now - 15 * 86_400, now), "2 weeks ago");
        assert_eq!(age(now - 400 * 86_400, now), "1 year ago");
        assert_eq!(age(now + 100, now), "just now", "clock skew");
        assert!(age(i64::MIN, now).ends_with("years ago"), "no overflow");
    }

    #[test]
    fn durations_show_hours_only_when_needed() {
        assert_eq!(duration(5), "0:05");
        assert_eq!(duration(245), "4:05");
        assert_eq!(duration(11_104), "3:05:04");
    }

    #[test]
    fn remote_text_becomes_one_clean_line() {
        assert_eq!(one_line("a\n\nb\u{1b}[2J\tc\u{202E}d", 100), "a b[2J cd");
        assert_eq!(one_line("héllo world", 3), "hél");
    }
}
