//! What tuitube keeps between runs, in `tuitube.db` (SQLite) in the data
//! folder: your subscriptions, the videos feeds and searches turned up,
//! Watch later and what you watched here. It never leaves this computer.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::config;
use crate::ids::{ChannelId, VideoId};
use crate::video::{MAX_CHANNEL, MAX_DESCRIPTION, MAX_TITLE, Video, one_line};

pub struct Store {
    db: Connection,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub id: ChannelId,
    pub title: String,
    /// Its photo's URL, once looked up.
    pub avatar: Option<String>,
    /// When the photo was last looked up (Unix seconds), found or not.
    pub avatar_checked: Option<i64>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS channels (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    avatar TEXT,
    avatar_checked INTEGER,
    subscribed INTEGER NOT NULL DEFAULT 0,
    feed_checked INTEGER,
    videos_checked INTEGER,
    streams_checked INTEGER
);
CREATE TABLE IF NOT EXISTS videos (
    id TEXT PRIMARY KEY,
    channel_id TEXT,
    channel TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    published INTEGER,
    views INTEGER,
    duration INTEGER,
    short INTEGER NOT NULL DEFAULT 0,
    live INTEGER NOT NULL DEFAULT 0,
    thumbnail TEXT
);
CREATE INDEX IF NOT EXISTS videos_by_channel ON videos(channel_id, published DESC);
CREATE INDEX IF NOT EXISTS videos_by_date ON videos(published DESC);
CREATE TABLE IF NOT EXISTS watch_later (
    video_id TEXT PRIMARY KEY,
    added INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS history (
    video_id TEXT PRIMARY KEY,
    watched INTEGER NOT NULL,
    position REAL,
    length REAL
);
";

const VIDEO_COLUMNS: &str = "v.id, v.channel_id, v.channel, v.title, v.description, v.published, \
                             v.views, v.duration, v.short, v.live, v.thumbnail";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let shown = config::shown(path);
        let db = Connection::open(path).with_context(|| format!("cannot open {shown}"))?;
        // Deleted rows (a video taken off History) are overwritten, not
        // left readable in the file's free pages.
        db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA secure_delete = ON;")?;
        db.execute_batch(SCHEMA)
            .with_context(|| format!("cannot set up {shown}"))?;
        migrate(&db).with_context(|| format!("cannot update {shown}"))?;
        // The data folder is already yours alone; this is for a copy of the
        // database moved out of it. The journal files exist by now.
        Self::protect(path);
        Ok(Self { db })
    }

    /// A store that lives only in memory: the demo's, and tests'.
    pub fn in_memory() -> Self {
        let db = Connection::open_in_memory().expect("SQLite opens in memory");
        db.execute_batch(SCHEMA).expect("the schema is valid");
        Self { db }
    }

    /// The database and its journal files readable only by you, whatever
    /// the umask.
    fn protect(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for suffix in ["", "-wal", "-shm"] {
                let mut file = path.as_os_str().to_owned();
                file.push(suffix);
                let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
            }
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    // Subscriptions.

    pub fn subscribe(&self, id: &ChannelId, title: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO channels (id, title, subscribed) VALUES (?1, ?2, 1)
             ON CONFLICT(id) DO UPDATE SET subscribed = 1,
                 title = CASE WHEN excluded.title = '' THEN channels.title ELSE excluded.title END",
            params![id.as_str(), title],
        )?;
        Ok(())
    }

