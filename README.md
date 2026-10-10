<div align="center">

# tuitube

**YouTube at the speed of your keyboard.**

A terminal YouTube client (TUI) that looks like YouTube: a sidebar, a grid of video cards with real thumbnails and channel photos, and mpv to play them.<br>
Arrow keys or vim keys. No Google account, no login, nothing sent to your watch history.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024-orange.svg)](https://www.rust-lang.org)
[![Built on yt-dlp](https://img.shields.io/badge/built%20on-yt--dlp%20%2B%20mpv-red.svg)](#built-with)

**[Get started](#get-started)** · **[Keys](#keys)** · **[Settings](#settings)** · **[Privacy](#privacy-and-security)**

<img src="docs/hero.png" alt="tuitube: the sidebar with Home, Shorts, Search, Watch later, History and subscriptions, beside a grid of video cards with thumbnails, lengths, a LIVE badge, channel photos, titles, views and descriptions, and a player bar playing a video's sound">

<sub>The demo's made-up channels and videos: <code>tuitube --demo</code></sub>

</div>

> [!IMPORTANT]
> **For personal use only.** tuitube isn't made, endorsed or allowed by YouTube or Google. It plays videos through [yt-dlp](https://github.com/yt-dlp/yt-dlp) and [mpv](https://mpv.io), which YouTube's terms don't permit. It never logs in, so your Google account is never involved. See the [disclaimer](#disclaimer).

---

## Why tuitube

- **Looks like YouTube.** Home, Shorts, Search, Watch later and History in the sidebar, then your channels. Videos in a grid of cards: the thumbnail with its length, the watched part in red under it, the channel's photo beside the title, the channel, views and age, and the start of the description. The grid fills the window, down to the top of the next row.
- **Your subscriptions, without an account.** Import them once from Google Takeout. tuitube keeps them on your computer and gets their new videos from YouTube's public feeds, so there's no password, cookie or API key anywhere. A channel YouTube has removed is marked in the sidebar, for you to unsubscribe with `x`.
- **Arrows or vim, your choice.** `←↓↑→` and `h j k l` both move, `Enter` plays, `/` searches, `gg` / `G`, `Ctrl-d` / `Ctrl-u`, `PgUp` / `PgDn`. Left from the first column goes to the sidebar, `Tab` back. `?` lists every key, and `O` opens the settings.
- **Real thumbnails.** Full 1280×720 pictures in Ghostty, kitty, WezTerm and iTerm2 (sixel terminals too), with the channel's photo cut to a circle. Block-character previews in any other terminal.
- **Plays in mpv.** `Enter` opens the video in mpv's window, `a` plays the sound only, with a two-row player bar in tuitube (the title, then the progress and keys): `Space` pauses, `,` `.` skip 10 seconds, `<` `>` a minute, `X` stops. Videos carry on where you stopped.
- **Skip sponsors, with SponsorBlock.** Turn it on in the settings (`O`) and the parts of videos that [SponsorBlock](https://sponsor.ajay.app)'s users marked (sponsor reads, intros, outros) are skipped as they come, with a yellow **[SKIP]** in the player bar. Off until you turn it on, since it asks a server that isn't YouTube's.
- **Autoplay and Mixes.** When a video ends, the next card in the list plays, so Watch later, a channel or a search plays through (`N` skips ahead, `A` turns autoplay off). `m` opens YouTube's Mix of a video, similar videos picked by YouTube, to play through; without an account YouTube makes Mixes only for music videos.
- **Search YouTube.** `/` searches, and more results load as you scroll. `c` opens a video's channel, `S` subscribes to it (or unsubscribes, `u` undoes).
- **Watch later and History.** `w` saves a video for later, and what you play shows up in History, both kept only on your computer. `x` takes a video off either list.
- **Live and upcoming.** Live streams say **LIVE**, scheduled ones **UPCOMING**, and Shorts get a tab of their own instead of crowding Home. Don't want them? Turn off Shorts, or live streams and premieres, in the settings.
- **Gentle on YouTube.** Feeds are fetched again only when they're more than 30 minutes old, and Home says when it was last updated (`R` fetches now). Lengths are looked up only for the cards on screen, 30 videos at a time, and tuitube backs off for an hour if YouTube asks it to slow down.
- **Make it yours.** Catppuccin, Tokyo Night, Dracula, Gruvbox, Nord and Rosé Pine themes (picked in the settings, `O`), Nerd Font icons where your terminal has them, and card and sidebar widths to taste.
- **Private by design.** No account, no telemetry, no servers in between. Nothing goes into a YouTube watch history, and a video is only looked up when you play it, never when you move over it.

## Get started

**1. Install the three programs tuitube runs.** It finds them on your `PATH`:

| Program | What for |
| --- | --- |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | Talks to YouTube: searches, channels, and what to play |
| [Deno](https://deno.com) | Runs the JavaScript yt-dlp needs for YouTube |
| [mpv](https://mpv.io) | Plays videos |

```sh
brew install yt-dlp deno mpv                               # macOS
sudo pacman -S yt-dlp deno mpv                             # Arch
winget install yt-dlp.yt-dlp DenoLand.Deno shinchiro.mpv   # Windows
```

**2. Install tuitube.** If you have Rust and [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), that's one command, on any system:

```sh
cargo binstall tuitube
```

Otherwise, on macOS or Linux, paste this into a terminal. It puts `tuitube` in `~/.local/bin`:

```sh
mkdir -p ~/.local/bin
curl -fsSL https://github.com/erictran308/tuitube/releases/latest/download/tuitube-aarch64-apple-darwin.tar.gz | tar xz -C ~/.local/bin tuitube
```

Swap the file name for your computer's:

| Computer | File |
| --- | --- |
| Mac with Apple silicon (M1 or later) | [`tuitube-aarch64-apple-darwin.tar.gz`](https://github.com/erictran308/tuitube/releases/latest/download/tuitube-aarch64-apple-darwin.tar.gz) |
| Mac with Intel | [`tuitube-x86_64-apple-darwin.tar.gz`](https://github.com/erictran308/tuitube/releases/latest/download/tuitube-x86_64-apple-darwin.tar.gz) |
| Linux, x86_64 | [`tuitube-x86_64-unknown-linux-gnu.tar.gz`](https://github.com/erictran308/tuitube/releases/latest/download/tuitube-x86_64-unknown-linux-gnu.tar.gz) |
| Linux, ARM64 | [`tuitube-aarch64-unknown-linux-gnu.tar.gz`](https://github.com/erictran308/tuitube/releases/latest/download/tuitube-aarch64-unknown-linux-gnu.tar.gz) |
| Windows, x86_64 | [`tuitube-x86_64-pc-windows-msvc.zip`](https://github.com/erictran308/tuitube/releases/latest/download/tuitube-x86_64-pc-windows-msvc.zip) |
| Windows, ARM64 | [`tuitube-aarch64-pc-windows-msvc.zip`](https://github.com/erictran308/tuitube/releases/latest/download/tuitube-aarch64-pc-windows-msvc.zip) |

- **Windows:** download the `.zip`, unzip it, and run `tuitube.exe` from Windows Terminal.
- **Linux:** needs glibc 2.35 or newer: Ubuntu 22.04, Debian 12, Fedora 36, RHEL 9 or later.
- **macOS:** if you downloaded the file in a browser instead, macOS blocks the app. Run `xattr -d com.apple.quarantine tuitube` once to allow it.

If `tuitube` isn't found afterwards, add `~/.local/bin` to your `PATH`. Each file comes with a signed record of the commit it was built from; to check one, run `gh attestation verify <file> --repo erictran308/tuitube`.

**Or build it from source** (Rust 1.90 or newer, and a C compiler for SQLite):

```sh
cargo install tuitube --locked
```

**3. Run it.**

```sh
tuitube
```

To look around first, `tuitube --demo` shows made-up channels and videos, fetching and playing nothing.

### Bring your subscriptions

1. Open [takeout.google.com](https://takeout.google.com) and click **Deselect all**.
2. Tick **YouTube and YouTube Music**, click **All YouTube data included**, and keep only **subscriptions**.
3. Export, download the archive and unzip it.
4. In tuitube, press `I` and paste (or drop) the path to `subscriptions.csv`. Or, before starting it:

   ```sh
   tuitube --import ~/Downloads/Takeout/YouTube\ and\ YouTube\ Music/subscriptions/subscriptions.csv
   ```

Home fills up with your channels' newest videos within a minute. You can also start from nothing: search with `/` and press `S` on a video to subscribe to its channel.

### Check your setup

```sh
tuitube --check
```

It says where yt-dlp, Deno and mpv are, and how your terminal draws images. If thumbnails look blocky and channel photos are letters, see [Images and icons](#images-and-icons).

## Keys

The status bar shows the keys for where you are, and `?` lists them all. `Tab` there goes to the settings.

| Key | Action |
| --- | --- |
| `←↓↑→` / `h j k l` | Move. Left from the first column goes to the sidebar |
| `Tab` / `Esc` | Sidebar ↔ videos |
| `gg` / `G`, `Home` / `End` | First / last |
| `PgUp` / `PgDn`, `Ctrl-u` / `Ctrl-d` | A page / half a page |
| `Enter` | Play in mpv; in the sidebar, open the view or channel |
| `a` | Listen: sound only, with the player bar in tuitube |
| `m` | YouTube's Mix of the video: similar videos to play through (music videos only, without an account) |
| `N` | Play the next video now |
| `A` | Autoplay on or off: when a video ends, the next card (or the rest of the Mix) plays |
| `Space` | Pause or play |
| `,` / `.`, `<` / `>` | Back / ahead 10 seconds, 1 minute |
| `X` | Stop |
| `w` | Save to Watch later, or take it off |
| `x` | Take off Watch later or History; in the sidebar, unsubscribe |
| `c` | The video's channel |
| `S` / `u` | Subscribe to the video's channel or unsubscribe / undo unsubscribing |
| `/` or `s` | Search YouTube (`Enter` searches, `Esc` cancels, `Ctrl-u` clears) |
| `Backspace` / `Ctrl-o` | Back to the view before |
| `o` / `y` | Open the video in your browser / copy its link |
| `R` | Refresh: your subscriptions, the search or the channel |
| `I` | Import subscriptions from Google Takeout |
| `?` / `O` | Every key / the settings: theme, Shorts, live streams, SponsorBlock, History |
| `q` | Quit |

## Settings

`O` (or `?` then `Tab`) opens the settings: show Shorts, show live streams and premieres, descriptions, autoplay, SponsorBlock, History and the theme. `j` / `k` move, `Enter` or `Space` turns one on or off (or picks the theme), and it's saved at once, changing only its own line in `settings.toml` and keeping the rest of the file (but not its comments). `A` turns autoplay on and off from anywhere.

Everything else is in `settings.toml` in [your data folder](#your-data). Every line is optional; changes made by hand take effect the next time tuitube starts:

```toml
theme = "mocha"        # latte, frappe, macchiato, mocha, tokyonight, dracula, gruvbox, nord, rose-pine
max_height = 1080      # tallest picture to play: 2160, 1440, 1080, 720, 480
refresh_minutes = 30   # how old a channel's feed may get before it's fetched again (15 at least)
history = true         # remember what you watch here, and where you stopped
autoplay = true        # when a video ends, play the next card (or the rest of its Mix)
sponsorblock = false   # skip what SponsorBlock's users marked
sponsorblock_categories = ["sponsor", "intro", "outro"]  # also: selfpromo, interaction, preview, hook, music_offtopic, filler
descriptions = true    # the start of each description on its card
shorts = true          # Shorts in the sidebar, search results, channels and Mixes
live = true            # live streams and premieres, while live or not started yet
images = "auto"        # auto, kitty, sixel, iterm2 or blocks
icons = "auto"         # auto, nerd (Nerd Font icons) or plain
card_width = 34        # narrowest card, in columns
sidebar_width = 26

# Where the programs are, if they aren't on PATH:
# yt_dlp = "/opt/homebrew/bin/yt-dlp"
# mpv = "/opt/homebrew/bin/mpv"
# deno = "/opt/homebrew/bin/deno"
```

### Images and icons

tuitube asks your terminal how it draws images. Ghostty, kitty, WezTerm, iTerm2 and sixel terminals show real thumbnails and channel photos; others get block-character previews.

Inside tmux or another multiplexer, the question often doesn't reach the terminal. In tmux, tuitube turns `allow-passthrough` on for its own pane while it runs, so images can get through, and puts the pane's old value back when it quits. For any multiplexer, `images = "kitty"` (or `TT_IMAGES=kitty`) tells tuitube to draw them anyway when your terminal is Ghostty or kitty.

Icons are Nerd Font icons in Ghostty, kitty and WezTerm, which have them built in, so they're all the same size. If one shows as a box, set `icons = "plain"`.

## Privacy and security

- **No account.** No login, no cookies, no API key: YouTube sees an anonymous visitor. tuitube never reads your browser's cookies.
- **No watch history.** Nothing tells YouTube what you watched. A video is looked up only when you play it, open its Mix (`m`), or autoplay plays it after the one before it ends, never when you move over it.
- **One route for everything.** If you use a proxy, tuitube, yt-dlp and mpv all go through the same one: the first set of `https_proxy`, `HTTPS_PROXY`, `all_proxy`, `ALL_PROXY`, `http_proxy`, `HTTP_PROXY`. It must be an `http://` proxy, since mpv can't send videos through any other kind; tuitube refuses to start with a SOCKS one rather than let videos go around it. With none set, nothing uses a proxy, not even one in your system settings. `tuitube --check` shows which is in use.
- **SponsorBlock only if you want it.** It's off until you turn it on in the settings (`O`). Then, for each video you play, tuitube sends sponsor.ajay.app the first 4 characters of a SHA-256 hash of its id (never the id), gets back the segments of the hundred or so videos that share them, and picks its own out here. The server sees your IP address and that you played one of those videos.
- **Your lists stay here.** Subscriptions, Watch later, History and settings are in one folder readable only by you. `x` takes a video off History or Watch later for good, even the one playing, and it's wiped from the database files at once. `history = false` stops History; what's already there stays until you take it off.
- **Locked-down helpers.** yt-dlp and mpv run with none of your own config files, scripts or plugins, a cleaned environment, and only ever URLs that tuitube built from checked video ids. tuitube controls mpv over a private channel no other program can reach. No program is ever found in the folder you start tuitube from, and when a yt-dlp run ends early, everything it started ends with it.
- **Pastes stay text.** Pasted text only ever goes into the search or import box: a paste that arrives as keystrokes (as it always does on Windows) is never taken for commands.
- **Careful with what YouTube sends.** Titles, names and descriptions are cleaned of terminal escape codes before they're shown; images come only from YouTube's image servers, within size limits.

The research behind these choices is in [`reports/`](reports/YouTube%20terminal%20client.md). To report a problem, see [SECURITY.md](SECURITY.md).

## Your data

Everything is in one folder on your machine (`tuitube --help` prints its path):

| OS | Location |
| --- | --- |
| macOS | `~/Library/Application Support/tuitube` |
| Linux | `~/.local/share/tuitube` |
| Windows | `%LOCALAPPDATA%\tuitube` |

It holds `tuitube.db` (your subscriptions, the videos tuitube has seen, Watch later and History), `settings.toml`, and a cache of thumbnails and channel photos. Deleting it starts tuitube afresh.

| Variable | Use |
| --- | --- |
| `TT_DATA_DIR` | Keep the data folder somewhere else |
| `TT_YT_DLP`, `TT_MPV`, `TT_DENO` | Where those programs are, if not on `PATH` |
| `TT_IMAGES` | How images are drawn: `auto`, `kitty`, `sixel`, `iterm2` or `blocks` |
| `TT_ICONS` | Which icons: `auto`, `nerd` or `plain` |
| `https_proxy` and the like | The one `http://` proxy everything goes through (see [Privacy and security](#privacy-and-security)) |

## Development

```sh
cargo test                       # offline tests
cargo test -- --ignored live     # against YouTube (logged out), sponsor.ajay.app and the real mpv
cargo clippy --all-targets
TT_DATA_DIR=$HOME/tt-test cargo run   # a separate data folder (a full path) for trying things
cargo run -- --demo              # made-up channels and videos, nothing fetched
```

`docs/hero.png` is one frame of `--demo`, drawn by `tools/hero.py` (its first lines say how).

Copy `.env.example` to `.env` in the source folder for development settings. Only development builds read `.env`, and only from the source folder, never the folder they're started from: an installed tuitube ignores it, so a `.env` in a folder you cloned can't change which programs it runs.

## Built with

- [yt-dlp](https://github.com/yt-dlp/yt-dlp) with [Deno](https://deno.com), and [mpv](https://mpv.io), run as separate programs
- [ratatui](https://ratatui.rs) and [ratatui-image](https://github.com/benjajaja/ratatui-image)
- [rusqlite](https://github.com/rusqlite/rusqlite), [reqwest](https://github.com/seanmonstar/reqwest) with rustls, and [quick-xml](https://github.com/tafia/quick-xml)
- The same UI groundwork, themes and safety rules as [tuigram](https://github.com/erictran308/tuigram) and [tuimeta](https://github.com/erictran308/tuimeta)
- Colors from [Catppuccin](https://catppuccin.com), [Tokyo Night](https://github.com/folke/tokyonight.nvim), [Dracula](https://draculatheme.com), [Gruvbox](https://github.com/morhetz/gruvbox), [Nord](https://www.nordtheme.com) and [Rosé Pine](https://rosepinetheme.com)

## Disclaimer

tuitube is an unofficial, independent hobby project. It is **not** affiliated with, endorsed by, sponsored by, or connected to Google LLC or YouTube in any way. "YouTube" and "Google" are trademarks of Google LLC, used here only to say what tuitube talks to.

Playing YouTube videos outside YouTube's own player breaks YouTube's terms of service. tuitube never logs in, but YouTube may still slow down or block requests from your network. **You use tuitube entirely at your own risk**, and you are responsible for complying with the laws and terms that apply to you.

The software is provided "as is", without warranty of any kind, express or implied. To the fullest extent permitted by law, the authors and contributors are **not liable** for any claim, damages or other liability arising from the software or its use. See the [MIT license](LICENSE) for the full terms.

For personal use only: do not use tuitube to download or redistribute videos, for automation, scraping, or any commercial purpose.

## License

[MIT](LICENSE). yt-dlp, Deno and mpv are separate programs under their own licenses; tuitube only starts them.
