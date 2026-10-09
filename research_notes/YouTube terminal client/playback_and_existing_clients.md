# Playback options and existing YouTube clients for a Rust TUI (tuitube), as of 2026-10-09

Method note: most facts come from primary sources: the mpv v0.41.0 docs and source at that tag, mpv master, the yt-dlp README and Changelog, repo READMEs, Cargo manifests and release feeds, GitHub security advisories, NVD, crates.io and Repology. Some facts come from **hands-on tests run in this session on 2026-10-09** on macOS arm64 with mpv v0.41.0, yt-dlp 2026.08.19, deno 2.9.6 and yt-dlp-ejs 0.8.0, all from Homebrew. Those are labelled "[local test 2026-10-09]". Release and commit dates come from each repo's GitHub `releases.atom` / `commits.atom` feeds (the `<updated>` stamp), fetched 2026-10-09.

## Q1. mpv as the player: JSON IPC vs libmpv, ytdl_hook vs direct stream URLs, YouTube 2025–2026 changes, and features (audio-only, background, resume, chapters, subs, quality, hwdec)

### Takeaway
The best fit for tuitube is to spawn the system `mpv` as a separate process and drive it over JSON IPC. This gives crash isolation, no GPL/LGPL linking question and no libmpv build pain on 3 OSes × 2 arches. libmpv2 6.0.0 (May 2026) is a working Rust binding, but it adds packaging and licensing weight for little gain in a TUI. As of 2026-10-09, YouTube playback still works through both routes. One is mpv's ytdl_hook, where mpv calls yt-dlp. The other is direct googlevideo DASH URLs passed as video plus `--audio-file`. Both depend on yt-dlp ≥ 2025.11.12 with a JS runtime (Deno), because the n/sig JS challenges are now solved by EJS. Direct URLs expire after about 6 h and are IP-bound. The open risk is SABR: YouTube's `web` client is SABR-only, and yt-dlp's SABR support (PR #13515) is still unmerged.

### Cited Findings

**JSON IPC basics**
- mpv's JSON IPC is enabled with `--input-ipc-server=<path>`. That is a Unix socket on Unix, and on Windows a named pipe `\\.\pipe\<name>`; mpv adds the `\\.\pipe\` prefix automatically if it is missing. `--input-ipc-client=fd://N` instead attaches one already-connected socket/pipe FD (for example from `socketpair()`), and "The player quits when the connection is closed." — [mpv v0.41.0 options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- On Windows, `--input-ipc-client` requires "a wrapped (created by `_open_osfhandle`) named pipe server handle with a client already connected… duplex with overlapped IO and inheritable handles." — [mpv v0.41.0 options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- Protocol: UTF-8 JSON, one message per line ending in `\n`, with no `\n` inside a message. Requests look like `{"command":[...], "request_id": <int>}`. Replies are `{"error":"success","data":...,"request_id":...}`, and events arrive as `{"event": ...}` on the same stream. `request_id` must be an integer; other types are deprecated. — [mpv v0.41.0 ipc.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/ipc.rst); the integer requirement dates to 0.30.0 — [interface-changes.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/interface-changes.rst)
- The mpv manual warns: "This is not intended to be a secure network protocol. It is explicitly insecure: there is no authentication, no encryption, and the commands themselves are insecure too. For example, the `run` command is exposed, which can run arbitrary system commands." — [mpv v0.41.0 ipc.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/ipc.rst)
- Windows pipe hardening in mpv's source: the pipe is created with an SDDL that grants GENERIC_READ/WRITE only to the current user SID and blocks lower integrity levels. It also uses `PIPE_REJECT_REMOTE_CLIENTS` and `FILE_FLAG_FIRST_PIPE_INSTANCE`. On Unix, mpv calls `fchmod(ipc_fd, 0600)`, `unlink()`s any existing path before `bind()`, and supports Linux abstract sockets when the path starts with `@`. — [ipc-win.c v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/input/ipc-win.c), [ipc-unix.c v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/input/ipc-unix.c)
- `loadfile <url> [<flags> [<index> [<options>]]]`: mpv 0.38.0 inserted an `index` argument. This "breaks all existing uses of this command which make use of the argument to include the list of options", so pass `-1` as the index when using per-file options. — [mpv v0.41.0 input.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/input.rst)
- Useful properties for a TUI include `time-pos`, `playback-time`, `percent-pos`, `duration`, `chapter`, `chapter-list`, `chapters`, `track-list`, `media-title`, `idle-active`, `eof-reached`, `paused-for-cache` and `demuxer-cache-state`. Useful commands include `observe_property`, `audio-add`, `sub-add`, `write-watch-later-config` and `run`. — [mpv v0.41.0 input.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/input.rst), [ipc.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/ipc.rst)
- [local test 2026-10-09] mpv 0.41.0 was spawned with `--no-config --idle=yes --terminal=no --load-scripts=no --osc=no --input-default-bindings=no --input-terminal=no --ytdl=yes --ytdl-raw-options=ignore-config=,js-runtimes=deno --ytdl-format=bv*[height<=720][vcodec^=avc1]+ba[acodec^=mp4a]/b --input-ipc-server=m.sock`. Results:
  - `get_property mpv-version` returned `mpv v0.41.0`.
  - `loadfile <youtube watch URL> replace -1 start=30` worked. `start-file` came at 0.0 s, `file-loaded` at 2.7 s (this includes the yt-dlp run) and `playback-restart` at 6.0 s.
  - `media-title` returned the YouTube title. `track-list` showed h264 720p video, AAC audio and 5 external subtitle tracks.
  - `hwdec-current` was `no`, because `--no-config` drops any user hwdec setting.
- [local test 2026-10-09] macOS rejected a socket path under the long scratchpad directory with `OSError: AF_UNIX path too long`, because `sun_path` is about 104 bytes on macOS. mpv's own code rejects paths of `sizeof(sun_path)-1` or more — [ipc-unix.c](https://github.com/mpv-player/mpv/blob/v0.41.0/input/ipc-unix.c). The socket file was still present after `quit` in this test.

