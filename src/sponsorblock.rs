//! SponsorBlock (sponsor.ajay.app): the parts of videos its users marked as
//! a sponsor read, an intro, an outro…, skipped while the video plays. It's
//! off until turned on (`B`, or `sponsorblock = true`), since it tells a
//! server that isn't YouTube's what you play. To keep that small, tuitube
//! sends only the first 4 hex characters of the SHA-256 of the video's id,
//! gets back the segments of every video whose id hashes the same (around a
//! hundred), and picks its own out of them here.
//!
//! The answer is untrusted like everything else: at most `MAX_BYTES`, read
//! leniently, only segments of the video asked about, in the categories
//! asked for, with sane times. Category names shown are tuitube's own, never
//! the server's text. Requests go through a client that follows no redirect
//! (`feed::client_without_redirects`): a redirect ends the request.

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::ids::VideoId;

/// The skip-segments endpoint, which takes a hash prefix of the id.
const API: &str = "https://sponsor.ajay.app/api/skipSegments/";
/// Hex characters of the id's SHA-256 sent: 65,536 buckets.
const PREFIX: usize = 4;
/// The biggest answer read. A bucket with every category is about 50 KB.
const MAX_BYTES: usize = 1024 * 1024;
/// Segments kept for one video, at most.
const MAX_SEGMENTS: usize = 64;
/// Segments shorter than this aren't worth a seek.
const MIN_LENGTH: f64 = 0.5;
/// The longest video a segment may be in: YouTube's limit is 12 hours.
const MAX_END: f64 = 24.0 * 3600.0;
/// A segment marked on a video whose length differs by more than this is
/// left alone: the video was cut since, so its times may be wrong.
const LENGTH_SLACK: f64 = 2.0;
/// Segments this close together are skipped as one.
const JOIN_WITHIN: f64 = 0.5;

/// The categories tuitube can skip, as SponsorBlock names them, with the
/// name shown. Others (highlights, chapters, exclusive access) aren't parts
/// to skip.
pub const CATEGORIES: [(&str, &str); 9] = [
    ("sponsor", "Sponsor"),
    ("selfpromo", "Self-promotion"),
    ("interaction", "Interaction reminder"),
    ("intro", "Intro"),
    ("outro", "Outro"),
    ("preview", "Preview"),
    ("hook", "Hook"),
    ("music_offtopic", "Non-music"),
    ("filler", "Filler"),
];

/// The category named `name`, as tuitube's own string, if it's one it
/// knows.
pub fn category(name: &str) -> Option<&'static str> {
    CATEGORIES.iter().find(|(c, _)| *c == name).map(|(c, _)| *c)
}

/// How a category is shown.
pub fn shown(category: &str) -> &'static str {
    CATEGORIES
        .iter()
        .find(|(c, _)| *c == category)
        .map_or("Segment", |(_, name)| name)
}

/// A part of a video to skip, in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub category: &'static str,
    /// The video's length when the segment was marked, if SponsorBlock
    /// knows it.
    video_length: Option<f64>,
}

impl Segment {
    #[cfg(test)]
    pub fn new(start: f64, end: f64, category: &'static str) -> Self {
        Self {
            start,
            end,
            category,
            video_length: None,
        }
    }

    /// Whether the segment was marked on a video of this length.
    fn fits(&self, length: Option<f64>) -> bool {
        match (self.video_length, length) {
            (Some(marked), Some(length)) => (marked - length).abs() <= LENGTH_SLACK,
            _ => true,
        }
    }
}

/// The first `PREFIX` hex characters of the SHA-256 of the id: all
/// SponsorBlock learns of it.
fn hash_prefix(id: &VideoId) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, id.as_str().as_bytes());
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    hex[..PREFIX].to_string()
}

