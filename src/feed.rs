//! Your subscriptions' new videos, from each channel's public Atom feed
//! (`/feeds/videos.xml?channel_id=…`): its 15 newest uploads, Shorts among
//! them (their link is `/shorts/…`). No account, no API key. YouTube's
//! feed server often answers 500 or 404 for a moment, so each fetch is
//! retried a few times before the channel is given up on until next time.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use quick_xml::Reader;
use quick_xml::events::Event;
use tokio::sync::Semaphore;

use crate::ids::{ChannelId, VideoId};
use crate::video::{self, MAX_CHANNEL, MAX_DESCRIPTION, MAX_TITLE, Video};

/// Feeds fetched at once.
pub const PARALLEL: usize = 6;
/// Tries per feed: YouTube's feed server fails about one request in three.
const TRIES: u32 = 4;
/// The biggest feed read. Real ones are 20–60 KB.
const MAX_BYTES: usize = 2 * 1024 * 1024;

/// A fetched feed: the channel's name and its newest videos.
#[derive(Debug, PartialEq)]
pub struct Feed {
    pub channel: String,
    pub videos: Vec<Video>,
}

/// The HTTP client every request to YouTube's web servers uses: rustls,
/// short timeouts, no cookies, at most a few redirects.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(concat!("tuitube/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("the TLS backend is built in")
}

/// Fetches `channel`'s feed, retrying when YouTube's server fails.
pub async fn fetch(
    client: &reqwest::Client,
    limit: &Arc<Semaphore>,
    channel: &ChannelId,
) -> Result<Feed> {
    let mut last = None;
    for attempt in 0..TRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(400 * 2u64.pow(attempt))).await;
        }
        let _permit = limit.acquire().await?;
        match fetch_once(client, channel).await {
            Ok(feed) => return Ok(feed),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no tries")))
}

async fn fetch_once(client: &reqwest::Client, channel: &ChannelId) -> Result<Feed> {
    let mut response = client.get(channel.feed_url()).send().await?;
    if !response.status().is_success() {
        bail!("HTTP {}", response.status().as_u16());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_BYTES {
            bail!("the feed is too big");
        }
        body.extend_from_slice(&chunk);
    }
    parse(&String::from_utf8_lossy(&body), channel)
}