    /// Subscribes to every channel, in one transaction; how many weren't
    /// subscribed before.
    pub fn subscribe_all(&mut self, channels: &[(ChannelId, String)]) -> Result<usize> {
        let tx = self.db.transaction()?;
        let mut added = 0;
        {
            let mut before = tx.prepare("SELECT subscribed FROM channels WHERE id = ?1")?;
            let mut insert = tx.prepare(
                "INSERT INTO channels (id, title, subscribed) VALUES (?1, ?2, 1)
                 ON CONFLICT(id) DO UPDATE SET subscribed = 1,
                     title = CASE WHEN channels.title = '' THEN excluded.title ELSE channels.title END",
            )?;
            for (id, title) in channels {
                let was: Option<i64> = before
                    .query_row(params![id.as_str()], |row| row.get(0))
                    .optional()?;
                if was != Some(1) {
                    added += 1;
                }
                insert.execute(params![id.as_str(), title])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// The channel's name as its own feed gives it: what a Takeout file or
    /// anything else called it gives way.
    pub fn rename(&self, id: &ChannelId, title: &str) -> Result<()> {
        if !title.is_empty() {
            self.db.execute(
                "UPDATE channels SET title = ?2 WHERE id = ?1 AND title <> ?2",
                params![id.as_str(), title],
            )?;
        }
        Ok(())
    }

    pub fn unsubscribe(&self, id: &ChannelId) -> Result<()> {
        self.db.execute(
            "UPDATE channels SET subscribed = 0 WHERE id = ?1",
            params![id.as_str()],
        )?;
        Ok(())
    }

    pub fn is_subscribed(&self, id: &ChannelId) -> Result<bool> {
        let found: Option<i64> = self
            .db
            .query_row(
                "SELECT subscribed FROM channels WHERE id = ?1",
                params![id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found == Some(1))
    }

    /// Your subscriptions by name.
    pub fn subscriptions(&self) -> Result<Vec<Channel>> {
        let mut query = self.db.prepare(
            "SELECT id, title, avatar, avatar_checked FROM channels
             WHERE subscribed = 1 ORDER BY title COLLATE NOCASE",
        )?;
        let rows = query.query_map([], channel_row)?;
        Ok(rows.filter_map(|row| row.ok().flatten()).collect())
    }

    /// Any channel tuitube has seen, subscribed or not.
    pub fn channel(&self, id: &ChannelId) -> Result<Option<Channel>> {
        Ok(self
            .db
            .query_row(
                "SELECT id, title, avatar, avatar_checked FROM channels WHERE id = ?1",
                params![id.as_str()],
                channel_row,
            )
            .optional()?
            .flatten())
    }

    /// Remembers a channel's photo (or that it has none), and its name if
    /// tuitube didn't know it.
    pub fn set_avatar(
        &self,
        id: &ChannelId,
        title: &str,
        avatar: Option<&str>,
        now: i64,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO channels (id, title, avatar, avatar_checked) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET avatar = excluded.avatar,
                 avatar_checked = excluded.avatar_checked,
                 title = CASE WHEN channels.title = '' THEN excluded.title ELSE channels.title END",
            params![id.as_str(), title, avatar, now],
        )?;
        Ok(())
    }

    /// When yt-dlp last looked at the channel's Videos tab, or its Live tab
    /// if `streams`.
    pub fn tab_checked(&self, id: &ChannelId, streams: bool) -> Result<Option<i64>> {
        let column = if streams {
            "streams_checked"
        } else {
            "videos_checked"
        };
        Ok(self
            .db
            .query_row(
                &format!("SELECT {column} FROM channels WHERE id = ?1"),
                params![id.as_str()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn set_tab_checked(&self, id: &ChannelId, streams: bool, now: i64) -> Result<()> {
        let column = if streams {
            "streams_checked"
        } else {
            "videos_checked"
        };
        self.db.execute(
            &format!(
                "INSERT INTO channels (id, title, {column}) VALUES (?1, '', ?2)
                 ON CONFLICT(id) DO UPDATE SET {column} = excluded.{column}"
            ),
            params![id.as_str(), now],
        )?;
        Ok(())
    }

    pub fn feed_checked(&self, id: &ChannelId, now: i64) -> Result<()> {
        self.db.execute(
            "UPDATE channels SET feed_checked = ?2 WHERE id = ?1",
            params![id.as_str(), now],
        )?;
        Ok(())
    }

    /// When a subscription's feed was last fetched: the newest of them.
    pub fn feeds_updated(&self) -> Result<Option<i64>> {
        Ok(self.db.query_row(
            "SELECT MAX(feed_checked) FROM channels WHERE subscribed = 1",
            [],
            |row| row.get(0),
        )?)
    }

    /// Forgets videos published before `before` that aren't in Watch later
    /// or History, so the store doesn't grow without end.
    pub fn prune(&self, before: i64) -> Result<usize> {
        Ok(self.db.execute(
            "DELETE FROM videos WHERE published IS NOT NULL AND published < ?1
             AND id NOT IN (SELECT video_id FROM watch_later)
             AND id NOT IN (SELECT video_id FROM history)",
            params![before],
        )?)
    }

    /// The subscriptions whose feed wasn't fetched since `since`.
    pub fn feeds_due(&self, since: i64) -> Result<Vec<ChannelId>> {
        let mut query = self.db.prepare(
            "SELECT id FROM channels WHERE subscribed = 1
             AND (feed_checked IS NULL OR feed_checked < ?1)",
        )?;
        let rows = query.query_map(params![since], |row| row.get::<_, String>(0))?;
        Ok(rows
            .filter_map(|row| ChannelId::parse(&row.ok()?))
            .collect())
    }

    // Videos.

    /// Saves videos from a feed, a search or a channel's list. What one
    /// source doesn't know (a feed has no lengths, a search no dates) keeps
    /// what another said.
    pub fn save_videos(&mut self, videos: &[Video]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO videos (id, channel_id, channel, title, description, published,
                                     views, duration, short, live, thumbnail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                     channel_id = COALESCE(excluded.channel_id, videos.channel_id),
                     channel = CASE WHEN excluded.channel = '' THEN videos.channel ELSE excluded.channel END,
                     title = excluded.title,
                     description = CASE WHEN excluded.description = '' THEN videos.description
                                        ELSE excluded.description END,
                     published = COALESCE(excluded.published, videos.published),
                     views = COALESCE(excluded.views, videos.views),
                     duration = COALESCE(excluded.duration, videos.duration),
                     short = MAX(excluded.short, videos.short),
                     live = CASE WHEN excluded.live <> 0 OR excluded.duration IS NOT NULL
                                 THEN excluded.live ELSE videos.live END,
                     thumbnail = COALESCE(excluded.thumbnail, videos.thumbnail)",
            )?;
            for v in videos {
                insert.execute(params![
                    v.id.as_str(),
                    v.channel_id.as_ref().map(ChannelId::as_str),
                    v.channel,
                    v.title,
                    v.description,
                    v.published,
                    v.views.map(|n| n.min(i64::MAX as u64) as i64),
                    v.duration,
                    v.short,
                    live_code(v),
                    v.thumbnail,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The newest videos from your subscriptions: Shorts only, or none.
    pub fn feed(&self, shorts: bool, limit: usize) -> Result<Vec<Video>> {
        self.videos(
            &format!(
                "SELECT {VIDEO_COLUMNS} FROM videos v JOIN channels c ON c.id = v.channel_id
                 WHERE c.subscribed = 1 AND v.short = ?1 AND v.published IS NOT NULL
                 ORDER BY v.published DESC LIMIT ?2"
            ),
            params![shorts, limit as i64],
        )
    }

    /// A channel's newest videos tuitube knows about.
    pub fn channel_videos(&self, id: &ChannelId, limit: usize) -> Result<Vec<Video>> {
        self.videos(
            &format!(
                "SELECT {VIDEO_COLUMNS} FROM videos v WHERE v.channel_id = ?1
                 ORDER BY v.published IS NULL, v.published DESC LIMIT ?2"
            ),
            params![id.as_str(), limit as i64],
        )
    }

    pub fn watch_later(&self) -> Result<Vec<Video>> {
        self.videos(
            &format!(
                "SELECT {VIDEO_COLUMNS} FROM watch_later w JOIN videos v ON v.id = w.video_id
                 ORDER BY w.added DESC"
            ),
            [],
        )
    }

    /// Adds the video to Watch later, or takes it off if it's there.
    /// True if it's on the list now.
    pub fn toggle_watch_later(&self, id: &VideoId, now: i64) -> Result<bool> {
        let removed = self.db.execute(
            "DELETE FROM watch_later WHERE video_id = ?1",
            params![id.as_str()],
        )?;
        if removed > 0 {
            return Ok(false);
        }
        self.db.execute(
            "INSERT INTO watch_later (video_id, added) VALUES (?1, ?2)",
            params![id.as_str(), now],
        )?;
        Ok(true)
    }

    // History.

    pub fn history(&self, limit: usize) -> Result<Vec<Video>> {
        self.videos(
            &format!(
                "SELECT {VIDEO_COLUMNS} FROM history h JOIN videos v ON v.id = h.video_id
                 ORDER BY h.watched DESC LIMIT ?1"
            ),
            params![limit as i64],
        )
    }

    /// Remembers that you watched the video up to `position` seconds (of
    /// `length`), so it can carry on from there.
    pub fn record_watch(
        &self,
        id: &VideoId,
        now: i64,
        position: Option<f64>,
        length: Option<f64>,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO history (video_id, watched, position, length) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(video_id) DO UPDATE SET watched = excluded.watched,
                 position = COALESCE(excluded.position, history.position),
                 length = COALESCE(excluded.length, history.length)",
            params![id.as_str(), now, position, length],
        )?;
        Ok(())
    }

    /// Where you stopped watching, and the video's length, in seconds.
    pub fn progress(&self, id: &VideoId) -> Result<Option<(f64, f64)>> {
        Ok(self
            .db
            .query_row(
                "SELECT position, length FROM history WHERE video_id = ?1
                 AND position IS NOT NULL AND length IS NOT NULL",
                params![id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Where you stopped in every video you watched here.
    pub fn all_progress(&self) -> Result<std::collections::HashMap<VideoId, (f64, f64)>> {
        let mut query = self.db.prepare(
            "SELECT video_id, position, length FROM history
             WHERE position IS NOT NULL AND length IS NOT NULL AND length > 0
             ORDER BY watched DESC LIMIT 5000",
        )?;
        let rows = query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
        })?;
        Ok(rows
            .filter_map(|row| {
                let (id, at, length) = row.ok()?;
                Some((VideoId::parse(&id)?, (at, length)))
            })
            .collect())
    }

    pub fn forget_watch(&self, id: &VideoId) -> Result<()> {
        self.db.execute(
            "DELETE FROM history WHERE video_id = ?1",
            params![id.as_str()],
        )?;
        Ok(())
    }

    fn videos(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Video>> {
        let mut query = self.db.prepare(sql)?;
        let rows = query.query_map(params, video_row)?;
        Ok(rows.filter_map(|row| row.ok().flatten()).collect())
    }
}

/// Adds what later versions keep to a database an earlier one made.
fn migrate(db: &Connection) -> Result<()> {
    let mut query = db.prepare("SELECT name FROM pragma_table_info('channels')")?;
    let columns: Vec<String> = query
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for column in ["videos_checked", "streams_checked"] {
        if !columns.iter().any(|c| c == column) {
            db.execute_batch(&format!("ALTER TABLE channels ADD COLUMN {column} INTEGER"))?;
        }
    }
    Ok(())
}

/// The `live` column: 1 live now, 2 not started yet, 0 neither. A feed
/// knows neither, so its 0 never overwrites what yt-dlp said (`save_videos`).
fn live_code(v: &Video) -> i64 {
    if v.live {
        1
    } else if v.upcoming {
        2
    } else {
        0
    }
}

fn channel_row(row: &rusqlite::Row) -> rusqlite::Result<Option<Channel>> {
    let id: String = row.get(0)?;
    let Some(id) = ChannelId::parse(&id) else {
        return Ok(None);
    };
    Ok(Some(Channel {
        id,
        title: one_line(&row.get::<_, String>(1)?, MAX_CHANNEL),
        avatar: row.get(2)?,
        avatar_checked: row.get(3)?,
    }))
}

/// A row as a [`Video`]; `None` for one whose id isn't a video id, which
/// only an edited database could hold.
fn video_row(row: &rusqlite::Row) -> rusqlite::Result<Option<Video>> {
    let id: String = row.get(0)?;
    let Some(id) = VideoId::parse(&id) else {
        return Ok(None);
    };
    let channel_id: Option<String> = row.get(1)?;
    Ok(Some(Video {
        id,
        channel_id: channel_id.as_deref().and_then(ChannelId::parse),
        // Cleaned again: rows kept by an older tuitube, or edited by hand,
        // get today's rules.
        channel: one_line(&row.get::<_, String>(2)?, MAX_CHANNEL),
        title: one_line(&row.get::<_, String>(3)?, MAX_TITLE),
        description: one_line(&row.get::<_, String>(4)?, MAX_DESCRIPTION),
        published: row.get(5)?,
        views: row.get::<_, Option<i64>>(6)?.map(|n| n.max(0) as u64),
        duration: row.get(7)?,
        short: row.get(8)?,
        live: row.get::<_, i64>(9)? == 1,
        upcoming: row.get::<_, i64>(9)? == 2,
        thumbnail: row.get(10)?,
    }))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn video(id: &str, channel: &str, published: Option<i64>) -> Video {
        Video {
            id: VideoId::parse(id).unwrap(),
            title: format!("Video {id}"),
            channel_id: ChannelId::parse(channel),
            channel: "Channel".into(),
            description: String::new(),
            published,
            views: None,
            duration: None,
            short: false,
            live: false,
            upcoming: false,
            thumbnail: None,
        }
    }

    const CH: &str = "UC7EVSn5inapL20oPSwAwEUg";

    #[test]
    fn the_feed_lists_subscribed_channels_newest_first_without_shorts() {
        let mut store = Store::in_memory();
        let ch = ChannelId::parse(CH).unwrap();
        store.subscribe(&ch, "BekBrace").unwrap();
        let mut short = video("aaaaaaaaaa3", CH, Some(300));
        short.short = true;
        store
            .save_videos(&[
                video("aaaaaaaaaa1", CH, Some(100)),
                video("aaaaaaaaaa2", CH, Some(200)),
                short,
                video("bbbbbbbbbb1", "UCxxxxxxxxxxxxxxxxxxxxxx", Some(400)),
            ])
            .unwrap();
        let feed: Vec<_> = store.feed(false, 10).unwrap();
        let ids: Vec<_> = feed.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["aaaaaaaaaa2", "aaaaaaaaaa1"]);
        assert_eq!(store.feed(true, 10).unwrap().len(), 1);
        store.unsubscribe(&ch).unwrap();
        assert!(store.feed(false, 10).unwrap().is_empty());
    }

    #[test]
    fn a_later_source_keeps_what_an_earlier_one_knew() {
        let mut store = Store::in_memory();
        let mut from_search = video("aaaaaaaaaa1", CH, None);
        from_search.duration = Some(61);
        from_search.views = Some(5);
        store.save_videos(&[from_search]).unwrap();
        store
            .save_videos(&[video("aaaaaaaaaa1", CH, Some(100))])
            .unwrap();
        let v = &store
            .channel_videos(&ChannelId::parse(CH).unwrap(), 5)
            .unwrap()[0];
        assert_eq!(
            (v.duration, v.views, v.published),
            (Some(61), Some(5), Some(100))
        );
    }

    #[test]
    fn a_feed_doesnt_undo_what_yt_dlp_said_about_a_live_stream() {
        let mut store = Store::in_memory();
        let ch = ChannelId::parse(CH).unwrap();
        let mut live = video("aaaaaaaaaa1", CH, None);
        live.live = true;
        store.save_videos(&[live]).unwrap();
        store
            .save_videos(&[video("aaaaaaaaaa1", CH, Some(100))])
            .unwrap();
        assert!(store.channel_videos(&ch, 5).unwrap()[0].live, "still live");
        let mut ended = video("aaaaaaaaaa1", CH, None);
        ended.duration = Some(3600);
        store.save_videos(&[ended]).unwrap();
        let v = &store.channel_videos(&ch, 5).unwrap()[0];
        assert!(!v.live && v.duration == Some(3600), "it ended");
    }

    #[test]
    fn watch_later_toggles_and_history_remembers_where_you_stopped() {
        let mut store = Store::in_memory();
        let v = video("aaaaaaaaaa1", CH, Some(1));
        store.save_videos(std::slice::from_ref(&v)).unwrap();
        assert!(store.toggle_watch_later(&v.id, 10).unwrap());
        assert_eq!(store.watch_later().unwrap().len(), 1);
        assert!(!store.toggle_watch_later(&v.id, 11).unwrap());
        assert!(store.watch_later().unwrap().is_empty());

        store.record_watch(&v.id, 20, None, None).unwrap();
        assert_eq!(store.progress(&v.id).unwrap(), None);
        store
            .record_watch(&v.id, 21, Some(42.0), Some(600.0))
            .unwrap();
        store.record_watch(&v.id, 22, None, None).unwrap();
        assert_eq!(store.progress(&v.id).unwrap(), Some((42.0, 600.0)));
        assert_eq!(store.history(10).unwrap().len(), 1);
        store.forget_watch(&v.id).unwrap();
        assert!(store.history(10).unwrap().is_empty());
    }

    #[test]
    fn a_database_from_the_first_version_gets_the_new_columns() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE channels (id TEXT PRIMARY KEY, title TEXT NOT NULL, avatar TEXT,
             avatar_checked INTEGER, subscribed INTEGER NOT NULL DEFAULT 0, feed_checked INTEGER);",
        )
        .unwrap();
        migrate(&db).unwrap();
        migrate(&db).unwrap();
        let store = Store { db };
        let ch = ChannelId::parse(CH).unwrap();
        assert_eq!(store.tab_checked(&ch, false).unwrap(), None);
        store.set_tab_checked(&ch, true, 42).unwrap();
        assert_eq!(store.tab_checked(&ch, true).unwrap(), Some(42));
        assert_eq!(store.tab_checked(&ch, false).unwrap(), None);
    }

    #[test]
    fn the_database_is_only_yours() {
        let dir = std::env::temp_dir().join(format!("tuitube-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tuitube.db");
        let store = Store::open(&path).unwrap();
        store
            .subscribe(&ChannelId::parse(CH).unwrap(), "BekBrace")
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