/// The request for `id`'s bucket: its hash prefix, and the categories (all
/// tuitube's own strings) as repeated parameters.
fn url(id: &VideoId, categories: &[&'static str]) -> String {
    let mut url = format!("{API}{}?actionType=skip", hash_prefix(id));
    for category in categories {
        url.push_str("&category=");
        url.push_str(category);
    }
    url
}

/// The parts of `id` to skip, in `categories`, in order. None marked is an
/// empty list.
pub async fn segments(
    client: &reqwest::Client,
    id: &VideoId,
    categories: &[&'static str],
) -> Result<Vec<Segment>> {
    if categories.is_empty() {
        return Ok(Vec::new());
    }
    // Errors leave the URL out: it's long, and says nothing more.
    let mut response = client
        .get(url(id, categories))
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    // No video in the bucket has segments in these categories.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    if !response.status().is_success() {
        bail!("HTTP {}", response.status().as_u16());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        if body.len() + chunk.len() > MAX_BYTES {
            bail!("the answer is too big");
        }
        body.extend_from_slice(&chunk);
    }
    parse(&body, id, categories)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Video {
    #[serde(rename = "videoID")]
    video_id: String,
    #[serde(deserialize_with = "crate::ytdlp::lenient")]
    segments: Vec<Marked>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Marked {
    category: String,
    #[serde(rename = "actionType")]
    action_type: Option<String>,
    segment: Vec<f64>,
    #[serde(rename = "videoDuration")]
    video_duration: Option<f64>,
}

/// Picks `id`'s segments out of its bucket: skips only, in `categories`,
/// with times that make sense, in order of their start. Touching ones are
/// joined only when skipped (`Skipper::due`), once each is known to fit the
/// video playing.
fn parse(body: &[u8], id: &VideoId, categories: &[&'static str]) -> Result<Vec<Segment>> {
    let videos: Vec<serde_json::Value> = serde_json::from_slice(body)?;
    let mut segments: Vec<Segment> = videos
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Video>(v).ok())
        .filter(|v| v.video_id == id.as_str())
        .flat_map(|v| v.segments)
        .filter(|m| m.action_type.as_deref().is_none_or(|a| a == "skip"))
        .filter_map(|m| {
            let category = category(&m.category).filter(|c| categories.contains(c))?;
            let [start, end] = m.segment[..] else {
                return None;
            };
            let sane = start.is_finite()
                && end.is_finite()
                && start >= 0.0
                && end <= MAX_END
                && end - start >= MIN_LENGTH;
            sane.then(|| Segment {
                start,
                end,
                category,
                video_length: m
                    .video_duration
                    .filter(|d| d.is_finite() && *d > 0.0 && *d <= MAX_END),
            })
        })
        .take(MAX_SEGMENTS)
        .collect();
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(segments)
}

/// A playing video's segments. Each is skipped once, the first time
/// playback is inside it, so seeking back into one plays it.
#[derive(Debug, Default)]
pub struct Skipper {
    segments: Vec<(Segment, bool)>,
}

impl Skipper {
    pub fn new(segments: Vec<Segment>) -> Self {
        Self {
            segments: segments.into_iter().map(|s| (s, false)).collect(),
        }
    }

    /// What to skip `at` seconds into a video `length` long, if playback is
    /// inside a segment not skipped yet (and not at its very end, where a
    /// seek lands): that segment, run on through the ones that touch it.
    /// Each must have been marked on a video of this length.
    pub fn due(&mut self, at: f64, length: Option<f64>) -> Option<Segment> {
        let first = self.segments.iter().position(|(s, skipped)| {
            !skipped && at >= s.start && at < s.end - MIN_LENGTH / 2.0 && s.fits(length)
        })?;
        self.segments[first].1 = true;
        let mut skip = self.segments[first].0.clone();
        while let Some(next) = self.segments.iter().position(|(s, skipped)| {
            !skipped && s.start <= skip.end + JOIN_WITHIN && s.end > skip.end && s.fits(length)
        }) {
            self.segments[next].1 = true;
            skip.end = self.segments[next].0.end;
        }
        Some(skip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> VideoId {
        VideoId::parse("dQw4w9WgXcQ").unwrap()
    }

    #[test]
    fn only_a_hash_prefix_of_the_id_is_sent() {
        // printf dQw4w9WgXcQ | shasum -a 256
        assert_eq!(hash_prefix(&id()), "5f6b");
        let url = url(&id(), &["sponsor", "intro"]);
        assert_eq!(
            url,
            "https://sponsor.ajay.app/api/skipSegments/5f6b?actionType=skip&category=sponsor&category=intro"
        );
        assert!(!url.contains("dQw4w9WgXcQ"));
        assert!(
            category("sponsor&x=1").is_none(),
            "only names tuitube knows"
        );
    }

    #[test]
    fn only_this_videos_skips_in_the_asked_categories_are_kept() {
        let body = br#"[
            {"videoID":"eXjGWlJOhWg","segments":[{"category":"sponsor","actionType":"skip","segment":[1,20]}]},
            {"videoID":"dQw4w9WgXcQ","segments":[
                {"category":"outro","actionType":"skip","segment":[200,212.5],"videoDuration":212.9},
                {"category":"sponsor","actionType":"skip","segment":[30,60],"UUID":"x","votes":3},
                {"category":"sponsor","actionType":"mute","segment":[70,80]},
                {"category":"poi_highlight","actionType":"poi","segment":[90,90]},
                {"category":"selfpromo","actionType":"skip","segment":[100,110]},
                {"category":"intro","segment":[0,"x"]},
                {"category":"intro","segment":[0,5,6]},
                {"category":"intro","segment":[-5,5]},
                {"category":"intro","segment":[10,10.2]},
                {"category":"intro","segment":[0,1e300]},
                "not a segment",
                {"category":"intro","segment":[0,8.5]}
            ]}
        ]"#;
        let got = parse(body, &id(), &["sponsor", "intro", "outro"]).unwrap();
        let times: Vec<_> = got.iter().map(|s| (s.start, s.end, s.category)).collect();
        assert_eq!(
            times,
            [
                (0.0, 8.5, "intro"),
                (30.0, 60.0, "sponsor"),
                (200.0, 212.5, "outro")
            ]
        );
        assert_eq!(got[2].video_length, Some(212.9));
        assert!(parse(b"{\"error\":1}", &id(), &["sponsor"]).is_err());
        assert!(parse(b"[]", &id(), &["sponsor"]).unwrap().is_empty());
    }

    #[test]
    fn touching_segments_are_skipped_as_one_if_each_fits() {
        let body = br#"[{"videoID":"dQw4w9WgXcQ","segments":[
            {"category":"sponsor","segment":[20,40]},
            {"category":"intro","segment":[0,20.3]},
            {"category":"sponsor","segment":[25,30]},
            {"category":"outro","segment":[50,60]}
        ]}]"#;
        let segments = parse(body, &id(), &["sponsor", "intro", "outro"]).unwrap();
        let starts: Vec<_> = segments.iter().map(|s| s.start).collect();
        assert_eq!(starts, [0.0, 20.0, 25.0, 50.0], "in order, not joined yet");
        let mut skipper = Skipper::new(segments);
        let skip = skipper.due(1.0, Some(600.0)).unwrap();
        assert_eq!((skip.start, skip.end, skip.category), (0.0, 40.0, "intro"));
        assert_eq!(
            skipper.due(30.0, Some(600.0)),
            None,
            "skipped with the first"
        );
        assert_eq!(skipper.due(50.5, Some(600.0)).unwrap().end, 60.0);

        // One marked on another cut of the video isn't skipped by touching
        // one that fits.
        let body = br#"[{"videoID":"dQw4w9WgXcQ","segments":[
            {"category":"intro","segment":[0,20],"videoDuration":600},
            {"category":"sponsor","segment":[20.3,590],"videoDuration":900}
        ]}]"#;
        let mut skipper = Skipper::new(parse(body, &id(), &["sponsor", "intro"]).unwrap());
        assert_eq!(skipper.due(1.0, Some(600.0)).unwrap().end, 20.0);
        assert_eq!(skipper.due(25.0, Some(600.0)), None);
    }

    #[test]
    fn a_hostile_answer_is_cut_short() {
        let one = r#"{"category":"sponsor","segment":[1,2]}"#;
        let many: Vec<String> = (0..1000)
            .map(|i| one.replace("[1,2]", &format!("[{},{}]", i * 10, i * 10 + 5)))
            .collect();
        let body = format!(
            r#"[{{"videoID":"dQw4w9WgXcQ","segments":[{}]}}]"#,
            many.join(",")
        );
        let got = parse(body.as_bytes(), &id(), &["sponsor"]).unwrap();
        assert_eq!(got.len(), MAX_SEGMENTS);
    }

    #[test]
    fn each_segment_is_skipped_once_when_playback_is_inside_it() {
        let segment = |start, end, video_length| Segment {
            start,
            end,
            category: "sponsor",
            video_length,
        };
        let mut skipper = Skipper::new(vec![
            segment(10.0, 20.0, None),
            segment(30.0, 40.0, Some(100.0)),
        ]);
        assert_eq!(skipper.due(5.0, Some(100.0)), None);
        assert_eq!(skipper.due(10.1, Some(100.0)).unwrap().end, 20.0);
        assert_eq!(skipper.due(15.0, Some(100.0)), None, "skipped already");
        assert_eq!(skipper.due(39.9, Some(100.0)), None, "where a seek lands");
        assert_eq!(
            skipper.due(35.0, Some(130.0)),
            None,
            "marked on another cut"
        );
        assert_eq!(skipper.due(35.0, Some(101.0)).unwrap().start, 30.0);
        assert_eq!(skipper.due(35.0, Some(101.0)), None);
    }
}

/// Against the real server: `cargo test -- --ignored live`.
#[cfg(test)]
mod live {
    use super::*;

    #[tokio::test]
    #[ignore = "talks to sponsor.ajay.app"]
    async fn live_sponsorblock_answers_for_a_bucket() {
        let client = crate::feed::client();
        let all: Vec<&'static str> = CATEGORIES.iter().map(|(c, _)| *c).collect();
        let id = VideoId::parse("dQw4w9WgXcQ").unwrap();
        let response = client.get(url(&id, &all)).send().await.unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        let videos: Vec<Video> = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        // Any video in the bucket with segments: they come back for it alone.
        let other = videos
            .iter()
            .find_map(|v| VideoId::parse(&v.video_id).filter(|_| !v.segments.is_empty()))
            .expect("a bucket has videos with segments");
        let segments = segments(&client, &other, &all).await.unwrap();
        assert!(!segments.is_empty(), "{other}");
        for s in &segments {
            assert!(s.start >= 0.0 && s.end > s.start, "{s:?}");
        }
    }
}
