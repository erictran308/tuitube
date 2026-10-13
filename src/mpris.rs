//! Now Playing on Linux: tuitube answers on the session D-Bus as an MPRIS
//! player, `org.mpris.MediaPlayer2.tuitube`, which is how desktop shells,
//! their media keys and `playerctl` find players. mpv publishes nothing
//! there itself (macOS's Now Playing is mpv's own: `player::NOW_PLAYING`).
//!
//! What it says ([`Shown`]) is what tuitube shows: the title (DeArrow's, if
//! on), the channel, the length, where it is, the thumbnail tuitube wrote
//! (`Images::cover`, as a `file://` URL), the volume. What it's asked
//! ([`Ask`]) comes back as `AppEvent::Mpris` and runs what the keys run:
//! play and pause, ⏭ and ⏮ as `N` and `[`, seeking, the volume, stopping.
//! Opening a link (`OpenUri`), raising a window and quitting are refused.
//! Only programs of your own user reach the session bus. All of it goes
//! with `now_playing`: turned off, the bus name is given up.

/// tuitube answers on the session bus itself: Linux.
pub const AVAILABLE: bool = cfg!(target_os = "linux");

/// What Now Playing shows.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Shown {
    /// What plays; none when nothing does.
    pub track: Option<Track>,
    pub paused: bool,
    /// Where it is, in seconds.
    pub position: f64,
    /// How loud, from 0 to 1 (0 when muted).
    pub volume: f64,
    /// A position can be asked for: mpv can be told, and it isn't live.
    pub can_seek: bool,
}

/// The video playing, as Now Playing names it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Track {
    /// The playback's number, which is its track id: a request about an
    /// earlier playback is ignored.
    pub play: u64,
    pub title: String,
    pub channel: String,
    /// The video's page, built from its checked id.
    pub url: String,
    /// The thumbnail tuitube wrote, as a `file://` URL.
    pub art: Option<String>,
    /// Its length in seconds, once known.
    pub length: Option<f64>,
}

/// What the desktop asks of tuitube.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum Ask {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Seconds ahead (or back, below 0) of where it is.
    Seek(f64),
    /// A position in seconds, for playback `play`.
    SetPosition {
        play: u64,
        at: f64,
    },
    /// From 0 to 1.
    Volume(f64),
}