/// Reads a channel's Atom feed. Only entries naming `channel` count: a feed
/// is the channel's own, so anything else in it is ignored.
pub fn parse(xml: &str, channel: &ChannelId) -> Result<Feed> {
    let mut reader = Reader::from_str(xml);
    let mut path: Vec<String> = Vec::new();
    let mut feed_title = String::new();
    let mut entry: Option<Entry> = None;
    let mut videos = Vec::new();
    let mut saw_feed = false;
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let name = e.name().as_ref().to_string();
                if name == "feed" {
                    saw_feed = true;
                }
                if name == "entry" {
                    entry = Some(Entry::default());
                }
                path.push(name);
            }
            Event::Empty(e) => {
                let name = e.name();
                let attr = |key: &str| -> Option<String> {
                    let a = e.try_get_attribute(key).ok()??;
                    let value = a.normalized_value(quick_xml::XmlVersion::Implicit1_0);
                    Some(value.ok()?.into_owned())
                };
                if let Some(entry) = entry.as_mut() {
                    match name.as_ref() {
                        "link" if attr("rel").as_deref() == Some("alternate") => {
                            entry.link = attr("href").unwrap_or_default();
                        }
                        "media:thumbnail" => entry.thumbnail = attr("url"),
                        "media:statistics" => {
                            entry.views = attr("views").and_then(|v| v.parse().ok());
                        }
                        _ => {}
                    }
                }
            }
            Event::End(_) => {
                if path.pop().as_deref() == Some("entry")
                    && let Some(done) = entry.take()
                    && let Some(video) = done.video(channel)
                {
                    videos.push(video);
                }
            }
            Event::Text(t) => text_into(&path, &mut entry, &mut feed_title, &t.xml10_content()),
            Event::CData(t) => {
                text_into(&path, &mut entry, &mut feed_title, &t.into_inner());
            }
            Event::GeneralRef(r) => {
                let c = match r.resolve_char_ref() {
                    Ok(Some(c)) => Some(c),
                    _ => predefined_entity(&r),
                };
                if let Some(c) = c {
                    text_into(
                        &path,
                        &mut entry,
                        &mut feed_title,
                        c.encode_utf8(&mut [0; 4]),
                    );
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_feed {
        bail!("not a feed");
    }
    let channel_name = video::one_line(&feed_title, MAX_CHANNEL);
    for v in &mut videos {
        if v.channel.is_empty() {
            v.channel = channel_name.clone();
        }
    }
    Ok(Feed {
        channel: channel_name,
        videos,
    })
}

fn predefined_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

/// Adds text to whichever field the element at `path` is.
fn text_into(path: &[String], entry: &mut Option<Entry>, feed_title: &mut String, text: &str) {
    let last = path.last().map(String::as_str);
    let Some(entry) = entry else {
        if last == Some("title") && path.len() == 2 {
            push_capped(feed_title, text, MAX_CHANNEL * 4);
        }
        return;
    };
    let field = match last {
        Some("yt:videoId") => &mut entry.id,
        Some("yt:channelId") => &mut entry.channel_id,
        Some("title") if path.len() == 3 => &mut entry.title,
        Some("name") if path.iter().any(|p| p == "author") => &mut entry.author,
        Some("published") => &mut entry.published,
        Some("media:description") => &mut entry.description,
        _ => return,
    };
    push_capped(field, text, MAX_DESCRIPTION * 4);
}

/// Appends without letting a hostile feed grow a field without end.
fn push_capped(field: &mut String, text: &str, max_bytes: usize) {
    if field.len() < max_bytes {
        field.push_str(crate::text::first_chars(text, max_bytes));
    }
}

#[derive(Default)]
struct Entry {
    id: String,
    channel_id: String,
    title: String,
    author: String,
    published: String,
    description: String,
    link: String,
    thumbnail: Option<String>,
    views: Option<u64>,
}

impl Entry {
    fn video(self, channel: &ChannelId) -> Option<Video> {
        let id = VideoId::parse(self.id.trim())?;
        if self.channel_id.trim() != channel.as_str() {
            return None;
        }
        let published = chrono::DateTime::parse_from_rfc3339(self.published.trim())
            .ok()
            .map(|t| t.timestamp());
        let short = self.link.starts_with("https://www.youtube.com/shorts/");
        Some(Video {
            id,
            title: video::one_line(&self.title, MAX_TITLE),
            channel_id: Some(channel.clone()),
            channel: video::one_line(&self.author, MAX_CHANNEL),
            description: video::one_line(&self.description, MAX_DESCRIPTION),
            published,
            views: self.views,
            duration: None,
            short,
            live: false,
            upcoming: false,
            thumbnail: video::checked_image_url(self.thumbnail.as_deref()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CH: &str = "UC7EVSn5inapL20oPSwAwEUg";

    fn sample() -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/" xmlns="http://www.w3.org/2005/Atom">
 <title>Bek &amp; Brace</title>
 <entry>
  <id>yt:video:rER_yvS0fKw</id>
  <yt:videoId>rER_yvS0fKw</yt:videoId>
  <yt:channelId>{CH}</yt:channelId>
  <title>Breakfast &#x1F1FA;&#x1F1F8; in Miami</title>
  <link rel="alternate" href="https://www.youtube.com/shorts/rER_yvS0fKw"/>
  <author><name>BekBrace</name></author>
  <published>2026-09-19T07:57:09+00:00</published>
  <media:group>
   <media:thumbnail url="https://i3.ytimg.com/vi/rER_yvS0fKw/hqdefault.jpg" width="480" height="360"/>
   <media:description></media:description>
   <media:community><media:statistics views="443"/></media:community>
  </media:group>
 </entry>
 <entry>
  <yt:videoId>dP5KpyC1PN8</yt:videoId>
  <yt:channelId>{CH}</yt:channelId>
  <title>Aventura Mall</title>
  <link rel="alternate" href="https://www.youtube.com/watch?v=dP5KpyC1PN8"/>
  <author><name>BekBrace</name></author>
  <published>2026-09-18T20:41:36+00:00</published>
  <media:group>
   <media:thumbnail url="https://evil.example/x.jpg"/>
   <media:description>Come along
with me &lt;3</media:description>
  </media:group>
 </entry>
 <entry>
  <yt:videoId>xxxxxxxxxxx</yt:videoId>
  <yt:channelId>UCsomebodyElsexxxxxxxxxx</yt:channelId>
  <title>Not this channel's</title>
 </entry>
</feed>"#
        )
    }

    #[test]
    fn a_feed_gives_the_channels_videos_with_shorts_marked() {
        let channel = ChannelId::parse(CH).unwrap();
        let feed = parse(&sample(), &channel).unwrap();
        assert_eq!(feed.channel, "Bek & Brace");
        assert_eq!(
            feed.videos.len(),
            2,
            "the other channel's entry is left out"
        );
        let short = &feed.videos[0];
        assert_eq!(short.id.as_str(), "rER_yvS0fKw");
        assert_eq!(short.title, "Breakfast 🇺🇸 in Miami");
        assert!(short.short);
        assert_eq!(short.views, Some(443));
        assert_eq!(short.published, Some(1_789_804_629));
        assert_eq!(
            short.thumbnail.as_deref(),
            Some("https://i3.ytimg.com/vi/rER_yvS0fKw/hqdefault.jpg")
        );
        let video = &feed.videos[1];
        assert!(!video.short);
        assert_eq!(video.description, "Come along with me <3");
        assert_eq!(video.thumbnail, None, "not one of YouTube's image hosts");
        assert_eq!(video.channel, "BekBrace");
    }

    #[test]
    fn an_error_page_is_not_a_feed() {
        let channel = ChannelId::parse(CH).unwrap();
        assert!(parse("<!DOCTYPE html><html><p>404", &channel).is_err());
    }
}
