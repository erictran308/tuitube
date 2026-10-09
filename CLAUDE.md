# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

tuitube: a terminal YouTube client for one person's own use, with a YouTube-like layout (sidebar + grid of video cards) and both arrow keys and vim keys. Rust, ratatui, tokio, MIT. A sibling of tuigram (`../tuigram`, Telegram) and tuimeta (`../tuimeta`, Meta's messengers): same security posture, same themes, same `text::clean`, config and settings code. The research behind every design choice is `reports/YouTube terminal client.md` (notes in `research_notes/`); read its security section before changing how programs are started or what is sent to YouTube.

No Google account, ever, in this version: subscriptions are local (imported from Google Takeout's `subscriptions.csv`), refreshed from public channel RSS feeds. Don't add cookie login or `--cookies-from-browser`: Google cookies are the whole Google account (Gmail, Drive), and YouTube's terms let it suspend the account. tuitube contains none of YouTube's cipher or InnerTube code; yt-dlp (a separate program) does that.

## Running

Running tuitube is safe (no account), but it talks to YouTube and mpv opens windows and plays sound. For checks, prefer tests that render into ratatui's `TestBackend` (`ui/mod.rs` tests). Use a separate data folder for trying things: `TT_DATA_DIR=/tmp/tt cargo run`. `tuitube --demo` (`demo.rs`) fills the real UI with made-up channels and videos, preloaded pictures drawn in code, an in-memory store and `App.demo` set: nothing is fetched or played. `docs/hero.png` is one frame of it: `cargo test -- --ignored export_hero_screen` writes `target/hero/screen.json` (cells, colors, where `Images::placed` says each picture went), and `tools/hero.py` draws it as Ghostty would (JetBrains Mono, Nerd Font icons, box lines drawn to the cell edges). Tests that talk to YouTube or run mpv are `#[ignore]`d: `cargo test -- --ignored live` (logged out, a handful of requests; the mpv one plays generated silence).

## Commands

```sh
cargo build
cargo test                       # offline tests
cargo test -- --ignored live     # against YouTube and the real mpv
cargo clippy --all-targets       # keep it warning-free
cargo fmt
cargo run -- --import path/to/subscriptions.csv
```

Needs `yt-dlp`, `deno` and `mpv` at run time (`brew install yt-dlp deno mpv`). `tools::Tools::find` resolves each to an absolute path once (settings `yt_dlp`/`mpv`/`deno`, `TT_YT_DLP`/`TT_MPV`/`TT_DENO`, else absolute `PATH` entries, then Homebrew/system folders); a program is never started by bare name. `.env` is read only by development builds, only from the working directory and only `TT_*` keys (`config::load_dotenv`, never the process environment). The data dir (`TT_DATA_DIR` or the platform app-data dir; `tuitube --help` prints it) is 0700, checked by `config::private_place` when set, and holds `tuitube.db` (SQLite, 0600), `settings.toml` (0600, written through a rename), `cache/images`, `cache/yt-dlp` and `work/` (yt-dlp's working directory).

## Security

Everything from YouTube is untrusted: titles, channel names, descriptions, feed XML, yt-dlp's JSON (it relays YouTube), image bytes, URLs.
- Ids are checked once where they come in (`ids::VideoId`, 11 of `[A-Za-z0-9_-]`; `ids::ChannelId`, `UC` + 22), and every URL tuitube opens, fetches or hands to a program is built from one (`VideoId::url`, `ChannelId::feed_url`…). A video id can start with `-`: programs get URLs, never bare ids, and always after `--`.
- Text that is shown goes through `video::one_line` (`text::clean`, whitespace collapsed, length capped: `MAX_TITLE`, `MAX_CHANNEL`, `MAX_DESCRIPTION`) when it's parsed (feed, yt-dlp, Takeout), before it's stored. Error text from yt-dlp goes through `ytdlp::error_line`, paths through `config::shown`, panic messages through `text::clean`.
- yt-dlp (`ytdlp.rs`) runs per request with `env_clear()` + `tools::child_env` (no `PYTHONPATH`, `LD_PRELOAD`, user site-packages or plugins), `current_dir` = `work/`, `--ignore-config --no-plugin-dirs --no-mark-watched --no-cookies --no-cookies-from-browser --no-remote-components`, Deno named by path (`--no-js-runtimes --js-runtimes deno:PATH`), its own `--cache-dir`, a timeout, capped output, `kill_on_drop`. Its JSON is parsed leniently (every field optional). Never use `--exec`, `--netrc-cmd`, external downloaders, `--cookies-from-browser`, or let tuitube run `yt-dlp -U`.
- Stream URLs from yt-dlp must pass `ytdlp::stream_url_allowed` (https, `*.googlevideo.com` / `*.youtube.com`) before mpv sees them; image URLs must pass `ids::image_url_allowed` (https, `i.ytimg.com`, `iN.ytimg.com`, `yt3.googleusercontent.com`, `yt3.ggpht.com`) before they're fetched.
- mpv (`player.rs`) starts with `--no-config --load-scripts=no --terminal=no --ytdl=no`, the picture URL after `--`, the sound in `--audio-file=`, a title with `$` doubled (`--title` expands `${…}`), the same cleaned environment, and is controlled over one end of a `socketpair` passed as fd 3 (`--input-ipc-client=fd://3`): no socket file exists, and mpv quits when tuitube's end closes. mpv's IPC can run programs; never expose it on a path, port or named pipe others can reach. Windows has no IPC yet (mpv is only started and stopped).
- Nothing tells YouTube what you watched: yt-dlp always gets `--no-mark-watched`, mpv plays only the resolved streams, and a video is resolved only on Enter / `a`, never when a card is highlighted. Local history (`history = true` in settings) stays in `tuitube.db`.
- Images: fetched with `feed::client()` (rustls, timeouts, ≤3 redirects, no cookies), at most `MAX_BYTES`, cached by checked id only (`images::cache_name`), decoded with the `image` crate built for JPEG/PNG/WebP only (no C decoders), within `images::limits`, at most `MAX_BUILDING` at once, a decoder panic contained (`images::panic_is_contained`, checked by the panic hook). Sixel/iTerm2 images aren't drawn under popups or under text (`Images::paints_over`).
- Image protocol (`images::picker`): what the terminal answers, unless `images` in settings / `TT_IMAGES` forces one; a terminal that answers nothing but whose environment says Ghostty or kitty gets the kitty protocol with the cell size from `TIOCGWINSZ` (`images::choose`). Thumbnails come from `VideoId::large_thumbnail_url` (1280×720 WebP), falling back to the search's URL and the 480×360 JPEG; channel photos are looked up only when real images can be drawn. `tuitube --check` prints the tools found and the image detection.
- Links open only through `open::that_detached` with a URL built from a checked id; `y` copies the same URL. No link from a description or title is ever opened.
- Takeout import (`takeout.rs`): a size cap, rows read by position (the header is localized), a row counts only if its id is a channel id; `\\server\share` paths are refused.

## Architecture

**Event loop (`app.rs`, `App::run`).** One `tokio::select!` over terminal events, `AppEvent`s (feeds, yt-dlp answers, channel photos, resolved streams, mpv events, finished images) and a 1 s tick. Each wake drains the backlog, then redraws; after each frame `Images::fetch` starts what the frame wanted. All state lives in `App`. Requests that change the view carry `App.request`, and answers to older ones are dropped; mpv events carry the playback number (`Player.play`).

**Views.** `View::{Home, Shorts, Search, WatchLater, History, Channel}`; `App::reload` rebuilds `App.videos` from the store (or the last search's results) and keeps the selection by id. `App::show` switches views (pushing the old one on `back` for Backspace / Ctrl-o). Moving in the sidebar shows what's stored at once; Enter on a channel also fetches its newest 60 videos with yt-dlp. Search asks for 30 results, then 30 more when the selection nears the end (`App::load_more`), up to 150.

**Feeds (`feed.rs`).** Each subscription's Atom feed (15 newest uploads; `/shorts/` links mark Shorts), `PARALLEL` at once, `TRIES` tries with backoff (YouTube's feed server fails a lot). A channel's feed is fetched again only when its stored copy is older than `refresh_hours` (default 72, `Settings::refresh_every`), checked every `CHECK_EVERY`; `R` fetches every feed now. A failed feed waits for the next check (`App.refreshed`). Feeds carry no lengths: `App::want_length` (called for each card on screen without one) has yt-dlp list the channel's Videos tab, then its Live tab (`ytdlp::Tab`), 30 videos per look for the cost of one; when each tab was looked at is kept in `channels.videos_checked` / `streams_checked` (`Store::tab_checked`), so a tab is looked at again only for a video newer than that, or every `LIVE_EVERY` for a live stream or premiere on screen. Opening a channel fetches its videos only when the kept ones are older than `refresh_hours`.

**Store (`store.rs`).** SQLite: `channels` (subscribed flag, photo URL and when it was looked up, when the feed was fetched), `videos` (merged across sources: a later source keeps what an earlier one knew), `watch_later`, `history` (position and length, for resuming and the red watched bar).

**Channel photos.** `App::avatar_url`: from the store if looked up in the last `AVATAR_EVERY`, else the same background look at the channel's Videos tab as for lengths (`App::look_up_channel`, its own semaphore, so it never delays what you asked for); asked for at 176 px (`ytdlp::small_avatar`).

**Playing.** Enter / `a` → `YtDlp::resolve` (format from `ytdlp::Quality`: no AV1, at most `max_height`) → `Player::start`. Resumes from the stored position if it's more than `RESUME_AFTER` from either end; the position is saved every `SAVE_EVERY` and when playback ends. The player bar (`ui::player_bar`) shows position and keys (Space, `,` `.` `<` `>`, `X`).

**UI (`ui/`).** `mod.rs`: top bar (logo, search box, feed progress), player bar, status bar, help. `sidebar.rs`: the menu and subscriptions, scrolled to the selection. `grid.rs`: `geometry` fits cards of at least `card_width` columns, thumbnails 16:9 in pixels from the terminal's cell size, and the top of the next row in what's left (kitty and block images are cut with `allow_clipping`; sixel/iTerm2 ones show a placeholder there); a card is the thumbnail (with the length badge), the watched bar, the channel photo (or its initial) beside a two-line title, the channel, views · age, and the description's start. Below 70 columns the sidebar and the grid take turns.