**libmpv embedding**
- `mpv_create()` uses "initial settings suitable for embedding": the terminal is never touched, no config files are loaded ("roughly equivalent to using --config=no"), idle mode is on, and parts of input handling are disabled. `mpv --show-profile=libmpv` [local test] prints: `config=no, idle=yes, terminal=no, input-terminal=no, osc=no, input-default-bindings=no, input-vo-keyboard=no, input-media-keys=no, media-controls=no`. — [client.h v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/include/mpv/client.h)
- Client API version in v0.41.0: `MPV_CLIENT_API_VERSION MPV_MAKE_VERSION(2, 5)`. — [client.h](https://github.com/mpv-player/mpv/blob/v0.41.0/include/mpv/client.h)
- Licensing: "mpv as a whole is licensed under the GNU General Public License GPL version 2 or later… by default". It is LGPLv2.1+ only "if built without using any GPL only files". `-Dgpl=false` is a convenience, but it "does not in itself create a LGPLv2.1+ license grant." — [mpv Copyright v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/Copyright). Homebrew labels its mpv formula "GPL-2.0-or-later AND LGPL-2.1-or-later". — [formulae.brew.sh mpv](https://formulae.brew.sh/formula/mpv)
- Rust crates as of 2026-10-09 (from crates.io):
  - `libmpv2` 6.0.0 (updated 2026-05-12, LGPL-2.1, about 40k recent downloads). Its README says "Libmpv version 2.0 (mpv version 0.35.0) is the minimum required version". It offers a `build_libmpv` feature and examples for events, a custom protocol, and OpenGL rendering into SDL2. — [crates.io libmpv2](https://crates.io/crates/libmpv2), [libmpv2-rs README](https://github.com/kohsine/libmpv2-rs)
  - `libmpv` 2.0.1 was last updated 2020-09-29 and is effectively abandoned. — [crates.io libmpv](https://crates.io/crates/libmpv)
  - `libmpv-sirno` 2.0.2-fork.1 (2022-12-28) is the fork youtube-tui uses. — [crates.io libmpv-sirno](https://crates.io/crates/libmpv-sirno)
  - IPC crates: `mpvipc` 1.3.1 (2026-02-15, **GPL-3.0**, GitLab); `mpv-ipc` 0.1.7 (2025-02-09, MIT); `mpv-client` 1.1.0 (2025-06-28, GPL-3.0). — [crates.io mpvipc](https://crates.io/crates/mpvipc), [mpv-ipc](https://crates.io/crates/mpv-ipc), [mpv-client](https://crates.io/crates/mpv-client)
- Bit-rot warning from a real Rust client: ytui-music's README says "since the dependency `libmpv` seems not to be maintained anymore, you will probably need to build from source in any platform." — [ytui-music README](https://github.com/sudipghimire533/ytui-music)
- youtube-tui ships an embedded libmpv audio player (feature `mpv = ["dep:libmpv-sirno"]`) but launches external `mpv` for video. — [youtube-tui Cargo.toml](https://github.com/Siriusmart/youtube-tui/blob/HEAD/Cargo.toml), [youtube-tui README](https://github.com/Siriusmart/youtube-tui)

**ytdl_hook (mpv calls yt-dlp)**
- `--ytdl=<yes|no>` enables the built-in youtube-dl hook (default yes). Script options use the `ytdl_hook-` prefix through `--script-opts`. They are `try_ytdl_first`, `exclude`, `include` (default includes `youtube.com`, `youtu.be`, `twitch.tv`), `all_formats`, `force_all_formats`, `thumbnails`, `use_manifests` and `ytdl_path`. The defaults for `ytdl_path` are "yt-dlp", "yt-dlp_x86" and "youtube-dl", and "mpv looks in order for the configured paths in PATH and in mpv's config directory." — [mpv v0.41.0 options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- `--ytdl-format` is passed straight to yt-dlp. `--ytdl-raw-options=key=value,...` passes arbitrary yt-dlp options; options without an argument need `=`, for example `force-ipv6=`, and "There is no sanity checking". — [mpv v0.41.0 options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- How ytdl_hook runs yt-dlp (master): `yt-dlp --no-warnings -J --flat-playlist --sub-format ass/srt/best`, plus `--format` or the default `bestvideo*+bestaudio/bestvideo+bestaudio/best`. Raw options are appended as `--<key> <value>`, then `--sub-langs all --write-srt` and `--no-playlist`, then a literal `--` before the URL. If `options/vid == "no"` and no format is set, it uses `bestaudio/best` ("Video disabled. Only using audio"). It does **not** pass `--ignore-config`. — [ytdl_hook.lua master](https://github.com/mpv-player/mpv/blob/master/player/lua/ytdl_hook.lua)
- ytdl_hook copies the per-format `http_headers` from yt-dlp (User-Agent, Cookie, Referer, X-Forwarded-For) and the cookies into file-local mpv options. It builds `edl://` URLs to combine separate DASH audio/video tracks and subtitle tracks. — [ytdl_hook.lua v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/player/lua/ytdl_hook.lua)
- Unreleased behaviour changes on mpv master since 0.41.0, which will land in 0.42 (master's interface-changes already has a `--- mpv 0.42.0 ---` header): "default to all_formats=yes" (2026-04-21), "write cookies via cookies-file instead of stream-lavf-o" (2026-05-16), "defer loading until the formats become available" (2026-06-26) and "pass a default format again" (2026-09-13). — [ytdl_hook.lua commit history](https://github.com/mpv-player/mpv/commits/master/player/lua/ytdl_hook.lua), [interface-changes.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/interface-changes.rst)
- mpv master also adds a libcurl network backend that serves http(s) URLs instead of FFmpeg when built in. It supports HTTP/2 and HTTP/3, honours `--user-agent`, `--http-header-fields`, `--cookies*` and `--tls-*`, and has `--curl-enabled` (default yes). This is unreleased as of 2026-10-09. — [mpv master options.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/man/options.rst)
- mpv 0.41.0 (GitHub release, feed-dated 2025-12-22) made gpu-next the default VO, prefers Vulkan hwdec when available, needs FFmpeg ≥ 6.1, and includes "ytdl_hook.lua: fix incorrect default format used with yt-dlp" and "vo_kitty: add auto-multiplexer-passthrough option". — [mpv v0.41.0 release](https://github.com/mpv-player/mpv/releases/tag/v0.41.0)

**Direct googlevideo URLs (client resolves streams, mpv plays URLs)**
- HTTP options available for direct URLs: `--user-agent`, `--cookies`, `--cookies-file` (Netscape format), `--http-header-fields` (string list) and `--http-proxy`. Proxies are not used for https URLs, and setting the proxy "does not try to make the ytdl script use the proxy". `--audio-file` adds an external audio track. — [mpv v0.41.0 options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- [local test 2026-10-09] `yt-dlp --ignore-config -J` on a public video gave:
  - Client used: `visionos player API JSON`. JS challenges were solved with deno, and the PO Token providers list was "none".
  - 49 formats: 28 `https` (DASH progressive-range), 17 `m3u8_native` (HLS from `manifest.googlevideo.com`) and 4 `mhtml` storyboards.
  - Default selection was `401+251`, meaning **AV1 2160p + Opus**.
  - Every https googlevideo URL had `expire` exactly **6.0 h** ahead and an `ip=` parameter (IP-bound). None had a `pot` parameter.
  - Audio: itag 140 is `mp4a.40.2` at about 129.5 kbps (m4a); itag 251 is Opus at about 128.9 kbps (webm).
  - Per-format `http_headers`: a Chrome 150 desktop User-Agent, Accept, Accept-Language and `Sec-Fetch-Mode: navigate`.
- [local test 2026-10-09] `mpv --no-config --ytdl=no --vid=no <itag140 URL>` played (AAC 2ch 44.1 kHz). `mpv --no-config --ytdl=no --audio-file=<itag140 URL> <itag135 URL>` played 480p h264 plus external audio with no extra headers. mpv printed the **full signed URL** (including `ip`, `sig`, `lsig` and `expire`) as the external audio track title in its terminal output.
- Real clients use this route. ytsub's "play_from_formats" passes `--no-ytdl`, `--force-media-title=<title>`, `--audio-file=<url>`, `--sub-file=<caption>` and `--chapters-file=<file>` as separate argv entries. — [ytsub src/mpv/mod.rs](https://github.com/sarowish/ytsub/blob/HEAD/src/mpv/mod.rs). pipe-viewer's default mpv template is `--really-quiet --force-media-title=*TITLE* --start=*START* --stream-lavf-o-append=request_size=10485760 --no-ytdl *VIDEO*` with `--audio-file=*AUDIO*`. — [pipe-viewer bin/pipe-viewer](https://github.com/trizen/pipe-viewer/blob/HEAD/bin/pipe-viewer)

**YouTube changes in 2025–2026 and how yt-dlp copes**
- yt-dlp issue #12482 was opened 2025-02-26: for the WEB client, YouTube "has removed the playback links for `adaptiveFormats` in the player response". "SABR is a custom streaming protocol built by YouTube." The SABR PR #13515 is still shown as Open. — [yt-dlp #12482](https://github.com/yt-dlp/yt-dlp/issues/12482). In current code, yt-dlp writes "Some {client} client https formats have been skipped as they are missing a URL… YouTube is forcing SABR streaming for this client" for `web`/`web_safari`. — [yt-dlp _video.py](https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/youtube/_video.py)
- SABR uses YouTube's custom "UMP format". A JS library implements UMP/SABR for custom clients and players: LuanRT/googlevideo, v4.1.1, released 2026-07-13. — [LuanRT/googlevideo](https://github.com/LuanRT/googlevideo)
- Unofficial SABR test builds of yt-dlp exist (bashonly `sabr` pre-release 2026-08-19; TheDcoder fork 2026-09-28). They are not official releases. — [bashonly/yt-dlp sabr](https://github.com/bashonly/yt-dlp/releases/tag/sabr), [TheDcoder/yt-dlp sabr](https://github.com/TheDcoder/yt-dlp/releases/tag/sabr)
- yt-dlp 2025.11.12: "An external JavaScript runtime is now required for full YouTube support… (e.g. Deno)… to solve the JavaScript challenges presented by YouTube" (issue #15012). — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- EJS wiki (edited 2026-07-12):
  - Minimum runtimes are Deno 2.3.0 (recommended, the only one "Enabled by default"), Node 22.0.0, Bun 1.2.11–1.3.14 (deprecated) and QuickJS 2023-12-9 (all QuickJS-NG versions).
  - Standalone yt-dlp binaries bundle yt-dlp-ejs. pip installs need `yt-dlp[default]`.
  - `--remote-components ejs:npm|ejs:github` fetches the scripts remotely.
  — [yt-dlp wiki EJS](https://github.com/yt-dlp/yt-dlp/wiki/EJS)
- yt-dlp 2026.06.09 raised the minimums to Deno v2.3.0 and Node v22, and limited Bun to 1.2.11–1.3.14. — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- README (master), `player_client` defaults: "By default, `visionos,web` is used. If no JavaScript runtime/engine is available, then `web` is omitted." With logged-in cookies the defaults are `web_embedded,tv_downgraded,web` for free accounts and `web_creator,tv_downgraded,web` for Premium. — [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md). yt-dlp 2026.08.19 added the `visionos` client and removed `android_vr` from the default clients. — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- yt-dlp's deno sandbox [local test 2026-10-09]: `deno run --ext=js --no-code-cache --no-prompt --no-remote --no-lock --node-modules-dir=none --no-config --no-npm --cached-only -`. The README's `youtube-ejs:jitless` arg "Provides better security… Do note that `node` and `bun` are still considered insecure." — [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md)
- PO Token Guide (edited 2026-07-12):
  - Per client: `web` needs PO tokens for subs and GVS and has "Only SABR formats". `web_safari` needs GVS, but its HLS formats "don't currently need a GVS token". `mweb` needs GVS. `tv` needs none, but all formats are DRM'd without cookies. `web_embedded` needs none but only works for embeddable videos. `android_vr` needs none, but "made for kids" videos are unavailable. `ios`/`android` need GVS or Player tokens.
  - The recommended setup is a PO Token Provider plugin (bgutil-ytdlp-pot-provider or yt-dlp-getpot-wpc) with `mweb`.
  - Tokens may last "as short as 12 hours" or "several months". Premium accounts don't need GVS tokens.
  — [yt-dlp wiki PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide)
- [local test 2026-10-09] The yt-dlp debug log reported "Detected experiment to bind GVS PO Token to video ID for web client". — consistent with the [PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide) note that most web GVS/Player tokens are video-ID bound.

**Features: audio-only, background, resume, chapters, subtitles, quality, hwdec**
- Audio-only: `--vid=no` makes ytdl_hook pick `bestaudio/best` ([ytdl_hook.lua](https://github.com/mpv-player/mpv/blob/master/player/lua/ytdl_hook.lua)). ytsub runs its audio sessions with `--idle=yes --vid=no` and detaches video sessions with `--idle=once`. — [ytsub src/mpv/mod.rs](https://github.com/sarowish/ytsub/blob/HEAD/src/mpv/mod.rs)
- Resume: `--save-position-on-quit`, `--resume-playback` and `--watch-later-dir` exist. Watch-later filenames "are hashed from the full paths of the media files". `--no-config` blocks "resume playback files and cache files". — [options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst), [mpv.rst FILES](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/mpv.rst)
- Per-file start: the `loadfile` 4th argument takes `start=...` (verified in the local IPC test above), and so does `--start`. — [input.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/input.rst)
- Chapters: `chapter-list` property ([input.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/input.rst)). ytsub feeds YouTube chapters to mpv through `--chapters-file` when using direct URLs. — [ytsub src/mpv/mod.rs](https://github.com/sarowish/ytsub/blob/HEAD/src/mpv/mod.rs)
- Subtitles: ytdl_hook asks yt-dlp for `--sub-langs all --write-srt` unless the user restricts languages. Master maps ass/srt and can map `srv3` when `subrandr` is present. — [ytdl_hook.lua master](https://github.com/mpv-player/mpv/blob/master/player/lua/ytdl_hook.lua). The `sub-add` command or `--sub-file` handles direct caption URLs. — [input.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/input.rst)
- Quality: `all_formats` can expose every yt-dlp format as delay-loaded tracks, but "it's not suitable for this purpose [runtime switching]… It's slow". — [options.rst v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst). Master now defaults `all_formats=yes`. — [ytdl_hook commits](https://github.com/mpv-player/mpv/commits/master/player/lua/ytdl_hook.lua)
- hwdec: "Hardware decoding is not enabled by default". `hwdec=auto` is recommended to try first; Ubuntu's `/etc/mpv/mpv.conf` sets `hwdec=vaapi`. — [options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst). Since 0.40.0, `--hwdec=auto` behaves like `auto-safe`. — [interface-changes.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/interface-changes.rst). In 0.41.0, vd_lavc prefers Vulkan hwdec and non-copy hwdec. — [v0.41.0 release](https://github.com/mpv-player/mpv/releases/tag/v0.41.0)

### Inferences
- **Recommended architecture:** spawn `mpv` per session and talk newline-delimited JSON yourself. The protocol is small enough that a ~200–300-line tokio client avoids GPL-3.0 crates like `mpvipc` and stale ones. On Unix, prefer `--input-ipc-client=fd://N` with a `socketpair()`. Then there is no filesystem socket, no other process can connect, mpv dies when tuitube drops the FD, and the macOS 104-byte path limit doesn't apply. On Windows, use a per-session random `\\.\pipe\tuitube-<random>`: mpv already restricts that pipe to the current user and rejects remote clients. If a path socket is used on Unix, put it in a 0700 per-user dir (`$XDG_RUNTIME_DIR` or a private dir under `TMPDIR`), keep it short, never use `@` abstract names on Linux (they have no filesystem permissions), and remove it on exit.
- **Two playback paths, kept swappable:**
  - (a) *Resolver in tuitube, direct URLs to mpv.* tuitube resolves formats (yt-dlp `-J` or an InnerTube library) and then passes `--ytdl=no` plus the video URL, `--audio-file` (or `audio-add`), subs and chapters. This gives tuitube full control of quality (itag picking), titles, chapters and resume. It never hands mpv a site URL, and it lets tuitube cache metadata. Re-resolve on 403 or after about 6 h, and re-resolve after IP changes such as VPN or network switches.
  - (b) *ytdl_hook as a fallback.* This is simpler and gets yt-dlp's client and PO-token logic for free, but tuitube has less control and must pin `ytdl_path` and pass `ignore-config=`.
  - Both paths break together if YouTube makes every non-PO-token client SABR-only. The HLS (`m3u8_native`) formats are the most likely surviving fallback, since the PO guide says web_safari HLS currently needs no GVS token.
- Always pass a `--ytdl-format` or explicit itags. In the test, yt-dlp's default picked AV1 2160p, which many machines cannot hardware-decode. A sane default is something like `bv*[height<=1080][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height<=1080]+ba/b`, with the cap configurable. Pass `--hwdec=auto` explicitly, because `--no-config` discards the user's setting.
- Do resume in tuitube, not mpv: observe `time-pos`, store it per video ID in tuitube's DB, and pass `start=` on `loadfile`. mpv's watch_later hashes the full path. That never matches direct googlevideo URLs, which change on every resolve, and `--no-config` disables it anyway.
- Detect the mpv version at connect time (`get_property mpv-version`) and build `loadfile` per version. For mpv < 0.38 (for example Ubuntu 24.04's 0.37), options go in the 3rd argument. Alternatively, avoid per-file options entirely and use `set_property` before `loadfile`.

### Gaps
- No primary-source statement on when or whether YouTube will make `visionos`/`tv_downgraded`/`mweb` SABR-only. The yt-dlp SABR PR merge date is unknown.
- I did not test playback **with cookies/login** or with a PO-token provider plugin, and I did not test age-restricted or members-only videos.
- Whether libmpv can open its own native video window on macOS from a non-main-thread Rust TUI (Cocoa main-thread constraints) was not verified. Check before choosing libmpv.

## Q2. Isolating mpv from user config and scripts: which options, what mpv loads by default, and how a client should launch it

### Takeaway
`--no-config` alone already blocks mpv.conf, input.conf, user scripts and resume/cache files (verified locally). The built-in Lua scripts (OSC, console, stats, select, positioning, context menu, auto-profiles) and the ytdl_hook still load unless turned off one by one. yt-dlp reads its own config and plugins unless told not to. The CLI equivalent of libmpv's locked-down profile is the flag list under Inferences.

### Cited Findings
- `--no-config`: "Do not load default configuration or any user files. This prevents loading of both the user-level and system-wide mpv.conf and input.conf files. Other user files are blocked as well, such as resume playback files and cache files. This option only takes effect when used as a command line flag." Files requested explicitly (`--include`, `--use-filedir-conf`) still load. — [options.rst v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- `--config-dir=<path>` forces one config dir and ignores `MPV_HOME`, but "cache and state paths… keep their auto-detection logic", and "`--no-config` takes precedence". — [options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- [local test 2026-10-09] An `MPV_HOME` was set up with `scripts/probe.lua` (logs a marker) and `mpv.conf` (`volume=37`). With defaults, the script loaded and volume was 37. With `--no-config`, the script did not load and volume was 100. With `--config-dir=<empty dir>`, likewise neither the script nor the conf applied.
- Default user files (Linux): `/etc/mpv/mpv.conf` (system-wide; `/usr/local/etc/mpv` by default prefix), `~/.config/mpv/{mpv.conf,input.conf,fonts.conf,subfont.ttf,fonts/}`, `~/.config/mpv/scripts/` ("All files in this directory are loaded as if they were passed to the `--script` option… alphabetical order"), `~/.config/mpv/script-opts/*.conf`, legacy `~/.mpv/`, and watch_later in `~/.local/state/mpv/watch_later/`. The XDG and `MPV_HOME` env vars override these. Windows uses `%APPDATA%/mpv/`, a `portable_config` folder next to mpv.exe, and a lower-priority config next to mpv.exe. macOS uses `~/.config/mpv/watch_later/` and `~/Library/Caches/io.mpv/`. — [mpv.rst FILES v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/mpv.rst)
- `--load-scripts=no` stops auto-loading the `scripts` subdirectory (default yes). The separate built-in toggles, all default yes except as noted, are `--load-stats-overlay`, `--load-console`, `--load-commands`, `--load-auto-profiles` (auto), `--load-select`, `--load-context-menu` (platform-dependent) and `--load-positioning`. `--osc` is also default yes. — [options.rst v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst); defaults confirmed via `mpv --no-config --list-options` [local test]
- `--input-default-bindings=no` disables the built-in "weak" bindings and scripts' `mp.add_key_binding` bindings, but not `mp.add_forced_key_binding`. `--input-builtin-bindings=no` stops loading the built-in bindings at start-up; it cannot be re-enabled later and "May be useful to libmpv clients". — [options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- `--load-unsafe-playlists` defaults to no. It loads URLs "which are considered unsafe… special protocols and anything that doesn't refer to normal files." — [options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- `--write-filename-in-watch-later-config` "may expose privacy-sensitive information". — [options.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/options.rst)
- `--terminal=no` / `--really-quiet` are recommended for terminal VOs because mpv output isn't synchronized with image output. — [vo.rst v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/vo.rst)
- yt-dlp isolation flags: `--ignore-config` ("Don't load any more configuration files except those given to --config-locations"), `--no-plugin-dirs` (clears plugin search dirs, including defaults), `--js-runtimes RUNTIME[:PATH]` / `--no-js-runtimes`, and `--no-remote-components`. — [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md)
- Real clients differ:
  - invidtui launches `mpv --idle --keep-open --no-terminal --really-quiet --no-input-terminal --user-agent=… --input-ipc-server=… --script-opts=ytdl_hook-ytdl_path=…`. It pins yt-dlp but does **not** pass `--no-config`, so user config and scripts apply. — [invidtui mediaplayer/mpv.go](https://github.com/darkhz/invidtui/blob/HEAD/mediaplayer/mpv.go)
  - ytsub launches `mpv --no-terminal --input-ipc-server=<endpoint>`, also without `--no-config`. — [ytsub src/mpv/mod.rs](https://github.com/sarowish/ytsub/blob/HEAD/src/mpv/mod.rs)

### Inferences
- Suggested launch for tuitube (argv only, never a shell):
  `mpv --no-config --load-scripts=no --osc=no --load-stats-overlay=no --load-console=no --load-commands=no --load-select=no --load-positioning=no --load-context-menu=no --load-auto-profiles=no --input-default-bindings=no --input-builtin-bindings=no --input-terminal=no --terminal=no --idle=yes --save-position-on-quit=no --resume-playback=no --hwdec=auto --ytdl=no|yes --script-opts=ytdl_hook-ytdl_path=<ABSOLUTE path> --ytdl-raw-options=ignore-config=,js-runtimes=deno:<abs path> --input-ipc-client=fd://N [-- <url>]`
- For a video window, the user may want keyboard control *inside* the mpv window. Consider keeping `--input-default-bindings=yes` (just the builtin bindings), or providing tuitube-defined bindings via a tuitube-owned `--input-conf=<file>`, rather than the user's input.conf.
- Offer an opt-in "use my mpv config" toggle (drop `--no-config` / `--load-scripts=no`) for power users who want their shaders or uosc. It should be off by default. Arbitrary user Lua/JS scripts run with mpv's privileges, and a script can bind to or observe everything tuitube does.
- `--no-plugin-dirs` would also disable yt-dlp PO-token provider plugins. Treat it as an explicit tradeoff: default to `ignore-config=` only, and make plugin loading a setting.
- Always put `--` before any URL argument to mpv and yt-dlp so a value starting with `-` can't be read as an option. ytdl_hook already does this for yt-dlp.

### Gaps
- I did not verify whether `--no-config` stops ytdl_hook from searching `~/.config/mpv/` for a `yt-dlp` binary. Pin `ytdl_path` to an absolute path regardless.
- Exact behaviour of `--input-builtin-bindings=no` combined with a video window on each OS was not tested.

## Q3. mpv availability and install story (macOS incl. Homebrew/IINA, Linux, Windows; x86_64 + ARM64) and minimum versions

### Takeaway
mpv 0.41.0 is current in Homebrew, Arch, Debian testing/unstable (14), Fedora 44, Ubuntu 26.04/26.10, Scoop, Chocolatey, MacPorts and nixpkgs. LTS distros lag: Ubuntu 22.04 has 0.34.1 and 24.04 has 0.37.0; Debian 12 has 0.35.1 and 13 has 0.40.0. Windows builds are third-party (shinchiro and zhongfly), including **aarch64** and **libmpv dev** packages. On macOS, `brew install mpv` pulls yt-dlp and deno with it. tuitube should require mpv ≥ 0.35 and handle the 0.38 `loadfile` change.

### Cited Findings
- mpv.io lists:
  - macOS: first-party CI builds ("intended mainly for testing and may be missing certain features"), stolendata builds, MacPorts, and "Homebrew (without application bundles)".
  - Windows: "All binary packages are unofficial third-party builds", from shinchiro and zhongfly, plus Scoop, Chocolatey and MSYS2.
  - Linux: "Distributions usually package outdated, unmaintained, and unsupported versions of mpv", especially Debian/Ubuntu, so the page recommends mpv-build or third-party packages.
  - The page does not mention IINA, WinGet, Flatpak or AppImage.
  — [mpv.io/installation](https://mpv.io/installation/)
- Homebrew (2026-10-09): formula `mpv` 0.41.0, whose dependencies include `yt-dlp`, `luajit`, `mujs`, `libplacebo`, `vulkan-loader` and `molten-vk`. Formula `yt-dlp` 2026.8.19 depends on `deno` and `python@3.14`. Formula `deno` is 2.9.7. Cask `iina` is 1.5.0. — [formulae.brew.sh/mpv](https://formulae.brew.sh/formula/mpv), [yt-dlp](https://formulae.brew.sh/formula/yt-dlp), [deno](https://formulae.brew.sh/formula/deno), [cask iina](https://formulae.brew.sh/cask/iina)
- Windows: the shinchiro daily release `20261009` contains `mpv-x86_64`, `mpv-x86_64-v3`, `mpv-i686` and **`mpv-aarch64`** archives, plus matching `mpv-dev-*` (libmpv) archives. — [shinchiro/mpv-winbuild-cmake releases](https://github.com/shinchiro/mpv-winbuild-cmake/releases). WinGet has a `shinchiro/mpv` manifest directory. — [winget-pkgs manifests/s/shinchiro/mpv](https://github.com/microsoft/winget-pkgs/tree/master/manifests/s/shinchiro/mpv)
- Distro versions (Repology, 2026-10-09):

  | Distro | mpv version |
  |---|---|
  | Ubuntu 22.04 | 0.34.1 |
  | Ubuntu 24.04 | 0.37.0 |
  | Ubuntu 25.04 / 25.10 | 0.40.0 |
  | Ubuntu 26.04 / 26.10 | 0.41.0 |
  | Debian 11 | 0.32.0 |
  | Debian 12 | 0.35.1 |
  | Debian 13 | 0.40.0 |
  | Debian 14 / unstable | 0.41.0 |
  | Fedora 40 | 0.37.0 |
  | Fedora 41 | 0.39.0 |
  | Fedora 42 / 43 | 0.40.0 |
  | Fedora 44 | 0.41.0 |
  | Alpine 3.22 / 3.23 | 0.40.0 |
  | Arch, Homebrew, Scoop, Chocolatey, MacPorts, nix unstable, openSUSE Tumbleweed | 0.41.0 |

  — [Repology mpv](https://repology.org/project/mpv/versions)
- Version milestones:
  - tct VO added in 0.22.0.
  - Integer `request_id` required in 0.30.0.
  - `--input-ipc-client` added in 0.33.0.
  - `--vo=kitty` and sixel alt-screen/buffered options added in 0.36.0.
  - `loadfile` index argument added in 0.38.0 (breaking).
  - `--hwdec=auto` now behaves like `auto-safe` as of 0.40.0.
  - `--vo-kitty-auto-multiplexer-passthrough` added in 0.41.0.
  — [interface-changes.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/interface-changes.rst)
- libmpv2 needs libmpv 2.0, which means mpv ≥ 0.35.0. — [libmpv2-rs README](https://github.com/kohsine/libmpv2-rs)
- mpv 0.41.0 requires FFmpeg 6.1+ and libplacebo 6.338.2+ to build. — [v0.41.0 release](https://github.com/mpv-player/mpv/releases/tag/v0.41.0)
- ytui-music's README shows the old install advice: `choco install mpv youtube-dl`, `brew install mpv youtube-dl`, and `apt install youtube-dl libmpv1 libmpv-dev`. All of it is outdated now that youtube-dl lags yt-dlp. — [ytui-music README](https://github.com/sudipghimire533/ytui-music)

### Inferences
- Minimum floor: mpv ≥ 0.35 (libmpv 2.0 era; Debian 12). Prefer ≥ 0.38 to get the modern `loadfile` signature. Also check yt-dlp ≥ 2025.11.12 (EJS) plus Deno ≥ 2.3.0, and in practice the newest yt-dlp, because YouTube breakage is fixed upstream within weeks. On first run, a `tuitube doctor` should print the versions of mpv, yt-dlp, deno/node and ffmpeg plus PATH resolution.
- Distro yt-dlp packages are often stale. Recommend the standalone yt-dlp binary, which bundles yt-dlp-ejs, or pipx `yt-dlp[default]`. Allow configuring absolute paths to `mpv`, `yt-dlp` and `deno`.
- Treat IINA as an mpv *frontend*, not a provider of a `mpv` CLI. Don't rely on it.

### Gaps
- Whether IINA's bundled CLI (`iina`/`iina-cli`) accepts mpv's `--input-ipc-server` pass-through was not verified.
- No primary source checked for Linux ARM64 (aarch64) mpv availability beyond distro packages (Arch Linux ARM, Debian arm64 and Fedora aarch64 presumably mirror the x86 versions above). This was not individually verified.

## Q4. Terminal-native playback (mpv `--vo=kitty/sixel/tct`) and in-process audio (symphonia/rodio/cpal)

### Takeaway
mpv can draw video inside the terminal (`kitty` since 0.36, `sixel`, `tct` half-blocks). It is software-scaled and the docs repeatedly suggest `--profile=sw-fast`, so it works as a novelty or preview mode, not as the main video path. In-process audio-only playback in Rust is feasible: symphonia decodes AAC-LC in MP4 ("Great") but has **no native Opus decoder**, so pick YouTube itag 140 (m4a AAC, about 128 kbps). This is what ytermusic does with rodio and symphonia-aac/isomp4; youtui also plays via rodio.

### Cited Findings
- `kitty` VO: "Graphical output for the terminal, using the kitty graphics protocol. Tested with kitty and Konsole. You may need to use `--profile=sw-fast`".
  - Layout options: `--vo-kitty-cols/rows/width/height/left/top`, `--vo-kitty-config-clear`, `--vo-kitty-alt-screen` (default yes) and `--vo-kitty-use-shm` ("much faster… not supported by as many terminals… not via e.g. SSH… not implemented on Windows").
  - `--vo-kitty-auto-multiplexer-passthrough` supports tmux and GNU screen.
  — [vo.rst v0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/vo.rst)
- `sixel` VO: "Tested with mlterm and xterm". Output is not synchronized, so `--really-quiet` is recommended. xterm needs `-ti 340` and limits images to 1000×1000. mpv must know the cell and pixel size, which is "an error-prone process which cannot be automated with certainty". Options include `--vo-sixel-*` cols/rows/left/top/pad/dither/reqcolors/buffered. — [vo.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/vo.rst)
- `tct` VO: "Color Unicode art… true color… 256-colors output is also supported". It defaults to half-blocks, "you may need to use `--profile=sw-fast`", and output "is not synchronized with other terminal output… `--terminal=no` or `--really-quiet` can help". — [vo.rst](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/vo.rst)
- Symphonia (README, 2026):
  - Demuxers: ISO/MP4 is "Great" (feature `isomp4`, not default); MKV/WebM is "Good" (default).
  - Decoders: AAC-LC is "Great" (feature `aac`, not default); **Opus is "-"**, meaning not implemented natively; HE-AAC is "-".
  - Third-party adapters: `symphonia-adapter-libopus` ("Opus via libopus") and `symphonia-adapter-fdk-aac`.
  — [Symphonia README](https://github.com/pdeljanov/Symphonia)
- Crate versions on crates.io (2026-10-09): symphonia 0.6.1 (2026-08-13, MPL-2.0), rodio 0.22.2 (2026-03-05), cpal 0.18.2 (2026-08-16). — [crates.io symphonia](https://crates.io/crates/symphonia), [rodio](https://crates.io/crates/rodio), [cpal](https://crates.io/crates/cpal)
- ytermusic's `player` crate uses `rodio = { version = "0.21.1", default-features = false, features = ["playback", "symphonia-aac", "symphonia-isomp4"] }`. Release beta 0.1.5 (2025-12-19) is "feat: switch to rodio". It caches downloads and plays offline. — [ytermusic crates/player/Cargo.toml](https://github.com/ccgauche/ytermusic/tree/HEAD/crates/player), [ytermusic releases](https://github.com/ccgauche/ytermusic/releases), [README](https://github.com/ccgauche/ytermusic)
- youtui plays via rodio (symphonia-all) on cpal. Its README says "Youtui uses the Rodio library for playback which relies on Cpal… ALSA development files are required". It downloads with a vendored `rusty_ytdl` or yt-dlp. — [youtui README](https://github.com/nick42d/youtui), [youtui Cargo.toml](https://github.com/nick42d/youtui/blob/HEAD/youtui/Cargo.toml)
- [local test 2026-10-09] YouTube audio formats observed: itag 140 is m4a `mp4a.40.2` at about 129.5 kbps; itag 251 is webm Opus at about 128.9 kbps. The URLs expire in 6 h and are IP-bound (see Q1).

### Inferences
- **Terminal video:** mpv's terminal VOs default to the alternate screen and redraw over everything. Running them while ratatui owns the same TTY means suspending the TUI: leave the alt screen and raw mode, hand the TTY to mpv, and restore on exit. The alternative is giving mpv an exact cell rectangle (`--vo-kitty-left/top/cols/rows`) with `--vo-kitty-alt-screen=no --vo-kitty-config-clear=no` and never painting that region, which is fragile with resizes. Expect high CPU (no hardware scaling path) and keep it as an opt-in "preview in terminal" mode. Use a real mpv window by default.
- **In-process audio** (reusing tuigram's cpal stack): use symphonia with `isomp4` and `aac` on itag 140. Write an HTTP range-reader `MediaSource` (reqwest with `Range:` headers) over the googlevideo URL, re-resolve when it expires, and use rodio or a cpal sink. Benefits: no mpv dependency for music/podcast use, gapless queue control, and media keys via souvlaki (as youtui does). Costs: seeking in DASH fMP4 over HTTP, Opus needs libopus (C dependency), no loudness normalization, and everything mpv gives for free (cache, reconnects, speed and pitch via rubberband) has to be rebuilt. A pragmatic plan is mpv `--vid=no` for audio-only in v1, with in-process audio as a later feature.

### Gaps
- No benchmark found of mpv `--vo=kitty`/`sixel` CPU or FPS at typical terminal sizes. Performance on Windows Terminal (sixel) and Ghostty or WezTerm (kitty protocol) was not tested.
- Symphonia `isomp4` seeking behaviour on fragmented-MP4 DASH segments from googlevideo was not verified.

## Q5. Survey of terminal YouTube clients (status as of 2026-10-09)

### Takeaway
The healthy, current terminal clients in 2026 are mostly Rust. **ytsub** (local InnerTube, sqlite, mpv over IPC, Takeout import) is the closest architectural model for tuitube. youtube-tui moved from Invidious to RustyPipe. ytermusic and youtui serve YouTube Music. invidtui, ytfzf, mov-cli, straw-viewer and ytui-music are dormant or abandoned. Invidious-only clients aged badly. Every client delegates playback to mpv (CLI or libmpv) or to rodio for audio. Login, where offered, means pasting browser cookies or using a TV/limited-input OAuth flow.

### Cited Findings

| Client | Lang | Last release (date) / last commit | Data backend | Player | Auth | Notes / sources |
|---|---|---|---|---|---|---|
| **youtube-tui** (Siriusmart) | Rust (GPL-3.0+) | v0.9.4 (2026-03-17); HEAD Cargo 0.9.5; commit 2026-05-29 | RustyPipe (InnerTube) by default since v0.9.0 (2025-07-31); Invidious re-added in v0.9.3 (2025-10-24), but "I do not have access to any running instances" | Launches external `mpv` for video; embedded libmpv (libmpv-sirno) audio player | None; local subscriptions, history, offline library | YAML config with `env: video-player: mpv, terminal-emulator: konsole -e`. Images via viuer sixel/halfblock. Vim-like commands. README TODO: "[URGENT] Replace the no-longer-going-to-compile `typemap` dependency… urgent for over 2 years". — [README](https://github.com/Siriusmart/youtube-tui), [Cargo.toml](https://github.com/Siriusmart/youtube-tui/blob/HEAD/Cargo.toml), [v0.9.0](https://github.com/Siriusmart/youtube-tui/releases/tag/v0.9.0), [v0.9.3](https://github.com/Siriusmart/youtube-tui/releases/tag/v0.9.3), [v0.9.4](https://github.com/Siriusmart/youtube-tui/releases/tag/v0.9.4) |
| **ytsub** (sarowish) | Rust (GPL-3.0) | v0.11.0 (2026-08-28); commit 2026-10-05 | `ApiBackend::Local` (default: direct `youtubei/v1/player`, `/browse`, `/navigation/resolve_url`) or Invidious, switchable at runtime | mpv via IPC (`--no-terminal --input-ipc-server`); direct formats (`--no-ytdl --audio-file --sub-file --chapters-file`) or yt-dlp | None; sqlite DB | Subscriptions-only. Imports Google Takeout `subscriptions.csv` and NewPipe exports. Thumbnails via kitty, sixel or iTerm protocols with ueberzugpp/chafa fallback. IPC endpoint is `temp_dir()/ytsub-mpv-<pid>-<n>.sock` or `\\.\pipe\ytsub-mpv-<pid>-<n>`. — [README](https://github.com/sarowish/ytsub), [src/api/mod.rs](https://github.com/sarowish/ytsub/blob/HEAD/src/api/mod.rs), [src/api/local.rs](https://github.com/sarowish/ytsub/blob/HEAD/src/api/local.rs), [src/mpv/mod.rs](https://github.com/sarowish/ytsub/blob/HEAD/src/mpv/mod.rs), [Cargo.toml](https://github.com/sarowish/ytsub/blob/HEAD/Cargo.toml) |
| **ytermusic** (ccgauche) | Rust | beta 0.1.5 (2025-12-19); commit 2026-05-13 | YouTube Music internal API (own `ytpapi2` crate); optional `rusty_ytdl` download backend | rodio + symphonia AAC/MP4; downloads and caches | Paste browser `Cookie` header + User-Agent into `headers.txt`; optional brand-account `account_id.txt` | About 20 MB RAM claimed; offline playback of cached tracks. — [README](https://github.com/ccgauche/ytermusic), [releases](https://github.com/ccgauche/ytermusic/releases), [Cargo workspace](https://github.com/ccgauche/ytermusic/blob/HEAD/Cargo.toml) |
| **youtui** + ytmapi-rs (nick42d) | Rust (MIT) | youtui v0.0.39 and ytmapi-rs v0.3.3 (2026-08-24) | ytmapi-rs (YouTube Music internal API) | rodio (symphonia-all) / cpal; native downloader or yt-dlp | `cookie.txt` from browser, or OAuth using the user's own Google Cloud "TVs and Limited Input devices" client; optional `po_token.txt` | Uses ratatui-image 10.0.8 for art and souvlaki for media keys. — [README](https://github.com/nick42d/youtui), [youtui/Cargo.toml](https://github.com/nick42d/youtui/blob/HEAD/youtui/Cargo.toml) |
| **ytui-music** (sudipghimire533) | Rust | v1.0.0-beta2 (2023-08-12); commit 2025-03-03 | Invidious (`config/src/invidious_servers.list`) | libmpv + youtube-dl | None | README: libmpv crate unmaintained, so build from source. Effectively dormant. — [README](https://github.com/sudipghimire533/ytui-music), [invidious_servers.list](https://github.com/sudipghimire533/ytui-music/blob/HEAD/config/src/invidious_servers.list) |
| **invidtui** (darkhz) | Go (MIT) | v0.4.6 (2024-07-14); commit 2024-07-14 | Invidious API; "automatically queries… and selects the best instance" | mpv via IPC (Go mpvipc) | Invidious account auth (feed, playlists, subscriptions) | Dormant for over 2 years; depends on Invidious instance health. — [README](https://github.com/darkhz/invidtui), [mediaplayer/mpv.go](https://github.com/darkhz/invidtui/blob/HEAD/mediaplayer/mpv.go) |
| **ytfzf** (pystardust) | POSIX sh | V2.6.2 "FINAL" (2024-01-31); commit 2024-09-27 | Scrapes YouTube or Invidious with curl + jq ("without API") | mpv (default), yt-dlp for downloads | None | README: "This project is no longer actively maintained". Thumbnails via ueberzugpp, chafa and others. Calls mpv with `$(eval echo "$url_handler_opts")` (eval of user-config values). — [README](https://github.com/pystardust/ytfzf), [ytfzf script](https://github.com/pystardust/ytfzf/blob/HEAD/ytfzf) |
| **yewtube** (fork of mps-youtube) | Python | v2.13.1 (2026-03-04) | `yewtube-search-python` (scraper) + `yt-dlp>=2023.9.24` | mplayer, mpv or VLC | None ("No Youtube API Key required") | Local playlists, downloads, comments. The `mps-youtube/mps-youtube` feed returns identical releases (appears renamed/redirected). — [README](https://github.com/mps-youtube/yewtube), [requirements.txt](https://github.com/mps-youtube/yewtube/blob/HEAD/requirements.txt), [CHANGELOG](https://github.com/mps-youtube/yewtube/blob/HEAD/CHANGELOG.md) |
| **pipe-viewer** (trizen) | Perl | 0.5.8 (2026-06-15); commit 2026-09-24 | "parses the YouTube website directly and relies on the invidious instances only as a fallback" | mpv with `--no-ytdl` + `--audio-file` (direct URLs); VLC; yt-dlp optional | None | Also ships a GTK GUI. Builds shell command strings with `quotemeta` escaping. — [README](https://github.com/trizen/pipe-viewer), [bin/pipe-viewer](https://github.com/trizen/pipe-viewer/blob/HEAD/bin/pipe-viewer) |
| **straw-viewer** (trizen) | Perl | 0.1.3 (2021-02-23); commit 2021-06-21 | Invidious | mpv | None | Abandoned; superseded by pipe-viewer. — [repo](https://github.com/trizen/straw-viewer) |
| **yt-x** (Benexl) | Shell | v0.8.7 (2026-10-01) | yt-dlp | (yt-dlp/mpv-style launcher) | None | fzf/rofi launcher with previews and colon search filters (`:today`, `:4k`, `:newest`…). — [README](https://github.com/Benexl/yt-x) |
| **mov-cli** | Python | 4.4.19 (2025-03-10); commit 2025-10-05 | Plugins (`mov-cli-youtube`) | mpv (default) | — | README: "mov-cli is in an unmaintained state!" — [README](https://github.com/mov-cli/mov-cli) |
| **newsboat** + YouTube RSS | C++/Rust | r2.45 (2026-10-04) | Channel RSS: `youtube.com/feeds/videos.xml?channel_id=…` | User macro → mpv | None | 2.45 fixed two advisories (see Q7). — [newsboat](https://github.com/newsboat/newsboat) |

- [local test 2026-10-09] The YouTube channel RSS feed still works (HTTP 200, `text/xml`). Each feed has **15** `<entry>` items, and each entry carries `yt:videoId`, `yt:channelId`, title, `published`/`updated`, `media:thumbnail` (`hqdefault.jpg` 480×360), `media:starRating count` and `media:statistics views`. A wrong or unknown channel ID returns a 404 HTML page. — [example feed](https://www.youtube.com/feeds/videos.xml?channel_id=UCXuqSBlHAE6Xw-yeJA0Tunw)
- RustyPipe (the Rust InnerTube library behind youtube-tui) is on Codeberg. Its last release is v0.11.4 (2025-04-23), and outside PRs opened Aug–Sep 2026 had not visibly landed in a release. — [codeberg ThetaDev/rustypipe](https://codeberg.org/ThetaDev/rustypipe), [crates.io rustypipe](https://crates.io/crates/rustypipe)
- The `invidious` crate (Siriusmart, AGPL-3.0) was last updated 2025-05-09. — [crates.io invidious](https://crates.io/crates/invidious)

### Inferences
- Lessons for tuitube:
  1. **Don't hard-depend on Invidious.** youtube-tui abandoned it, invidtui and ytui-music died with it, and Invidious itself now needs a Deno "companion" for streams. Use local InnerTube (like ytsub) or yt-dlp, with RSS as a cheap, robust feed source for subscriptions. It gives 15 latest uploads per channel, with no auth and no JS challenges.
  2. **Keep the backend swappable** behind a trait. youtube-tui's v0.9.0 notes say it "Separated out the code for fetching video info… making it easier to add new backends".
  3. **Store subscriptions and history locally** (sqlite) with Takeout CSV and NewPipe import, as ytsub does.
  4. **Treat the player as a replaceable external process** and pin binary paths.
  5. Login is the least mature area. The cookie-paste UX (ytermusic, youtui) is fragile, and youtui's own-OAuth-client approach is heavyweight. Login is not needed for local subscriptions or history.
- ytsub's IPC endpoint uses `std::env::temp_dir()` with a predictable `<pid>-<n>` name. On Linux that is a shared `/tmp`. tuitube should prefer `--input-ipc-client=fd://` or a private 0700 directory.

### Gaps
- No primary source checked on the exact player invocation of yt-x or the mov-cli YouTube plugin.
- The newer Rust YouTube TUIs found were ytsub, youtui and ytermusic. A broader 2025–2026 search (crates.io keyword search, GitHub topic `youtube-tui`) was not done exhaustively because of time, and the GitHub API was rate-limited during research.

## Q6. GUI references (FreeTube, NewPipe/Tubular, LibreTube, Grayjay, SmartTube, Invidious/Piped): feature sets and privacy design

### Takeaway
The privacy-respecting GUI clients converge on the same feature set:
- account-less **local subscriptions, history and playlists**
- subscription **import/export** (Takeout CSV and NewPipe JSON)
- **SponsorBlock** and **DeArrow** (FreeTube, LibreTube; SmartTube has SponsorBlock)
- **Return YouTube Dislike** (LibreTube, Piped)
- background or audio-only playback
- a choice of a built-in extractor or a proxy backend (Invidious/Piped)

This is a good v1/v2 feature checklist for tuitube. Note Tubular has been discontinued and SmartTube suffered a supply-chain compromise in Dec 2025.

### Cited Findings
- **FreeTube** v0.25.3 Beta (2026-08-28):
  - "uses a built in extractor… The Invidious API can also optionally be used. FreeTube does not use any official APIs… Your subscriptions, playlists and history are stored locally on your computer and never sent out."
  - Features: subscribe without account, profiles, export/import subscriptions, SponsorBlock, DeArrow, mini player.
  - RYD is not listed in the README features.
  — [FreeTube README](https://github.com/FreeTubeApp/FreeTube)
- **NewPipe** v0.29.1 (2026-08-15):
  - Features: background audio "only loading the audio stream to save data", popup player, subscriptions "without logging into any account", history, local playlists.
  - It parses the website or uses internal APIs when official APIs are restricted.
  - v0.29.0 (2026-07-22) dropped Android 5 and introduced "compose multiplatform" and "Compose rewrite" PRs. NewPipe Extractor is at v0.26.5 (2026-08-15).
  — [NewPipe README](https://github.com/TeamNewPipe/NewPipe), [v0.29.0 release](https://github.com/TeamNewPipe/NewPipe/releases/tag/v0.29.0), [NewPipeExtractor](https://github.com/TeamNewPipe/NewPipeExtractor)
- **Tubular** (NewPipe fork): "This repo is discontinued… For alternatives, see: PipePipe… Morphe". Last release v0.28.4 (2026-03-09), last commit 2026-07-07. — [Tubular README](https://github.com/polymorphicshade/Tubular)
- **LibreTube** v32.1 (2026-08-20), nightly 2026-10-08:
  - Features: subscriptions, subscription groups, watch/search history, downloads, background playback, "User Accounts via Piped (optional)", SponsorBlock, ReturnYouTubeDislike, DeArrow.
  - It "only sends the minimum amount of data necessary".
  — [LibreTube README](https://github.com/libre-tube/LibreTube)
- **SmartTube** 32.56 stable (2026-09-23):
  - Android TV only. Features include SponsorBlock (skip only; you cannot submit segments), PiP, 8K/HDR and live chat.
  - Sign-in uses "one-time connection codes" that appear in Google Account as "YouTube TV" connections.
  — [SmartTube README](https://github.com/yuliskov/SmartTube)
- **Piped**:
  - No GitHub releases; last commit 2026-09-11.
  - Features: SponsorBlock, RYD via RYD-Proxy, login (server-side accounts), "No connections to Google's servers", locally saved preferences.
  — [Piped README](https://github.com/TeamPiped/Piped)
- **Invidious**:
  - Releases are date-versioned (v2.20260804.1, 2026-08-05).
  - "Invidious companion" (Deno) now handles "all the video stream retrieval from YouTube servers" (release-master 2026-09-19).
  — [iv-org/invidious](https://github.com/iv-org/invidious), [invidious-companion README](https://github.com/iv-org/invidious-companion)
- **Grayjay** (FUTO): community sources describe it as distributed under FUTO's "Source First" license (source-available, non-commercial), with a plugin system and desktop builds. These are forum posts; I found no primary FUTO page in this research. — [Lemmy discussion "What is Grayjay?"](https://lemmy.ndlug.org/post/1512189)
- Takeout import path as documented by ytsub: Google Takeout → "YouTube and YouTube Music" → only `subscriptions` → `Takeout/YouTube and YouTube Music/subscriptions/subscriptions.csv`. — [ytsub README](https://github.com/sarowish/ytsub)

### Inferences
- tuitube v1 privacy checklist:
  - local sqlite for subscriptions, history, playlists and watch positions
  - Takeout CSV and NewPipe import/export
  - SponsorBlock: skip by observing `time-pos` and sending `seek`, or by injecting `--chapters-file` chapters
  - DeArrow titles and thumbnails, RYD counts, all opt-in with per-feature toggles because each is a third-party network call that leaks video IDs
  - no Google login required
  - optional cookies only for features that need an account (watch later sync, members-only)
- Follow FreeTube's "two extractor choices" pattern (built-in vs proxy) and NewPipe's "only load audio stream in background mode".

### Gaps
- Primary-source details on Grayjay's 2026 status, its desktop release cadence and the exact license version were not obtained (no reachable primary page in this session).
- I did not check whether FreeTube or NewPipe added RYD in 2026, or the SponsorBlock status in upstream NewPipe 0.29.x.

## Q7. Known security issues and CVEs in these clients and their dependencies, and what they teach

### Takeaway
Real incidents fall into a few classes:
1. Untrusted metadata reaching a shell or terminal: the yt-dlp `--exec` and `--write-link` CVEs, mpv's 2026 terminal escape injection advisory, and newsboat OPML `exec:` URLs.
2. Untrusted import files: NewPipe backup deserialization RCE, newsboat OPML.
3. Binary search-path hijack: Anki's planted `yt-dlp.exe`, and Rust's own Windows `Command` fixes.
4. Supply chain: SmartTube's signing key and dev machine compromise.
5. Server-side access control: Invidious 2026 CVEs.

I found no published advisories for ytfzf, pipe-viewer, yewtube, youtube-tui, ytsub, FreeTube, LibreTube or Piped.

### Cited Findings
- **mpv terminal sequence injection** (GHSA-546v-22c3-7927, published 2026-05-12): mpv "prints media metadata (title tags, chapter names, etc.) to the terminal… A crafted media file with escape sequences in its metadata can inject sequences… spoofing terminal content and overwriting the clipboard", for example via OSC 52. The advisory lists affected "< 0.41.0" and patched "None", which is ambiguous as displayed. — [mpv GHSA-546v-22c3-7927](https://github.com/mpv-player/mpv/security/advisories/GHSA-546v-22c3-7927)
- **mpv CVE-2018-6360**: "mpv through 0.28.0 allows remote attackers to execute arbitrary code via a crafted web site, because it reads HTML documents containing VIDEO elements, and accepts arbitrary URLs in a src attribute without a protocol whitelist in player/lua/ytdl_hook.lua". — [NVD CVE-2018-6360](https://nvd.nist.gov/vuln/detail/CVE-2018-6360)
- **yt-dlp `--exec` injection**:
  - CVE-2023-40581 (Windows `%q`), CVE-2024-22423 (bypass of that fix), CVE-2025-54072 (Windows placeholder expansion).
  - In 2026.06.09: "Usage of vulnerable conversions (e.g. `%()s`) with the `--exec` option is an all-too-common pitfall… `--exec` now only allows safe conversions" (GHSA-69qj-pvh9-c5wg).
  — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md), [NVD CVE-2023-40581](https://nvd.nist.gov/vuln/detail/CVE-2023-40581), [NVD CVE-2024-22423](https://nvd.nist.gov/vuln/detail/CVE-2024-22423)
- **yt-dlp 2026 CVEs**:
  - CVE-2026-26331, `--netrc-cmd` command injection, fixed 2026.02.21.
  - Three fixed in 2026.06.09: CVE-2026-50019 (cookie leak with `--downloader curl`), CVE-2026-50023 (dangerous `.desktop`/`.url`/`.webloc` file creation) and CVE-2026-50574 (code execution via manifest downloads with aria2c; aria2c HLS/DASH support removed).
  - CVE-2026-55404, "Downstream command injection via improper sanitization of --write-link output", fixed 2026.07.04.
  — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md), [NVD CVE-2026-55404](https://nvd.nist.gov/vuln/detail/CVE-2026-55404)
- Earlier yt-dlp issues: CVE-2024-38519 (file-extension sanitization to prevent RCE), CVE-2023-46121 (Generic extractor proxy injection, MITM) and CVE-2023-35934 (cookie leak). — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- **Wrapper-library argument injection**: YoutubeDLSharp CVE-2025-43858, "an unsafe conversion of arguments allows the injection of a malicious commands" (versions 1.0.0-beta4 up to but not including 1.1.2). — [NVD CVE-2025-43858](https://nvd.nist.gov/vuln/detail/CVE-2025-43858)
- **Binary planting**: Anki CVE-2025-62185 — "a crafted shared deck can place a YouTube downloader executable in the media folder, and this is executed for a YouTube link in the deck. The executable name could be youtube-dl.exe or yt-dlp.exe". — [NVD CVE-2025-62185](https://nvd.nist.gov/vuln/detail/CVE-2025-62185)
- **Rust std on Windows**: Rust 1.58.0 (2022-01-13) stopped `Command` searching the current directory for executables on Windows. — [Rust 1.58.0 announcement](https://blog.rust-lang.org/2022/01/13/Rust-1.58.0/). CVE-2024-24576: `Command` "did not properly escape arguments of batch files on Windows", fixed in 1.77.2. — [Ubuntu CVE-2024-24576](https://ubuntu.com/security/CVE-2024-24576)
- **NewPipe CVE-2024-32876** (GHSA-wxrm-jhpf-vp6v, 2024-04-24): importing a malicious backup could lead to arbitrary code execution through Java Object Serialization. Affected 0.13.4 up to (not including) 0.27.0; fixed with a class allow-list in 0.27.0. — [NewPipe advisory](https://github.com/TeamNewPipe/NewPipe/security/advisories/GHSA-wxrm-jhpf-vp6v)
- **newsboat** (both published 2026-10-04, fixed in 2.45, affecting ≥ 2.10):
  - OPML import "does not properly sanitize the URLs… `exec:` and `filter:` URLs… make it execute provided commands" (GHSA-4m8m-vj3m-hfxx).
  - Podcast URLs with newline/tab characters could trick Podboat "into downloading other URLs and writing them at arbitrary paths" (GHSA-pvg7-gjxf-23c9).
  — [GHSA-4m8m-vj3m-hfxx](https://github.com/newsboat/newsboat/security/advisories/GHSA-4m8m-vj3m-hfxx), [GHSA-pvg7-gjxf-23c9](https://github.com/newsboat/newsboat/security/advisories/GHSA-pvg7-gjxf-23c9)
- **SmartTube supply-chain compromise** (reported around 2025-12-02):
  - The developer said: "My development environment was infected by unknown malicious software… a few builds may have been affected… Public keys may have been compromised".
  - Analysts found a hidden native library `libalphasdk.so` that fingerprinted devices and fetched config. Reports differ on the affected versions (30.43–30.47 vs 30.51 and others).
  - A new key and app ID were issued.
  — [SmartTube README](https://github.com/yuliskov/SmartTube), [BleepingComputer](https://bleepingcomputer.com/news/security/smarttube-youtube-app-for-android-tv-breached-to-push-malicious-update), [PCWorld](https://www.pcworld.com/article/2997507/malware-found-in-popular-smarttube-app-on-smart-tvs-heres-what-to-do-about-it.html), [Slashdot 2025-12-02](https://tech.slashdot.org/story/25/12/02/1924229)
- **Invidious 2026**: CVE-2026-57946 (unauthenticated access to private playlist contents via the RSS playlist endpoint; fixed in 2.20260626.0) and CVE-2026-58447 (authenticated users could delete videos from other users' playlists; fixed in commit 77ad416). — [NVD CVE-2026-57946](https://nvd.nist.gov/vuln/detail/CVE-2026-57946), [NVD CVE-2026-58447](https://nvd.nist.gov/vuln/detail/CVE-2026-58447)
- Published advisory pages checked on 2026-10-09 showed **none** for FreeTube, Invidious, LibreTube, ytfzf, Piped, SmartTube, pipe-viewer, yewtube, youtube-tui and ratatui-image. — e.g. [FreeTube advisories](https://github.com/FreeTubeApp/FreeTube/security/advisories), [ytfzf advisories](https://github.com/pystardust/ytfzf/security/advisories)
- Defensive patterns in existing code:
  - mpv's ytdl_hook passes `--` before the URL to yt-dlp. — [ytdl_hook.lua](https://github.com/mpv-player/mpv/blob/master/player/lua/ytdl_hook.lua)
  - pipe-viewer builds shell strings but `quotemeta`s URLs and file names. — [bin/pipe-viewer](https://github.com/trizen/pipe-viewer/blob/HEAD/bin/pipe-viewer)
  - ytsub passes the title as a single argv element (`--force-media-title=<title>`) with no shell. — [ytsub src/mpv/mod.rs](https://github.com/sarowish/ytsub/blob/HEAD/src/mpv/mod.rs)
- [local test 2026-10-09] mpv printed the full signed googlevideo URL (IP-bound, with signatures) to its terminal output when it was used as `--audio-file`.

### Inferences
- Rules for tuitube:
  1. **Never use a shell.** Use `tokio::process::Command` with argv and put `--` before URLs. Never use yt-dlp `--exec`/`--netrc-cmd`/`--write-link`. Never launch through `.cmd`/`.bat` shims on Windows (CVE-2024-24576); resolve to the real `.exe`.
  2. **Resolve and pin absolute binary paths** for mpv, yt-dlp and deno at startup. Show them in `tuitube doctor`, never search the CWD or a media/download dir, and pass `ytdl_path` / `--js-runtimes deno:<abs>` explicitly (lessons from the Anki CVE and Rust 1.58).
  3. **Sanitize every remote string before rendering** in ratatui: titles, channel names, descriptions, comments and chapter names. Strip C0/C1 controls, ESC, OSC/CSI/DCS and bidi overrides. Ratatui writes cell contents as-is, so the mpv OSC 52 advisory pattern applies to tuitube's own UI too. Run mpv with `--terminal=no` so mpv never writes YouTube metadata or signed URLs to the user's terminal.
  4. **Treat imports as untrusted data.** For Takeout CSV, NewPipe JSON and OPML: parse with strict schemas, accept only `https://www.youtube.com/...` or channel-ID patterns, and never accept special URL schemes (newsboat `exec:`) or deserialize code-bearing formats (NewPipe).
  5. **Lock down the mpv IPC channel** (`run` is exposed): use an fd-passed socketpair, or a private socket dir / current-user pipe.
  6. **Don't log** googlevideo URLs, cookies or PO tokens. Redact `sig`, `lsig`, `pot` and `ip` in debug logs.
  7. **Release hygiene** (SmartTube lesson): sign releases, publish checksums, use reproducible CI builds, and keep signing keys off dev machines.
  8. If cookies are ever supported, store them in the OS keychain or with 0600 perms. Never pass them on the command line, where they are visible in `ps`. Prefer `--cookies-file` pointing at a 0600 temp file, or IPC `set_property`.

### Gaps
- Whether the mpv GHSA-546v-22c3-7927 fix has shipped (0.41.x or 0.42) was unclear from the advisory page ("patched: None").
- No exploit or advisory found specifically for title-based shell injection in ytfzf, ani-cli or yt-x. Their code was not audited beyond the ytfzf `eval` lines noted in Q5.

## Q8. Thumbnails in the terminal with ratatui-image; YouTube thumbnail formats and sizes

### Takeaway
ratatui-image is current: v11.1.0 stable on crates.io, v12.0.0-rc.2 tagged 2026-10-05, and the repo moved to the `ratatui` GitHub org. It supports kitty, sixel and iTerm2 protocols with a halfblocks fallback and terminal/font-size querying, as tuigram already uses. YouTube serves both JPEG (`/vi/`) and WebP (`/vi_webp/`) thumbnails at fixed sizes. WebP is about 45–56% smaller in testing. `maxresdefault` can 404 while still returning a 120×90 placeholder body, so check the HTTP status.

### Cited Findings
- ratatui-image does three things: "Query the terminal for available graphics protocols… Guess by env vars. If that fails, query the terminal with some control sequences. Fallback to 'halfblocks'"; query font size in pixels; and render. Sixel is "immediate-mode" and kitty is "stateful".
  - The `Image` widget is stateless and never blocks.
  - `StatefulImage` resizes at render time, and "The resizing and encoding is blocking". `thread::ThreadProtocol` is recommended for non-blocking resize.
  - Use `Picker::from_query_stdio()` to detect.
  — [ratatui-image README](https://github.com/ratatui/ratatui-image)
- Versions: crates.io `ratatui-image` max stable 11.1.0 (updated 2026-10-05, MIT, about 477k recent downloads); GitHub tags v12.0.0-rc.1 and rc.2 on 2026-10-05. — [crates.io ratatui-image](https://crates.io/crates/ratatui-image), [releases feed](https://github.com/benjajaja/ratatui-image/releases)
- youtui uses `ratatui-image = 10.0.8` with features `image-defaults, crossterm`. — [youtui Cargo.toml](https://github.com/nick42d/youtui/blob/HEAD/youtui/Cargo.toml)
- ytsub's terminal support matrix:
  - Kitty protocol: kitty, Ghostty.
  - Sixel: foot, Contour, xterm (`-ti 340`), BlackBox, Windows Terminal.
  - Inline Images Protocol: WezTerm ("Also supports Sixel, but images seem to be misplaced sometimes"), Rio, mlterm.
  - It caches thumbnails under `~/.cache/ytsub/thumbnail`.
  — [ytsub README](https://github.com/sarowish/ytsub)
- [local test 2026-10-09] `i.ytimg.com` responses for one video (dQw4w9WgXcQ):

  | Name | JPEG `/vi/<id>/<name>.jpg` | WebP `/vi_webp/<id>/<name>.webp` |
  |---|---|---|
  | `default` | 120×90, 2,888 B | 1,954 B |
  | `mqdefault` | 320×180, 10,303 B | 6,686 B |
  | `hqdefault` | 480×360, 21,011 B | 10,404 B |
  | `sddefault` | 640×480, 31,029 B | (not fetched) |
  | `maxresdefault` | 1280×720, 65,324 B | 28,620 B |
  | `hq720` | 1280×720 | (not fetched) |

  For an old video (jNQXAC9IVRw), `maxresdefault.jpg` returned **HTTP 404 with a 120×90 JPEG body**, while `hqdefault.jpg` returned 200 at 480×360.
- [local test 2026-10-09] yt-dlp's `thumbnail` field for the test video was `https://i.ytimg.com/vi_webp/dQw4w9WgXcQ/maxresdefault.webp`. Its `thumbnails` list held 23 jpg and 19 webp candidates.
- hqdefault and sddefault are 4:3 with letterboxing; mqdefault and maxres are 16:9 without. hqdefault is the "safe fallback" that exists for nearly every video. — [dev.to: How YouTube thumbnail URLs work](https://dev.to/alexlv_cheng/how-youtube-thumbnail-urls-work-a-simple-guide-36i6)
- The YouTube RSS `media:thumbnail` is `hqdefault.jpg` 480×360 [local test, see Q5].

### Inferences
- For list rows, use `mqdefault` (320×180, 16:9, no letterbox, about 7–10 KB). For a detail pane, use `hq720` or `maxresdefault` with a fallback to `hqdefault`, cropping the letterbox. Prefer WebP to halve bandwidth. This needs the `image` crate's `webp` feature, which is on in ratatui-image's `image-defaults`; verify against the chosen feature set. Always check the HTTP status: a 404 placeholder decodes fine and would silently show a grey image.
- Cache thumbnails on disk keyed by video ID and size, decode and resize them off the UI task (ratatui-image `ThreadProtocol`), and limit concurrent fetches. Reuse tuigram's existing Picker detection code.

### Gaps
- I did not verify that ratatui-image 11.x's `image-defaults` includes WebP decoding, or what changed in the v12 RC API.
- I did not verify that `hq720` exists for all videos (it exists for the HD test video) or whether YouTube serves AVIF thumbnails anywhere.
