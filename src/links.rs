//! YouTube links pasted or typed in: a video (with the time it starts at), a
//! playlist or a channel. Only YouTube's own hosts count, and only the ids
//! are kept: what tuitube then opens is built from them ([`crate::ids`]),
//! never the link itself, so a link can't send anything elsewhere.

use crate::ids::{ChannelId, ChannelName, PlaylistId, VideoId};

/// The longest text read as a link: real ones are under 200 characters.
const MAX_LINK: usize = 2_000;
/// The latest start a link's `t=` may give: 100 hours.
const MAX_START: u32 = 100 * 3600;

/// What a link is to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Video {
        id: VideoId,
        /// Seconds in, from `t=` or `start=`.
        start: Option<u32>,
        /// A `/shorts/` link.
        short: bool,
    },
    Playlist(PlaylistId),
    Channel(ChannelId),
    /// A channel by handle or name: yt-dlp finds which.
    Named(ChannelName),
    /// Your account's own lists (Watch later, Liked videos): only YouTube
    /// signed in has them.
    AccountList,
}

/// `text` as a YouTube link, if it's one: `https://www.youtube.com/watch?…`,
/// `youtu.be/…`, `/shorts/`, `/live/`, `/embed/`, `/playlist?list=`,
/// `/channel/UC…`, `/@handle`, `/c/…`, `/user/…`, on youtube.com, its
/// `www.`, `m.` and `music.` hosts, youtu.be and youtube-nocookie.com, with
/// or without `https://`.
pub fn parse(text: &str) -> Option<Link> {
    let text = text.trim();
    if text.len() > MAX_LINK || text.chars().any(char::is_whitespace) {
        return None;
    }
    let with_scheme = if text.contains("://") {
        text.to_string()
    } else {
        format!("https://{text}")
    };
    let url = reqwest::Url::parse(&with_scheme).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let query = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    let start = || {
        query("t")
            .or_else(|| query("start"))
            .and_then(|t| seconds(&t))
    };
    let video = |id: &str, short: bool| {
        Some(Link::Video {
            id: VideoId::parse(id)?,
            start: start(),
            short,
        })
    };
    let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    if host == "youtu.be" {
        return match path.as_slice() {
            [id] => video(id, false),
            _ => None,
        };
    }
    if !matches!(
        host,
        "youtube.com"
            | "www.youtube.com"
            | "m.youtube.com"
            | "music.youtube.com"
            | "youtube-nocookie.com"
            | "www.youtube-nocookie.com"
    ) {
        return None;
    }
    match path.as_slice() {
        ["watch"] => match query("v") {
            Some(id) => video(&id, false),
            None => playlist(&query("list")?),
        },
        ["shorts", id, ..] => video(id, true),
        ["live" | "embed" | "v" | "e", id, ..] => video(id, false),
        ["playlist"] => playlist(&query("list")?),
        ["channel", id, ..] => ChannelId::parse(id).map(Link::Channel),
        ["c", name, ..] => ChannelName::custom(name).map(Link::Named),
        ["user", name, ..] => ChannelName::user(name).map(Link::Named),
        [handle, ..] => ChannelName::handle(handle.strip_prefix('@')?).map(Link::Named),
        [] => None,
    }
}

fn playlist(list: &str) -> Option<Link> {
    // Watch later, Liked videos and Liked music.
    if matches!(list, "WL" | "LL" | "LM") {
        return Some(Link::AccountList);
    }
    PlaylistId::parse(list).map(Link::Playlist)
}

