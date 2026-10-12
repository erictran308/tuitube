//! DeArrow (sponsor.ajay.app): titles its users wrote for videos whose own
//! are clickbait, shown in their place. It's off until turned on (the
//! settings tab, or `dearrow = true`), since it tells a server that isn't
//! YouTube's which videos are on screen. As with SponsorBlock, tuitube sends
//! only the first 4 hex characters of the SHA-256 of a video's id, gets back
//! the titles of every video whose id hashes the same, and picks its own
//! out of them here. Thumbnails aren't asked for: they'd need the whole id,
//! sent to another server.
//!
//! The answer is untrusted like everything else: at most
//! `sponsorblock::MAX_BYTES`, read leniently, the title cleaned and cut as
//! YouTube's are. It's only shown: what's stored, searched and blocked is
//! YouTube's title. Requests go through a client that follows no redirect
//! (`feed::client_without_redirects`).

use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;

use crate::ids::VideoId;
use crate::sponsorblock;
use crate::video;

/// The branding endpoint, which takes a hash prefix of the id.
const API: &str = "https://sponsor.ajay.app/api/branding/";
/// Requests going at once.
const AT_ONCE: usize = 4;
/// Requests a session, at most.
const MAX_ASKS: usize = 3000;
/// Requests failing in a row that stop the asking until DeArrow is turned
/// on again: the server is down, and each card would fail the same way.
const MAX_FAILURES: usize = 3;

