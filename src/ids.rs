//! YouTube's ids, checked once where they come in. Every URL tuitube opens
//! or hands to yt-dlp and mpv is built from one of these, never from text a
//! video, a feed or a file supplied: an id can start with `-`, and a URL
//! taken as is could be any site, or an option.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An 11-character video id: letters, digits, `-` and `_`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VideoId(String);

/// A channel id: `UC` and 22 more of the same characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChannelId(String);

fn id_chars(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl VideoId {
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == 11 && id_chars(s)).then(|| Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The video's page, which mpv, yt-dlp and the browser are given.
    pub fn url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.0)
    }

    /// YouTube's 1280×720 thumbnail, as WebP (half the size of the JPEG).
    /// Very old videos don't have one.
    pub fn large_thumbnail_url(&self) -> String {
        format!("https://i.ytimg.com/vi_webp/{}/hq720.webp", self.0)
    }

    /// The thumbnail YouTube makes for every video, 480×360 with black bars
    /// above and below a 16:9 picture (cropped away when drawn).
    pub fn thumbnail_url(&self) -> String {
        format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", self.0)
    }
}

impl ChannelId {
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == 24 && s.starts_with("UC") && id_chars(s)).then(|| Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn url(&self) -> String {
        format!("https://www.youtube.com/channel/{}", self.0)
    }

    /// The channel's uploads, newest first, as yt-dlp lists them.
    pub fn videos_url(&self) -> String {
        format!("https://www.youtube.com/channel/{}/videos", self.0)
    }

    /// The channel's live streams and premieres, past and coming.
    pub fn streams_url(&self) -> String {
        format!("https://www.youtube.com/channel/{}/streams", self.0)
    }

    /// YouTube's Atom feed of the channel's 15 newest uploads.
    pub fn feed_url(&self) -> String {
        format!(
            "https://www.youtube.com/feeds/videos.xml?channel_id={}",
            self.0
        )
    }

    /// The channel id from a channel URL (`https://www.youtube.com/channel/UC…`),
    /// as Google Takeout writes them.
    pub fn from_url(url: &str) -> Option<Self> {
        let rest = url
            .strip_prefix("https://www.youtube.com/channel/")
            .or_else(|| url.strip_prefix("http://www.youtube.com/channel/"))
            .or_else(|| url.strip_prefix("https://youtube.com/channel/"))?;
        Self::parse(rest.split(['/', '?']).next()?)
    }
}

impl TryFrom<String> for VideoId {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Self::parse(&s).ok_or_else(|| "not a video id".into())
    }
}

impl TryFrom<String> for ChannelId {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Self::parse(&s).ok_or_else(|| "not a channel id".into())
    }
}

impl From<VideoId> for String {
    fn from(id: VideoId) -> String {
        id.0
    }
}

impl From<ChannelId> for String {
    fn from(id: ChannelId) -> String {
        id.0
    }
}

impl fmt::Display for VideoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `url` may be fetched as a thumbnail or channel photo: https, on
/// one of the hosts YouTube serves images from. Image URLs come from yt-dlp
/// and feeds, so they're checked rather than trusted.
pub fn image_url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    // No userinfo or port tricks: `i.ytimg.com@evil.example`.
    if host.contains(['@', ':', '\\']) {
        return false;
    }
    host == "i.ytimg.com"
        || host == "yt3.googleusercontent.com"
        || host == "yt3.ggpht.com"
        || host
            .strip_prefix('i')
            .and_then(|h| h.strip_suffix(".ytimg.com"))
            .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_ids_are_eleven_safe_characters() {
        assert!(VideoId::parse("dQw4w9WgXcQ").is_some());
        assert!(VideoId::parse("-w27jSj882I").is_some(), "may start with -");
        assert!(VideoId::parse("dQw4w9WgXc").is_none());
        assert!(VideoId::parse("dQw4w9WgXcQQ").is_none());
        assert!(VideoId::parse("dQw4w9 gXcQ").is_none());
        assert!(VideoId::parse("../../etc/p").is_none());
    }

    #[test]
    fn channel_ids_start_with_uc() {
        assert!(ChannelId::parse("UC7EVSn5inapL20oPSwAwEUg").is_some());
        assert!(ChannelId::parse("7EVSn5inapL20oPSwAwEUg").is_none());
        assert!(ChannelId::parse("UC7EVSn5inapL20oPSwAwEU/").is_none());
        let from_url =
            ChannelId::from_url("http://www.youtube.com/channel/UC7EVSn5inapL20oPSwAwEUg");
        assert_eq!(from_url.unwrap().as_str(), "UC7EVSn5inapL20oPSwAwEUg");
        assert!(
            ChannelId::from_url("https://evil.example/channel/UC7EVSn5inapL20oPSwAwEUg").is_none()
        );
    }

    #[test]
    fn only_youtubes_image_hosts_are_fetched() {
        assert!(image_url_allowed(
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"
        ));
        assert!(image_url_allowed(
            "https://i3.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"
        ));
        assert!(image_url_allowed(
            "https://yt3.googleusercontent.com/abc=s88"
        ));
        assert!(!image_url_allowed("http://i.ytimg.com/vi/x/hqdefault.jpg"));
        assert!(!image_url_allowed("https://i.ytimg.com@evil.example/x.jpg"));
        assert!(!image_url_allowed("https://i.ytimg.com.evil.example/x.jpg"));
        assert!(!image_url_allowed("https://evil.example/i.ytimg.com/x.jpg"));
        assert!(!image_url_allowed("https://i.ytimg.com:8443/x.jpg"));
        assert!(!image_url_allowed("https://iab.ytimg.com/x.jpg"));
    }
}