pub use imp::Mpris;

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result};
    use mpris_server::zbus::{self, fdo};
    use mpris_server::{
        LoopStatus, Metadata, PlaybackRate, PlaybackStatus, PlayerInterface, Property,
        RootInterface, Server, Signal, Time, TrackId, Volume,
    };
    use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

    use super::{Ask, Shown};
    use crate::app::AppEvent;

    /// The track ids: this, then the playback's number.
    const TRACK_PATH: &str = "/io/github/erictran308/tuitube/play/";

    /// How long connecting to the session bus may take.
    const CONNECT_WITHIN: Duration = Duration::from_secs(2);

    /// A position this far from where playing on would put it is a seek
    /// (tuitube's, mpv window's, a SponsorBlock skip): said as `Seeked`.
    const SEEK_SLACK: f64 = 1.5;

    /// tuitube on the session bus, for as long as this is kept.
    pub struct Mpris {
        shown: Arc<Mutex<Shown>>,
        /// To the task that sends the bus's signals, in order. Dropped,
        /// the task ends and the bus name is given up.
        out: UnboundedSender<Out>,
        last: Shown,
        at: Instant,
    }

    enum Out {
        Changed(Vec<Property>),
        Seeked(f64),
    }

    impl Mpris {
        /// Takes `org.mpris.MediaPlayer2.tuitube` on the session bus (with
        /// `.instance<pid>` after it if another tuitube has it). What it's
        /// asked goes to `tx`.
        pub async fn start(tx: UnboundedSender<AppEvent>) -> Result<Self> {
            let shown = Arc::new(Mutex::new(Shown::default()));
            let answers = || Answers {
                shown: shown.clone(),
                tx: tx.clone(),
            };
            let server = tokio::time::timeout(CONNECT_WITHIN, async {
                match Server::new("tuitube", answers()).await {
                    Ok(server) => Ok(server),
                    Err(_) => {
                        let instance = format!("tuitube.instance{}", std::process::id());
                        Server::new(&instance, answers()).await
                    }
                }
            })
            .await
            .context("the session bus didn't answer")?
            .context("not on the session bus")?;
            let (out, mut queue) = unbounded_channel();
            tokio::spawn(async move {
                while let Some(out) = queue.recv().await {
                    let _ = match out {
                        Out::Changed(properties) => server.properties_changed(properties).await,
                        Out::Seeked(at) => {
                            let position = time(at);
                            server.emit(Signal::Seeked { position }).await
                        }
                    };
                }
            });
            Ok(Self {
                shown,
                out,
                last: Shown::default(),
                at: Instant::now(),
            })
        }

        /// Says what plays now: the properties that changed, and a seek.
        pub fn update(&mut self, now: Shown) {
            if now == self.last {
                return;
            }
            *self.shown.lock().unwrap_or_else(PoisonError::into_inner) = now.clone();
            let before = std::mem::replace(&mut self.last, now);
            let changed = changes(&before, &self.last);
            if !changed.is_empty() {
                let _ = self.out.send(Out::Changed(changed));
            }
            if seeked(&before, &self.last, self.at.elapsed().as_secs_f64()) {
                let _ = self.out.send(Out::Seeked(self.last.position));
            }
            self.at = Instant::now();
        }
    }

    /// Seconds as the bus's microseconds.
    fn time(seconds: f64) -> Time {
        Time::from_micros((seconds * 1e6) as i64)
    }

    fn track_id(play: u64) -> TrackId {
        TrackId::try_from(format!("{TRACK_PATH}{play}")).unwrap_or(TrackId::NO_TRACK)
    }

    /// The playback a track id names, if it's one of tuitube's.
    fn play_of(track: &TrackId) -> Option<u64> {
        track.to_string().strip_prefix(TRACK_PATH)?.parse().ok()
    }

    fn metadata(shown: &Shown) -> Metadata {
        let Some(track) = &shown.track else {
            return Metadata::builder().trackid(TrackId::NO_TRACK).build();
        };
        let mut metadata = Metadata::builder()
            .trackid(track_id(track.play))
            .title(track.title.clone())
            .artist([track.channel.clone()])
            .url(track.url.clone());
        if let Some(length) = track.length.filter(|l| l.is_finite() && *l > 0.0) {
            metadata = metadata.length(time(length));
        }
        if let Some(art) = &track.art {
            metadata = metadata.art_url(art.clone());
        }
        metadata.build()
    }

    fn status(shown: &Shown) -> PlaybackStatus {
        match (&shown.track, shown.paused) {
            (None, _) => PlaybackStatus::Stopped,
            (Some(_), true) => PlaybackStatus::Paused,
            (Some(_), false) => PlaybackStatus::Playing,
        }
    }

    /// The properties that differ, as `PropertiesChanged` says them.
    fn changes(before: &Shown, now: &Shown) -> Vec<Property> {
        let mut changed = Vec::new();
        if before.track != now.track {
            changed.push(Property::Metadata(metadata(now)));
        }
        if before.track.is_some() != now.track.is_some() {
            let some = now.track.is_some();
            changed.extend([
                Property::CanGoNext(some),
                Property::CanGoPrevious(some),
                Property::CanPlay(some),
                Property::CanPause(some),
            ]);
        }
        if status(before) != status(now) {
            changed.push(Property::PlaybackStatus(status(now)));
        }
        if before.volume != now.volume {
            changed.push(Property::Volume(now.volume));
        }
        if before.can_seek != now.can_seek {
            changed.push(Property::CanSeek(now.can_seek));
        }
        changed
    }

    /// The same video moved other than by playing on, `elapsed` seconds
    /// after it was last said.
    fn seeked(before: &Shown, now: &Shown, elapsed: f64) -> bool {
        let same = matches!((&before.track, &now.track), (Some(a), Some(b)) if a.play == b.play);
        let played = if before.paused { 0.0 } else { elapsed };
        same && (now.position - (before.position + played)).abs() > SEEK_SLACK
    }

    /// The bus's side: reads what's shown, passes what's asked on.
    struct Answers {
        shown: Arc<Mutex<Shown>>,
        tx: UnboundedSender<AppEvent>,
    }

    impl Answers {
        fn shown(&self) -> Shown {
            self.shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn ask(&self, ask: Ask) -> fdo::Result<()> {
            self.tx
                .send(AppEvent::Mpris(ask))
                .map_err(|_| fdo::Error::Failed("tuitube is quitting".into()))
        }
    }

    impl RootInterface for Answers {
        async fn raise(&self) -> fdo::Result<()> {
            Err(fdo::Error::NotSupported("tuitube has no window".into()))
        }

        async fn quit(&self) -> fdo::Result<()> {
            Err(fdo::Error::NotSupported("quit tuitube with q".into()))
        }

        async fn can_quit(&self) -> fdo::Result<bool> {
            Ok(false)
        }

        async fn fullscreen(&self) -> fdo::Result<bool> {
            Ok(false)
        }

        async fn set_fullscreen(&self, _fullscreen: bool) -> zbus::Result<()> {
            Err(zbus::Error::Unsupported)
        }

        async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
            Ok(false)
        }

        async fn can_raise(&self) -> fdo::Result<bool> {
            Ok(false)
        }

        async fn has_track_list(&self) -> fdo::Result<bool> {
            Ok(false)
        }

        async fn identity(&self) -> fdo::Result<String> {
            Ok("tuitube".into())
        }

        async fn desktop_entry(&self) -> fdo::Result<String> {
            Ok(String::new())
        }

        async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
            Ok(Vec::new())
        }
    }

    impl PlayerInterface for Answers {
        async fn next(&self) -> fdo::Result<()> {
            self.ask(Ask::Next)
        }

        async fn previous(&self) -> fdo::Result<()> {
            self.ask(Ask::Previous)
        }

        async fn pause(&self) -> fdo::Result<()> {
            self.ask(Ask::Pause)
        }

        async fn play_pause(&self) -> fdo::Result<()> {
            self.ask(Ask::PlayPause)
        }

        async fn stop(&self) -> fdo::Result<()> {
            self.ask(Ask::Stop)
        }

        async fn play(&self) -> fdo::Result<()> {
            self.ask(Ask::Play)
        }

        async fn seek(&self, offset: Time) -> fdo::Result<()> {
            self.ask(Ask::Seek(offset.as_micros() as f64 / 1e6))
        }

        async fn set_position(&self, track: TrackId, position: Time) -> fdo::Result<()> {
            // Another track's position is ignored, as the spec says.
            match play_of(&track) {
                Some(play) => self.ask(Ask::SetPosition {
                    play,
                    at: position.as_micros() as f64 / 1e6,
                }),
                None => Ok(()),
            }
        }

        async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
            Err(fdo::Error::NotSupported("tuitube opens no links".into()))
        }

        async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
            Ok(status(&self.shown()))
        }

        async fn loop_status(&self) -> fdo::Result<LoopStatus> {
            Ok(LoopStatus::None)
        }

        async fn set_loop_status(&self, _loop_status: LoopStatus) -> zbus::Result<()> {
            Err(zbus::Error::Unsupported)
        }

        async fn rate(&self) -> fdo::Result<PlaybackRate> {
            Ok(1.0)
        }

        async fn set_rate(&self, _rate: PlaybackRate) -> zbus::Result<()> {
            Err(zbus::Error::Unsupported)
        }

        async fn shuffle(&self) -> fdo::Result<bool> {
            Ok(false)
        }

        async fn set_shuffle(&self, _shuffle: bool) -> zbus::Result<()> {
            Err(zbus::Error::Unsupported)
        }

        async fn metadata(&self) -> fdo::Result<Metadata> {
            Ok(metadata(&self.shown()))
        }

        async fn volume(&self) -> fdo::Result<Volume> {
            Ok(self.shown().volume)
        }

        async fn set_volume(&self, volume: Volume) -> zbus::Result<()> {
            if volume.is_finite() {
                let _ = self.ask(Ask::Volume(volume.clamp(0.0, 1.0)));
            }
            Ok(())
        }

        async fn position(&self) -> fdo::Result<Time> {
            let shown = self.shown();
            Ok(shown.track.map_or(Time::ZERO, |_| time(shown.position)))
        }

        async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
            Ok(1.0)
        }

        async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
            Ok(1.0)
        }

        async fn can_go_next(&self) -> fdo::Result<bool> {
            Ok(self.shown().track.is_some())
        }

        async fn can_go_previous(&self) -> fdo::Result<bool> {
            Ok(self.shown().track.is_some())
        }

        async fn can_play(&self) -> fdo::Result<bool> {
            Ok(self.shown().track.is_some())
        }

        async fn can_pause(&self) -> fdo::Result<bool> {
            Ok(self.shown().track.is_some())
        }

        async fn can_seek(&self) -> fdo::Result<bool> {
            Ok(self.shown().can_seek)
        }

        async fn can_control(&self) -> fdo::Result<bool> {
            Ok(true)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::Track;
        use super::*;

        fn playing(play: u64, position: f64) -> Shown {
            Shown {
                track: Some(Track {
                    play,
                    title: "A title".into(),
                    channel: "A channel".into(),
                    url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
                    art: Some("file:///home/me/p-dQw4w9WgXcQ.jpg".into()),
                    length: Some(212.5),
                }),
                paused: false,
                position,
                volume: 0.8,
                can_seek: true,
            }
        }

        #[test]
        fn what_plays_is_said_with_tuitubes_own_track_ids() {
            let said = metadata(&playing(7, 3.0));
            let id = said.trackid().unwrap();
            assert_eq!(id.to_string(), "/io/github/erictran308/tuitube/play/7");
            assert_eq!(play_of(&id), Some(7));
            assert_eq!(said.title(), Some("A title"));
            assert_eq!(said.artist(), Some(vec!["A channel".to_string()]));
            assert_eq!(said.length(), Some(Time::from_micros(212_500_000)));
            assert_eq!(
                said.art_url().as_deref(),
                Some("file:///home/me/p-dQw4w9WgXcQ.jpg")
            );
            assert_eq!(play_of(&TrackId::NO_TRACK), None, "not tuitube's");
            let nothing = metadata(&Shown::default());
            assert_eq!(nothing.trackid(), Some(TrackId::NO_TRACK));
            assert_eq!(nothing.title(), None);
        }

        #[test]
        fn only_what_changed_is_said() {
            let before = playing(7, 3.0);
            assert!(changes(&before, &before.clone()).is_empty());
            let mut paused = before.clone();
            paused.paused = true;
            assert!(matches!(
                changes(&before, &paused)[..],
                [Property::PlaybackStatus(PlaybackStatus::Paused)]
            ));
            let stopped = changes(&before, &Shown::default());
            assert!(matches!(stopped[0], Property::Metadata(_)));
            assert!(
                stopped
                    .iter()
                    .any(|p| matches!(p, Property::CanGoNext(false)))
            );
            assert!(
                stopped
                    .iter()
                    .any(|p| matches!(p, Property::PlaybackStatus(PlaybackStatus::Stopped)))
            );
        }

        #[test]
        fn a_jump_is_a_seek_playing_on_isnt() {
            let before = playing(7, 10.0);
            assert!(!seeked(&before, &playing(7, 11.0), 1.0), "played on");
            assert!(seeked(&before, &playing(7, 60.0), 1.0), "went ahead");
            assert!(seeked(&before, &playing(7, 0.0), 1.0), "went back");
            assert!(!seeked(&before, &playing(8, 60.0), 1.0), "another video");
            let mut paused = before.clone();
            paused.paused = true;
            assert!(
                seeked(&paused, &playing(7, 15.0), 5.0),
                "paused doesn't move"
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "talks to the session D-Bus"]
        async fn live_mpris_says_what_plays_and_passes_on_what_its_asked() {
            let (tx, mut rx) = unbounded_channel();
            let mut mpris = Mpris::start(tx).await.expect("a session bus");
            mpris.update(playing(7, 3.0));
            let bus = zbus::Connection::session().await.unwrap();
            let names = fdo::DBusProxy::new(&bus)
                .await
                .unwrap()
                .list_names()
                .await
                .unwrap();
            let name = names
                .iter()
                .find(|n| n.starts_with("org.mpris.MediaPlayer2.tuitube"))
                .expect("on the bus")
                .to_string();
            let player = zbus::Proxy::new(
                &bus,
                name.as_str(),
                "/org/mpris/MediaPlayer2",
                "org.mpris.MediaPlayer2.Player",
            )
            .await
            .unwrap();
            let status: String = player.get_property("PlaybackStatus").await.unwrap();
            assert_eq!(status, "Playing");
            let shown: std::collections::HashMap<String, zbus::zvariant::OwnedValue> =
                player.get_property("Metadata").await.unwrap();
            let title: String = shown["xesam:title"].clone().try_into().unwrap();
            assert_eq!(title, "A title");

            let _: () = player.call("Next", &()).await.unwrap();
            let id = zbus::zvariant::ObjectPath::try_from("/io/github/erictran308/tuitube/play/7")
                .unwrap();
            let _: () = player
                .call("SetPosition", &(id, 42_000_000_i64))
                .await
                .unwrap();
            let refused = player
                .call::<_, _, ()>("OpenUri", &("https://example.com",))
                .await;
            assert!(refused.is_err(), "opens no links");
            let mut asked = Vec::new();
            while asked.len() < 2 {
                match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                    Ok(Some(AppEvent::Mpris(ask))) => asked.push(ask),
                    _ => break,
                }
            }
            assert_eq!(asked, [Ask::Next, Ask::SetPosition { play: 7, at: 42.0 }]);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use anyhow::{Result, bail};
    use tokio::sync::mpsc::UnboundedSender;

    use super::Shown;
    use crate::app::AppEvent;

    /// Nothing: only Linux has tuitube on its session bus.
    pub enum Mpris {}

    impl Mpris {
        pub async fn start(_tx: UnboundedSender<AppEvent>) -> Result<Self> {
            bail!("tuitube answers on the session bus only on Linux")
        }

        pub fn update(&mut self, _now: Shown) {
            match *self {}
        }
    }
}