/// A link's start: `90`, `90s`, `1m30s` or `1h2m3s`.
fn seconds(t: &str) -> Option<u32> {
    if let Ok(n) = t.parse::<u32>() {
        return Some(n.min(MAX_START));
    }
    let mut total: u32 = 0;
    let mut digits = String::new();
    for c in t.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit = match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        let n: u32 = digits.parse().ok()?;
        total = total.saturating_add(n.saturating_mul(unit));
        digits.clear();
    }
    digits.is_empty().then_some(total.min(MAX_START))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "dQw4w9WgXcQ";

    fn video(start: Option<u32>, short: bool) -> Option<Link> {
        Some(Link::Video {
            id: VideoId::parse(ID).unwrap(),
            start,
            short,
        })
    }

    #[test]
    fn video_links_of_every_kind() {
        for link in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "http://youtube.com/watch?v=dQw4w9WgXcQ&list=RDdQw4w9WgXcQ&index=2",
            "www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&si=abc",
            "https://youtu.be/dQw4w9WgXcQ?si=xyz",
            "youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ?feature=shared",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
            "  https://www.youtube.com/watch?v=dQw4w9WgXcQ\n",
        ] {
            assert_eq!(parse(link), video(None, false), "{link}");
        }
        assert_eq!(
            parse("https://www.youtube.com/shorts/dQw4w9WgXcQ"),
            video(None, true)
        );
    }

    #[test]
    fn a_links_start_time_is_kept() {
        assert_eq!(
            parse("https://youtu.be/dQw4w9WgXcQ?t=90"),
            video(Some(90), false)
        );
        assert_eq!(
            parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1h2m3s"),
            video(Some(3723), false)
        );
        assert_eq!(
            parse("https://www.youtube.com/embed/dQw4w9WgXcQ?start=42"),
            video(Some(42), false)
        );
        assert_eq!(
            parse("https://youtu.be/dQw4w9WgXcQ?t=soon"),
            video(None, false),
            "a start that isn't a time is left out"
        );
        assert_eq!(seconds("99999999999"), None);
        assert_eq!(seconds("4294967295"), Some(MAX_START));
        assert_eq!(seconds("2m"), Some(120));
        assert_eq!(seconds("5x"), None);
    }

    #[test]
    fn playlists_and_channels() {
        let list = "PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf";
        assert_eq!(
            parse(&format!("https://www.youtube.com/playlist?list={list}")),
            Some(Link::Playlist(PlaylistId::parse(list).unwrap()))
        );
        assert_eq!(
            parse(&format!("https://www.youtube.com/watch?list={list}")),
            Some(Link::Playlist(PlaylistId::parse(list).unwrap()))
        );
        assert_eq!(
            parse("https://www.youtube.com/playlist?list=WL"),
            Some(Link::AccountList)
        );
        let ch = "UC7EVSn5inapL20oPSwAwEUg";
        assert_eq!(
            parse(&format!("https://www.youtube.com/channel/{ch}/videos")),
            Some(Link::Channel(ChannelId::parse(ch).unwrap()))
        );
        assert_eq!(
            parse("https://www.youtube.com/@BekBrace"),
            Some(Link::Named(ChannelName::handle("BekBrace").unwrap()))
        );
        assert_eq!(
            parse("youtube.com/@bek.brace-1/streams"),
            Some(Link::Named(ChannelName::handle("bek.brace-1").unwrap()))
        );
        assert_eq!(
            parse("https://www.youtube.com/c/SomeName"),
            Some(Link::Named(ChannelName::custom("SomeName").unwrap()))
        );
        assert_eq!(
            ChannelName::handle("BekBrace").unwrap().videos_url(),
            "https://www.youtube.com/@BekBrace/videos"
        );
    }

    #[test]
    fn anything_else_is_not_a_link() {
        for text in [
            "rust programming",
            ID,
            "https://evil.example/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com.evil.example/watch?v=dQw4w9WgXcQ",
            "https://youtube.com@evil.example/watch?v=dQw4w9WgXcQ",
            "https://user:pass@www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com:8080/watch?v=dQw4w9WgXcQ",
            "javascript://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "file:///etc/passwd",
            "https://www.youtube.com/watch?v=-short",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ%0A",
            "https://www.youtube.com/@a",
            "https://www.youtube.com/@na%C3%AFve",
            "https://www.youtube.com/feed/subscriptions",
            "https://www.youtube.com/",
            "https://youtu.be/",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ extra words",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
}
