//! What tuitube keeps between runs, in `tuitube.db` (SQLite) in the data
//! folder: your subscriptions, the videos feeds and searches turned up,
//! Watch later, the Jukebox and what you watched here. It never leaves this
//! computer.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::config;
use crate::ids::{ChannelId, VideoId};
use crate::video::{MAX_CHANNEL, MAX_DESCRIPTION, MAX_TITLE, Video, one_line};

pub struct Store {
    db: Connection,
}

/// One of your playlists, as imported.
#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub count: usize,
}

/// One of your groups of subscriptions, with how many of your subscriptions
/// are in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub count: usize,
}

/// The longest name a group keeps, in characters.
pub const MAX_GROUP_NAME: usize = 40;

/// Videos known only by their ids.
fn placeholders(videos: &[(VideoId, Option<i64>)]) -> Vec<Video> {
    videos
        .iter()
        .map(|(id, _)| Video::placeholder(id.clone(), false))
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub id: ChannelId,
    pub title: String,
    /// Its photo's URL, once looked up.
    pub avatar: Option<String>,
    /// When the photo was last looked up (Unix seconds), found or not.
    pub avatar_checked: Option<i64>,
    /// YouTube says the channel is gone (removed, or deleted): its feed
    /// isn't fetched any more.
    pub gone: bool,
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
    streams_checked INTEGER,
    gone INTEGER
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
CREATE TABLE IF NOT EXISTS jukebox (
    video_id TEXT PRIMARY KEY,
    place INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS history (
    video_id TEXT PRIMARY KEY,
    watched INTEGER NOT NULL,
    position REAL,
    length REAL
);
CREATE TABLE IF NOT EXISTS playlists (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS playlist_videos (
    playlist INTEGER NOT NULL,
    video_id TEXT NOT NULL,
    place INTEGER NOT NULL,
    PRIMARY KEY (playlist, video_id)
);
CREATE TABLE IF NOT EXISTS groups (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE IF NOT EXISTS group_channels (
    group_id INTEGER NOT NULL,
    channel_id TEXT NOT NULL,
    PRIMARY KEY (group_id, channel_id)
);
";

const VIDEO_COLUMNS: &str = "v.id, v.channel_id, v.channel, v.title, v.description, v.published, \
                             v.views, v.duration, v.short, v.live, v.thumbnail";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let shown = config::shown(path);
        let db = Connection::open(path).with_context(|| format!("cannot open {shown}"))?;
        // Deleted rows (a video taken off History) are overwritten, not
        // left readable in the file's free pages. Under WAL that holds only
        // once the zeroed pages are checkpointed: removals do it at once
        // (`Store::scrub`).
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

    /// Subscribing to a channel you weren't subscribed to takes back YouTube
    /// saying it's gone ([`Store::set_gone`]): it may be back.
    pub fn subscribe(&self, id: &ChannelId, title: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO channels (id, title, subscribed) VALUES (?1, ?2, 1)
             ON CONFLICT(id) DO UPDATE SET subscribed = 1,
                 gone = CASE WHEN channels.subscribed = 1 THEN channels.gone END,
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
                     gone = CASE WHEN channels.subscribed = 1 THEN channels.gone END,
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

    /// Remembers that YouTube says the channel is gone, so its feed isn't
    /// fetched again.
    pub fn set_gone(&self, id: &ChannelId, now: i64) -> Result<()> {
        self.db.execute(
            "UPDATE channels SET gone = ?2 WHERE id = ?1",
            params![id.as_str(), now],
        )?;
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
            "SELECT id, title, avatar, avatar_checked, gone FROM channels
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
                "SELECT id, title, avatar, avatar_checked, gone FROM channels WHERE id = ?1",
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

    /// Forgets videos published before `before` that aren't in Watch later,
    /// the Jukebox or History, so the store doesn't grow without end.
    pub fn prune(&self, before: i64) -> Result<usize> {
        Ok(self.db.execute(
            "DELETE FROM videos WHERE published IS NOT NULL AND published < ?1
             AND id NOT IN (SELECT video_id FROM watch_later)
             AND id NOT IN (SELECT video_id FROM jukebox)
             AND id NOT IN (SELECT video_id FROM history)
             AND id NOT IN (SELECT video_id FROM playlist_videos)",
            params![before],
        )?)
    }

    /// The subscriptions whose feed wasn't fetched since `since`.
    pub fn feeds_due(&self, since: i64) -> Result<Vec<ChannelId>> {
        let mut query = self.db.prepare(
            "SELECT id FROM channels WHERE subscribed = 1 AND gone IS NULL
             AND (feed_checked IS NULL OR feed_checked < ?1)",
        )?;
        let rows = query.query_map(params![since], |row| row.get::<_, String>(0))?;
        Ok(rows
            .filter_map(|row| ChannelId::parse(&row.ok()?))
            .collect())
    }

    /// The subscriptions whose feed is fetched (not gone), the one with the
    /// newest video first: the likeliest to have uploaded since.
    pub fn most_active(&self) -> Result<Vec<ChannelId>> {
        let mut query = self.db.prepare(
            "SELECT c.id FROM channels c LEFT JOIN videos v ON v.channel_id = c.id
             WHERE c.subscribed = 1 AND c.gone IS NULL
             GROUP BY c.id ORDER BY MAX(v.published) DESC, c.id",
        )?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows
            .filter_map(|row| ChannelId::parse(&row.ok()?))
            .collect())
    }

    // Videos.

    /// Saves videos from a feed, a search or a channel's list. What one
    /// source doesn't know (a feed has no lengths, a search no dates) keeps
    /// what another said.
    pub fn save_videos(&mut self, videos: &[Video]) -> Result<()> {
        self.save(videos, "COALESCE(excluded.published, videos.published)")
    }

    /// Saves videos whose dates are rough ("3 days ago", worked out): a
    /// date already known, from a feed, stays.
    pub fn save_roughly_dated(&mut self, videos: &[Video]) -> Result<()> {
        self.save(videos, "COALESCE(videos.published, excluded.published)")
    }

    fn save(&mut self, videos: &[Video], published: &str) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut insert = tx.prepare(&format!(
                "INSERT INTO videos (id, channel_id, channel, title, description, published,
                                     views, duration, short, live, thumbnail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                     channel_id = COALESCE(excluded.channel_id, videos.channel_id),
                     channel = CASE WHEN excluded.channel = '' THEN videos.channel ELSE excluded.channel END,
                     title = excluded.title,
                     description = CASE WHEN length(excluded.description) < length(videos.description)
                                        THEN videos.description ELSE excluded.description END,
                     published = {published},
                     views = COALESCE(excluded.views, videos.views),
                     duration = COALESCE(excluded.duration, videos.duration),
                     short = MAX(excluded.short, videos.short),
                     live = CASE WHEN excluded.live <> 0 OR excluded.duration IS NOT NULL
                                 THEN excluded.live ELSE videos.live END,
                     thumbnail = COALESCE(excluded.thumbnail, videos.thumbnail)",
            ))?;
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

    /// Saves videos from a list that knows little of them (a Takeout file):
    /// a video already known keeps all it has, and one known only by its id
    /// gets what this says.
    pub fn save_if_unknown(&mut self, videos: &[Video]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO videos (id, channel_id, channel, title, description, short)
                 VALUES (?1, ?2, ?3, ?4, '', ?5)
                 ON CONFLICT(id) DO UPDATE SET
                     channel_id = COALESCE(videos.channel_id, excluded.channel_id),
                     channel = CASE WHEN videos.channel = '' THEN excluded.channel ELSE videos.channel END,
                     title = CASE WHEN videos.title = 'youtu.be/' || videos.id THEN excluded.title
                                  ELSE videos.title END,
                     short = MAX(excluded.short, videos.short)",
            )?;
            for v in videos {
                insert.execute(params![
                    v.id.as_str(),
                    v.channel_id.as_ref().map(ChannelId::as_str),
                    v.channel,
                    v.title,
                    v.short,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Adds videos (and when each was added) to Watch later; how many
    /// weren't in it.
    pub fn import_watch_later(&mut self, videos: &[(VideoId, Option<i64>)]) -> Result<usize> {
        self.save_if_unknown(&placeholders(videos))?;
        let tx = self.db.transaction()?;
        let mut added = 0;
        {
            let mut insert = tx.prepare(
                "INSERT INTO watch_later (video_id, added) VALUES (?1, ?2)
                 ON CONFLICT(video_id) DO NOTHING",
            )?;
            for (id, at) in videos {
                added += insert.execute(params![id.as_str(), at.unwrap_or(0)])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Adds videos to the playlist called `name` (made if there's none),
    /// after the ones it has; how many weren't in it.
    pub fn import_playlist(
        &mut self,
        name: &str,
        videos: &[(VideoId, Option<i64>)],
    ) -> Result<usize> {
        self.save_if_unknown(&placeholders(videos))?;
        let tx = self.db.transaction()?;
        let mut added = 0;
        {
            tx.execute(
                "INSERT INTO playlists (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
                params![name],
            )?;
            let playlist: i64 = tx.query_row(
                "SELECT id FROM playlists WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )?;
            let mut insert = tx.prepare(
                "INSERT INTO playlist_videos (playlist, video_id, place)
                 VALUES (?1, ?2, (SELECT COALESCE(MAX(place), 0) + 1 FROM playlist_videos
                                  WHERE playlist = ?1))
                 ON CONFLICT(playlist, video_id) DO NOTHING",
            )?;
            for (id, _) in videos {
                added += insert.execute(params![playlist, id.as_str()])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Your playlists (imported), by name, with how many videos each has.
    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut query = self.db.prepare(
            "SELECT p.id, p.name, COUNT(v.video_id) FROM playlists p
             LEFT JOIN playlist_videos v ON v.playlist = p.id
             GROUP BY p.id ORDER BY p.name COLLATE NOCASE",
        )?;
        let rows = query.query_map([], |row| {
            Ok(Playlist {
                id: row.get(0)?,
                name: one_line(&row.get::<_, String>(1)?, MAX_TITLE),
                count: row.get::<_, i64>(2)?.max(0) as usize,
            })
        })?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    pub fn playlist_videos(&self, playlist: i64) -> Result<Vec<Video>> {
        self.videos(
            &format!(
                "SELECT {VIDEO_COLUMNS} FROM playlist_videos p JOIN videos v ON v.id = p.video_id
                 WHERE p.playlist = ?1 ORDER BY p.place"
            ),
            params![playlist],
        )
    }

    /// Takes the video off the playlist; whether it was on it.
    pub fn remove_from_playlist(&self, playlist: i64, id: &VideoId) -> Result<bool> {
        let removed = self.db.execute(
            "DELETE FROM playlist_videos WHERE playlist = ?1 AND video_id = ?2",
            params![playlist, id.as_str()],
        )?;
        if removed > 0 {
            self.scrub()?;
        }
        Ok(removed > 0)
    }

    /// Deletes the playlist (not its videos, which may be elsewhere).
    pub fn delete_playlist(&mut self, playlist: i64) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM playlist_videos WHERE playlist = ?1",
            params![playlist],
        )?;
        tx.execute("DELETE FROM playlists WHERE id = ?1", params![playlist])?;
        tx.commit()?;
        self.scrub()
    }

    /// Your groups of subscriptions, by name, with how many of your
    /// subscriptions each has. A channel you no longer subscribe to stays in
    /// its groups, uncounted, for if you subscribe again.
    pub fn groups(&self) -> Result<Vec<Group>> {
        let mut query = self.db.prepare(
            "SELECT g.id, g.name, COUNT(c.id) FROM groups g
             LEFT JOIN group_channels gc ON gc.group_id = g.id
             LEFT JOIN channels c ON c.id = gc.channel_id AND c.subscribed = 1
             GROUP BY g.id ORDER BY g.name COLLATE NOCASE",
        )?;
        let rows = query.query_map([], |row| {
            Ok(Group {
                id: row.get(0)?,
                name: one_line(&row.get::<_, String>(1)?, MAX_GROUP_NAME),
                count: row.get::<_, i64>(2)?.max(0) as usize,
            })
        })?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// The group called `name`, in any case, made if there's none: its id.
    pub fn create_group(&self, name: &str) -> Result<i64> {
        let name = one_line(name, MAX_GROUP_NAME);
        self.db.execute(
            "INSERT INTO groups (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
            params![name],
        )?;
        Ok(self.db.query_row(
            "SELECT id FROM groups WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?)
    }

    /// Names the group `name`; false if another group has that name.
    pub fn rename_group(&self, group: i64, name: &str) -> Result<bool> {
        let name = one_line(name, MAX_GROUP_NAME);
        let taken: Option<i64> = self
            .db
            .query_row(
                "SELECT id FROM groups WHERE name = ?1 AND id != ?2",
                params![name, group],
                |row| row.get(0),
            )
            .optional()?;
        if taken.is_some() {
            return Ok(false);
        }
        self.db.execute(
            "UPDATE groups SET name = ?1 WHERE id = ?2",
            params![name, group],
        )?;
        Ok(true)
    }

    /// Deletes the group (not its channels, which stay your subscriptions).
    pub fn delete_group(&mut self, group: i64) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM group_channels WHERE group_id = ?1",
            params![group],
        )?;
        tx.execute("DELETE FROM groups WHERE id = ?1", params![group])?;
        tx.commit()?;
        self.scrub()
    }

    /// Puts the channel in the group, or takes it out.
    pub fn set_in_group(&self, group: i64, channel: &ChannelId, inside: bool) -> Result<()> {
        if inside {
            self.db.execute(
                "INSERT INTO group_channels (group_id, channel_id) VALUES (?1, ?2)
                 ON CONFLICT DO NOTHING",
                params![group, channel.as_str()],
            )?;
        } else {
            self.db.execute(
                "DELETE FROM group_channels WHERE group_id = ?1 AND channel_id = ?2",
                params![group, channel.as_str()],
            )?;
        }
        Ok(())
    }

    /// The groups the channel is in.
    pub fn groups_of(&self, channel: &ChannelId) -> Result<Vec<i64>> {
        let mut query = self
            .db
            .prepare("SELECT group_id FROM group_channels WHERE channel_id = ?1")?;
        let rows = query.query_map(params![channel.as_str()], |row| row.get(0))?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// The newest videos of the group's channels you subscribe to, as Home
    /// lists them: no Shorts.
    pub fn group_feed(&self, group: i64, limit: usize) -> Result<Vec<Video>> {
        self.videos(
            &format!(
                "SELECT {VIDEO_COLUMNS} FROM videos v
                 JOIN channels c ON c.id = v.channel_id
                 JOIN group_channels g ON g.channel_id = c.id
                 WHERE g.group_id = ?1 AND c.subscribed = 1 AND v.short = 0
                   AND v.published IS NOT NULL
                 ORDER BY v.published DESC LIMIT ?2"
            ),
            params![group, limit as i64],
        )
    }

    /// Adds watch history: each video watched at a time. A video in History
    /// already keeps where you stopped, and the later of the two times. How
    /// many weren't in it.
    pub fn import_history(&mut self, watched: &[(Video, i64)]) -> Result<usize> {
        let videos: Vec<Video> = watched.iter().map(|(v, _)| v.clone()).collect();
        self.save_if_unknown(&videos)?;
        let tx = self.db.transaction()?;
        let mut added = 0;
        {
            let mut new = tx.prepare(
                "INSERT INTO history (video_id, watched) VALUES (?1, ?2)
                 ON CONFLICT(video_id) DO NOTHING",
            )?;
            let mut later =
                tx.prepare("UPDATE history SET watched = MAX(watched, ?2) WHERE video_id = ?1")?;
            for (video, at) in watched {
                let n = new.execute(params![video.id.as_str(), at])?;
                if n == 0 {
                    later.execute(params![video.id.as_str(), at])?;
                }
                added += n;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// The video, if tuitube knows it.
    pub fn video(&self, id: &VideoId) -> Result<Option<Video>> {
        Ok(self
            .videos(
                &format!("SELECT {VIDEO_COLUMNS} FROM videos v WHERE v.id = ?1"),
                params![id.as_str()],
            )?
            .pop())
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

    /// Adds the video to Watch later; whether it wasn't on it already.
    pub fn add_to_watch_later(&self, id: &VideoId, now: i64) -> Result<bool> {
        let added = self.db.execute(
            "INSERT OR IGNORE INTO watch_later (video_id, added) VALUES (?1, ?2)",
            params![id.as_str(), now],
        )?;
        Ok(added > 0)
    }

    /// Takes the video off Watch later; whether it was on it.
    pub fn remove_from_watch_later(&self, id: &VideoId) -> Result<bool> {
        let removed = self.db.execute(
            "DELETE FROM watch_later WHERE video_id = ?1",
            params![id.as_str()],
        )?;
        if removed > 0 {
            self.scrub()?;
        }
        Ok(removed > 0)
    }

    // The Jukebox: videos to play, in order.

    pub fn jukebox(&self) -> Result<Vec<Video>> {
        self.videos(
            &format!(
                "SELECT {VIDEO_COLUMNS} FROM jukebox j JOIN videos v ON v.id = j.video_id
                 ORDER BY j.place"
            ),
            [],
        )
    }

    /// Puts the video at the end of the Jukebox. False if it was in
    /// already, and stays where it is.
    pub fn add_to_jukebox(&self, id: &VideoId) -> Result<bool> {
        let added = self.db.execute(
            "INSERT INTO jukebox (video_id, place)
             VALUES (?1, (SELECT COALESCE(MAX(place), 0) + 1 FROM jukebox))
             ON CONFLICT(video_id) DO NOTHING",
            params![id.as_str()],
        )?;
        Ok(added > 0)
    }

    /// Takes the video off the Jukebox; whether it was on it.
    pub fn remove_from_jukebox(&self, id: &VideoId) -> Result<bool> {
        let removed = self.db.execute(
            "DELETE FROM jukebox WHERE video_id = ?1",
            params![id.as_str()],
        )?;
        if removed > 0 {
            self.scrub()?;
        }
        Ok(removed > 0)
    }

    /// Puts the Jukebox's videos in this order. One not in it stays out.
    pub fn order_jukebox(&mut self, ids: &[VideoId]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut set = tx.prepare("UPDATE jukebox SET place = ?2 WHERE video_id = ?1")?;
            for (place, id) in ids.iter().enumerate() {
                set.execute(params![id.as_str(), place as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Empties the Jukebox; how many videos it had.
    pub fn clear_jukebox(&self) -> Result<usize> {
        let removed = self.db.execute("DELETE FROM jukebox", [])?;
        self.scrub()?;
        Ok(removed)
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

    /// Saves where you are in a video still in History, and says whether it
    /// is: one taken off History while it plays stays off.
    pub fn update_watch(
        &self,
        id: &VideoId,
        now: i64,
        position: f64,
        length: Option<f64>,
    ) -> Result<bool> {
        let updated = self.db.execute(
            "UPDATE history SET watched = ?2, position = ?3, length = COALESCE(?4, length)
             WHERE video_id = ?1",
            params![id.as_str(), now, position, length],
        )?;
        Ok(updated > 0)
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
        self.scrub()
    }

    /// secure_delete zeroes a deleted row only in the new copy of its page:
    /// under WAL the old copies stay in tuitube.db-wal, and tuitube.db keeps
    /// the old page until a checkpoint. This writes the zeroed pages back and
    /// empties the WAL now, so a removed entry isn't left readable even if
    /// tuitube then ends without closing the database.
    fn scrub(&self) -> Result<()> {
        self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
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
    for column in ["videos_checked", "streams_checked", "gone"] {
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
        gone: row.get::<_, Option<i64>>(4)?.is_some(),
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
        description: crate::video::lines(&row.get::<_, String>(4)?, MAX_DESCRIPTION),
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
    fn subscribing_again_takes_back_that_a_channel_is_gone() {
        let mut store = Store::in_memory();
        let ch = ChannelId::parse(CH).unwrap();
        let due = |store: &Store| !store.feeds_due(i64::MAX).unwrap().is_empty();
        store.subscribe(&ch, "BekBrace").unwrap();
        store.set_gone(&ch, 1).unwrap();
        assert!(!due(&store));
        // Subscribed while still subscribed (a Takeout imported again): kept.
        store.subscribe(&ch, "BekBrace").unwrap();
        store
            .subscribe_all(&[(ch.clone(), "BekBrace".into())])
            .unwrap();
        assert!(!due(&store));
        assert!(store.subscriptions().unwrap()[0].gone);

        store.unsubscribe(&ch).unwrap();
        store.subscribe(&ch, "BekBrace").unwrap();
        assert!(due(&store), "subscribed again");
        store.set_gone(&ch, 2).unwrap();
        store.unsubscribe(&ch).unwrap();
        store
            .subscribe_all(&[(ch.clone(), "BekBrace".into())])
            .unwrap();
        assert!(due(&store), "imported again");
        assert!(!store.subscriptions().unwrap()[0].gone);
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
    fn a_rough_date_fills_in_but_never_replaces_a_feeds() {
        let mut store = Store::in_memory();
        let ch = ChannelId::parse(CH).unwrap();
        store
            .save_videos(&[video("aaaaaaaaaa1", CH, Some(100))])
            .unwrap();
        store
            .save_roughly_dated(&[
                video("aaaaaaaaaa1", CH, Some(90)),
                video("aaaaaaaaaa2", CH, Some(200)),
            ])
            .unwrap();
        let dates: Vec<_> = store
            .channel_videos(&ch, 5)
            .unwrap()
            .iter()
            .map(|v| (v.id.as_str().to_string(), v.published))
            .collect();
        assert_eq!(
            dates,
            [
                ("aaaaaaaaaa2".to_string(), Some(200)),
                ("aaaaaaaaaa1".to_string(), Some(100))
            ]
        );
    }

    #[test]
    fn the_most_active_channels_come_first() {
        let mut store = Store::in_memory();
        let (quiet, busy, empty, gone) = (
            "UC000000000000000000000a",
            "UC000000000000000000000b",
            "UC000000000000000000000c",
            "UC000000000000000000000d",
        );
        for id in [quiet, busy, empty, gone] {
            store.subscribe(&ChannelId::parse(id).unwrap(), id).unwrap();
        }
        store.set_gone(&ChannelId::parse(gone).unwrap(), 1).unwrap();
        store
            .save_videos(&[
                video("aaaaaaaaaa1", quiet, Some(100)),
                video("aaaaaaaaaa2", busy, Some(50)),
                video("aaaaaaaaaa3", busy, Some(300)),
                video("aaaaaaaaaa4", gone, Some(900)),
            ])
            .unwrap();
        let order: Vec<_> = store.most_active().unwrap();
        let order: Vec<&str> = order.iter().map(ChannelId::as_str).collect();
        assert_eq!(order, [busy, quiet, empty]);
    }

    #[test]
    fn a_takeout_list_fills_in_only_what_isnt_known() {
        let mut store = Store::in_memory();
        let ch = ChannelId::parse(CH).unwrap();
        let mut known = video("aaaaaaaaaa1", CH, Some(100));
        known.duration = Some(60);
        store.save_videos(&[known]).unwrap();
        let ids = [
            (VideoId::parse("aaaaaaaaaa1").unwrap(), None),
            (VideoId::parse("aaaaaaaaaa2").unwrap(), Some(5)),
        ];
        assert_eq!(store.import_watch_later(&ids).unwrap(), 2);
        assert_eq!(store.import_watch_later(&ids).unwrap(), 0, "in it already");
        let v = &store.channel_videos(&ch, 5).unwrap()[0];
        assert_eq!(
            (v.title.as_str(), v.duration),
            ("Video aaaaaaaaaa1", Some(60))
        );
        let unknown = store
            .video(&VideoId::parse("aaaaaaaaaa2").unwrap())
            .unwrap()
            .unwrap();
        assert!(unknown.needs_details());
        // A history entry names it: that fills in its title.
        let mut named = Video::placeholder(unknown.id.clone(), false);
        named.title = "Now named".into();
        store.import_history(&[(named, 7)]).unwrap();
        assert_eq!(
            store.video(&unknown.id).unwrap().unwrap().title,
            "Now named"
        );
    }

    #[test]
    fn a_groups_feed_has_only_its_channels_you_subscribe_to() {
        let mut store = Store::in_memory();
        let (a, b) = (CH, "UCxxxxxxxxxxxxxxxxxxxxxx");
        let (ch_a, ch_b) = (ChannelId::parse(a).unwrap(), ChannelId::parse(b).unwrap());
        store.subscribe(&ch_a, "BekBrace").unwrap();
        store.subscribe(&ch_b, "Other").unwrap();
        let mut short = video("aaaaaaaaaa2", a, Some(300));
        short.short = true;
        store
            .save_videos(&[
                video("aaaaaaaaaa1", a, Some(100)),
                short,
                video("bbbbbbbbbb1", b, Some(200)),
            ])
            .unwrap();
        let rust = store.create_group("Rust").unwrap();
        assert_eq!(
            store.create_group(" rust ").unwrap(),
            rust,
            "a name once, in any case"
        );
        store.set_in_group(rust, &ch_a, true).unwrap();
        store.set_in_group(rust, &ch_a, true).unwrap();
        let feed = |s: &Store| -> Vec<String> {
            let videos = s.group_feed(rust, 10).unwrap();
            videos.iter().map(|v| v.id.as_str().to_string()).collect()
        };
        assert_eq!(feed(&store), ["aaaaaaaaaa1"], "no Shorts, no other channel");
        let groups = store.groups().unwrap();
        assert_eq!(
            groups,
            [Group {
                id: rust,
                name: "Rust".into(),
                count: 1
            }]
        );
        assert_eq!(store.groups_of(&ch_a).unwrap(), [rust]);
        assert!(store.groups_of(&ch_b).unwrap().is_empty());

        // Unsubscribed: out of the feed and the count, but kept for when
        // it's subscribed to again.
        store.unsubscribe(&ch_a).unwrap();
        assert!(feed(&store).is_empty());
        assert_eq!(store.groups().unwrap()[0].count, 0);
        store.subscribe(&ch_a, "BekBrace").unwrap();
        assert_eq!(feed(&store).len(), 1);

        // Renamed, but never to another group's name.
        let music = store.create_group("Music").unwrap();
        assert!(!store.rename_group(rust, "MUSIC").unwrap());
        assert!(store.rename_group(rust, "Rust and Go").unwrap());
        let names: Vec<String> = store
            .groups()
            .unwrap()
            .into_iter()
            .map(|g| g.name)
            .collect();
        assert_eq!(names, ["Music", "Rust and Go"]);
        store.set_in_group(rust, &ch_a, false).unwrap();
        assert!(feed(&store).is_empty());
        // Deleted: its channels stay subscriptions.
        store.set_in_group(music, &ch_b, true).unwrap();
        store.delete_group(music).unwrap();
        assert_eq!(store.groups().unwrap().len(), 1);
        assert!(store.groups_of(&ch_b).unwrap().is_empty());
        assert_eq!(store.subscriptions().unwrap().len(), 2);
    }

    #[test]
    fn playlists_keep_their_order_and_their_videos_arent_pruned() {
        let mut store = Store::in_memory();
        let ids: Vec<(VideoId, Option<i64>)> = ["aaaaaaaaaa3", "aaaaaaaaaa1", "aaaaaaaaaa2"]
            .iter()
            .map(|id| (VideoId::parse(id).unwrap(), None))
            .collect();
        assert_eq!(store.import_playlist("Road trip", &ids[..2]).unwrap(), 2);
        assert_eq!(
            store.import_playlist("Road trip", &ids).unwrap(),
            1,
            "added after"
        );
        let lists = store.playlists().unwrap();
        assert_eq!((lists[0].name.as_str(), lists[0].count), ("Road trip", 3));
        let order: Vec<String> = store
            .playlist_videos(lists[0].id)
            .unwrap()
            .iter()
            .map(|v| v.id.as_str().to_string())
            .collect();
        assert_eq!(order, ["aaaaaaaaaa3", "aaaaaaaaaa1", "aaaaaaaaaa2"]);
        let mut old = video("aaaaaaaaaa3", CH, Some(1));
        old.title = "Old".into();
        store.save_videos(&[old]).unwrap();
        store.prune(1_000).unwrap();
        assert_eq!(store.playlist_videos(lists[0].id).unwrap().len(), 3);
        store.delete_playlist(lists[0].id).unwrap();
        assert!(store.playlists().unwrap().is_empty());
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
    fn watch_later_keeps_videos_and_history_remembers_where_you_stopped() {
        let mut store = Store::in_memory();
        let v = video("aaaaaaaaaa1", CH, Some(1));
        store.save_videos(std::slice::from_ref(&v)).unwrap();
        assert!(store.add_to_watch_later(&v.id, 10).unwrap());
        assert!(!store.add_to_watch_later(&v.id, 11).unwrap(), "in already");
        assert_eq!(store.watch_later().unwrap().len(), 1);
        assert!(store.remove_from_watch_later(&v.id).unwrap());
        assert!(!store.remove_from_watch_later(&v.id).unwrap());
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
    fn the_jukebox_keeps_its_order_and_its_videos() {
        let mut store = Store::in_memory();
        let ids: Vec<VideoId> = ["aaaaaaaaaa1", "aaaaaaaaaa2", "aaaaaaaaaa3"]
            .iter()
            .map(|id| VideoId::parse(id).unwrap())
            .collect();
        store
            .save_videos(
                &ids.iter()
                    .map(|id| video(id.as_str(), CH, Some(1)))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let order = |store: &Store| -> Vec<String> {
            store
                .jukebox()
                .unwrap()
                .iter()
                .map(|v| v.id.as_str().to_string())
                .collect()
        };
        assert!(store.add_to_jukebox(&ids[0]).unwrap());
        assert!(store.add_to_jukebox(&ids[1]).unwrap());
        assert!(!store.add_to_jukebox(&ids[0]).unwrap(), "in already");
        assert_eq!(order(&store), ["aaaaaaaaaa1", "aaaaaaaaaa2"]);
        assert!(store.add_to_jukebox(&ids[2]).unwrap());
        let reordered = [ids[1].clone(), ids[0].clone(), ids[2].clone()];
        store.order_jukebox(&reordered).unwrap();
        assert_eq!(order(&store), ["aaaaaaaaaa2", "aaaaaaaaaa1", "aaaaaaaaaa3"]);
        // Added after reordering: still last.
        let extra = VideoId::parse("aaaaaaaaaa4").unwrap();
        store
            .save_videos(&[video("aaaaaaaaaa4", CH, None)])
            .unwrap();
        store.add_to_jukebox(&extra).unwrap();
        assert_eq!(order(&store).last().unwrap(), "aaaaaaaaaa4");
        assert!(store.remove_from_jukebox(&extra).unwrap());
        // Old videos in it aren't pruned.
        assert_eq!(store.prune(100).unwrap(), 0);
        assert!(store.remove_from_jukebox(&ids[1]).unwrap());
        assert!(!store.remove_from_jukebox(&ids[1]).unwrap());
        assert_eq!(order(&store), ["aaaaaaaaaa1", "aaaaaaaaaa3"]);
        assert_eq!(store.prune(100).unwrap(), 1, "out of it, an old one goes");
        assert_eq!(store.clear_jukebox().unwrap(), 2);
        assert!(store.jukebox().unwrap().is_empty());
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

    #[test]
    fn a_removed_entry_is_gone_from_the_files_at_once() {
        let dir = std::env::temp_dir().join(format!("tuitube-scrub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tuitube.db");
        let (old, new, later, queued, cleared) = [
            "SCRUBold001",
            "SCRUBnew001",
            "SCRUBlater1",
            "SCRUBjuke01",
            "SCRUBjuke02",
        ]
        .map(|id| VideoId::parse(id).unwrap())
        .into();
        // One recorded in an earlier run, one in this run, one saved for
        // later and two put in the Jukebox in this run (one taken off, then
        // the rest cleared).
        let store = Store::open(&path).unwrap();
        store
            .record_watch(&old, 1000, Some(100.0), Some(600.0))
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        store
            .record_watch(&new, 2000, Some(42.0), Some(300.0))
            .unwrap();
        store.add_to_watch_later(&later, 2000).unwrap();
        store.add_to_jukebox(&queued).unwrap();
        store.add_to_jukebox(&cleared).unwrap();
        store.forget_watch(&old).unwrap();
        store.forget_watch(&new).unwrap();
        assert!(store.remove_from_watch_later(&later).unwrap());
        assert!(store.remove_from_jukebox(&queued).unwrap());
        assert_eq!(store.clear_jukebox().unwrap(), 1);
        // Still open, as if tuitube were then killed or its window closed.
        let found = |id: &VideoId| {
            ["", "-wal"].iter().any(|suffix| {
                let mut file = path.as_os_str().to_owned();
                file.push(suffix);
                std::fs::read(&file)
                    .unwrap_or_default()
                    .windows(11)
                    .any(|w| w == id.as_str().as_bytes())
            })
        };
        for id in [&old, &new, &later, &queued, &cleared] {
            assert!(!found(id), "{id} is still in the files");
        }
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