/// DeArrow's title for `id`, if its users gave it one that isn't YouTube's
/// own.
pub async fn title(client: &reqwest::Client, id: &VideoId) -> Result<Option<String>> {
    let url = format!("{API}{}", sponsorblock::hash_prefix(id));
    match sponsorblock::fetch(client, &url).await? {
        Some(body) => parse(&body, id),
        // No video in the bucket has a title or thumbnail.
        None => Ok(None),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Branding {
    #[serde(deserialize_with = "crate::ytdlp::lenient")]
    titles: Vec<Title>,
}

#[derive(Deserialize)]
struct Title {
    title: String,
    #[serde(default)]
    original: bool,
    #[serde(default)]
    votes: f64,
    #[serde(default)]
    locked: bool,
}

/// Picks `id`'s title out of its bucket: the best (locked first, then by
/// votes), as DeArrow's own extension does, if it's locked or not voted
/// down, and isn't YouTube's own title.
fn parse(body: &[u8], id: &VideoId) -> Result<Option<String>> {
    let mut videos: HashMap<String, serde_json::Value> = serde_json::from_slice(body)?;
    let Some(branding) = videos.remove(id.as_str()) else {
        return Ok(None);
    };
    let branding: Branding = serde_json::from_value(branding).unwrap_or_default();
    let best = branding
        .titles
        .into_iter()
        .filter(|t| t.votes.is_finite())
        .reduce(|best, t| {
            if (t.locked, t.votes) > (best.locked, best.votes) {
                t
            } else {
                best
            }
        });
    Ok(best
        .filter(|t| !t.original && (t.locked || t.votes >= 0.0))
        .map(|t| tidy(&t.title))
        .filter(|t| !t.is_empty()))
}

/// A title as DeArrow's extension shows it: the server's `‹` back to `<`
/// (it stands in for HTML), the `>` that keeps a word from its formatter
/// dropped, then cleaned as YouTube's are.
fn tidy(title: &str) -> String {
    let title = title.replace('‹', "<");
    let mut kept = String::with_capacity(title.len());
    let mut after_space = true;
    let mut chars = title.chars().peekable();
    while let Some(c) = chars.next() {
        let marks_word =
            c == '>' && after_space && chars.peek().is_some_and(|n| !n.is_whitespace());
        if !marks_word {
            kept.push(c);
        }
        after_space = c.is_whitespace();
    }
    video::one_line(&kept, video::MAX_TITLE)
}

/// DeArrow's titles this session, asked for as cards come on screen.
#[derive(Debug, Default)]
pub struct Titles {
    /// By video: `None` while asked, or if it has none.
    known: HashMap<VideoId, Option<String>>,
    /// Cards on screen not asked about yet, for after the frame.
    wanted: Vec<VideoId>,
    /// Requests going.
    asking: usize,
    /// Requests made this session.
    asked: usize,
    /// Requests that failed in a row.
    failures: usize,
}

impl Titles {
    /// DeArrow's title for `id`, once it has said one.
    pub fn get(&self, id: &VideoId) -> Option<&str> {
        self.known.get(id)?.as_deref()
    }

    /// A card on screen: asked about after the frame, once a session.
    pub fn want(&mut self, id: &VideoId) {
        if !self.known.contains_key(id) && !self.wanted.contains(id) {
            self.wanted.push(id.clone());
        }
    }

    /// The videos to ask about now: the cards wanted, as many as can go at
    /// once, none once the session's requests are used up or the server has
    /// kept failing. The rest wait for the next frame to want them again.
    pub fn take_wanted(&mut self) -> Vec<VideoId> {
        let wanted = std::mem::take(&mut self.wanted);
        if self.failures >= MAX_FAILURES {
            return Vec::new();
        }
        let room = AT_ONCE
            .saturating_sub(self.asking)
            .min(MAX_ASKS.saturating_sub(self.asked));
        let ids: Vec<VideoId> = wanted.into_iter().take(room).collect();
        for id in &ids {
            self.known.insert(id.clone(), None);
        }
        self.asking += ids.len();
        self.asked += ids.len();
        ids
    }

    /// DeArrow answered about `id`. Says whether this failure is the one
    /// that stops the asking, to be said once.
    pub fn answered(&mut self, id: VideoId, result: &Result<Option<String>>) -> bool {
        self.asking = self.asking.saturating_sub(1);
        match result {
            Ok(title) => {
                self.failures = 0;
                self.known.insert(id, title.clone());
                false
            }
            Err(_) => {
                // Asked again when it next comes on screen.
                self.known.remove(&id);
                self.failures += 1;
                self.failures == MAX_FAILURES
            }
        }
    }

    /// Turned on again: the server gets another chance.
    pub fn retry(&mut self) {
        self.failures = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> VideoId {
        VideoId::parse(s).unwrap()
    }

    #[test]
    fn the_best_title_of_the_video_asked_about_is_kept() {
        let body = br#"{
            "dQw4w9WgXcQ": {"titles": [
                {"title": "Second", "original": false, "votes": 5, "locked": false},
                {"title": "Rick Astley - >Never gonna give you up (a > b)", "original": false, "votes": 1, "locked": true},
                "not a title"
            ], "thumbnails": []},
            "q4FQa7N4kBg": {"titles": [{"title": "Someone else's", "votes": 9, "original": false, "locked": true}]}
        }"#;
        assert_eq!(
            parse(body, &id("dQw4w9WgXcQ")).unwrap().as_deref(),
            Some("Rick Astley - Never gonna give you up (a > b)"),
            "locked first; the formatter's > dropped"
        );
        assert_eq!(
            parse(body, &id("aaaaaaaaaaa")).unwrap(),
            None,
            "not in the bucket"
        );
    }

    #[test]
    fn youtubes_own_or_a_voted_down_title_isnt_shown() {
        let one = |title: &str| {
            let body = format!(r#"{{"dQw4w9WgXcQ": {{"titles": [{title}]}}}}"#);
            parse(body.as_bytes(), &id("dQw4w9WgXcQ")).unwrap()
        };
        assert_eq!(
            one(r#"{"title": "Original", "original": true, "votes": 3, "locked": false}"#),
            None,
            "the crowd picked YouTube's"
        );
        assert_eq!(
            one(r#"{"title": "Bad", "original": false, "votes": -1, "locked": false}"#),
            None
        );
        assert_eq!(
            one(r#"{"title": "New", "original": false, "votes": 0, "locked": false}"#).as_deref(),
            Some("New"),
            "one submitter's, as the extension shows"
        );
        assert_eq!(
            one("{\"title\": \"\\u202eevil\\u0007 ‹b›  line\\nbreak\", \"votes\": 0}").as_deref(),
            Some("evil <b› line break"),
            "cleaned as YouTube's titles are"
        );
        assert_eq!(one(r#"{"title": "   ", "votes": 2}"#), None);
        assert!(parse(b"[]", &id("dQw4w9WgXcQ")).is_err());
        let body = br#"{"dQw4w9WgXcQ": {"titles": {"no": "list"}}}"#;
        assert_eq!(parse(body, &id("dQw4w9WgXcQ")).unwrap(), None);
    }

    #[test]
    fn titles_are_asked_a_few_at_a_time_once_each() {
        let mut titles = Titles::default();
        let ids: Vec<VideoId> = (0..6).map(|i| id(&format!("video{i:06}"))).collect();
        for id in &ids {
            titles.want(id);
            titles.want(id);
        }
        let first = titles.take_wanted();
        assert_eq!(first, ids[..AT_ONCE], "as many as go at once");
        assert!(titles.take_wanted().is_empty(), "the rest wait their turn");
        assert!(!titles.answered(ids[0].clone(), &Ok(Some("Better".into()))));
        assert!(!titles.answered(ids[1].clone(), &Ok(None)));
        assert_eq!(titles.get(&ids[0]), Some("Better"));
        assert_eq!(titles.get(&ids[1]), None);
        for id in &ids {
            titles.want(id);
        }
        assert_eq!(titles.take_wanted(), ids[4..], "asked about once a session");
    }

    #[test]
    fn a_server_that_keeps_failing_is_left_alone_until_turned_on_again() {
        let mut titles = Titles::default();
        let ids: Vec<VideoId> = (0..4).map(|i| id(&format!("video{i:06}"))).collect();
        for id in &ids {
            titles.want(id);
        }
        let asked = titles.take_wanted();
        let failed = |titles: &mut Titles, id: &VideoId| {
            titles.answered(id.clone(), &Err(anyhow::anyhow!("HTTP 502")))
        };
        assert!(!failed(&mut titles, &asked[0]));
        assert!(!failed(&mut titles, &asked[1]));
        assert!(failed(&mut titles, &asked[2]), "said once");
        assert!(!failed(&mut titles, &asked[3]));
        titles.want(&ids[0]);
        assert!(titles.take_wanted().is_empty());
        titles.retry();
        titles.want(&ids[0]);
        assert_eq!(
            titles.take_wanted(),
            [ids[0].clone()],
            "a failed one is asked again"
        );
    }
}

/// Against the real server: `cargo test -- --ignored live`.
#[cfg(test)]
mod live {
    use super::*;

    #[tokio::test]
    #[ignore = "talks to sponsor.ajay.app"]
    async fn live_dearrow_answers_for_a_bucket() {
        let client = crate::feed::client_without_redirects();
        // Its DeArrow title was locked in October 2026.
        let id = VideoId::parse("dQw4w9WgXcQ").unwrap();
        let title = title(&client, &id).await.unwrap();
        assert!(title.is_some_and(|t| t.contains("Rick Astley")));
    }
}
