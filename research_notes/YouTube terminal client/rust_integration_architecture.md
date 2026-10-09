# Rust integration architecture for tuitube (data layer, processes, local state, mpv), as of 2026-10-09

Method note: crate versions and dates come from the crates.io API, queried 2026-10-09. "Local measurement" means a test run on 2026-10-09 on the author's Mac (macOS 27.2 arm64), using Homebrew yt-dlp 2026.08.19 (Python 3.14.7, yt-dlp-ejs 0.8.0, curl_cffi 0.16.2), Deno 2.9.6 and mpv 0.41.0. All requests were anonymous and few. tuigram and tuimeta were only read, never run.

## 1. Which data-layer architecture: (a) in-process Rust, (b) yt-dlp per request, (c) long-lived sidecar, (d) Invidious/Piped, (e) Data API v3

### Takeaway
yt-dlp should do the extraction, run as a separate process: (b) first, with (c) as the upgrade path. Rust in-process code should handle only the stable, simple parts: RSS feeds, thumbnails and local state. Three things decide it, plus one structural fact.
- **Licensing:** (a) fails. rustypipe, the only maintained full InnerTube crate, is GPL-3.0, which would make the tuitube binary GPL.
- **Maintenance:** rustypipe's last crates.io release was April 2025. yt-dlp shipped five stable releases in 2026 and publishes nightlies.
- **Updates without a tuitube release:** possible only when the extractor is a separate program, such as yt-dlp with `-U` or a package manager, or a sidecar.
- **Structure:** (d) means running a PostgreSQL, Crystal and Deno server stack for one user. (e) can't return streams, Watch Later or history. Its only real use would be a one-time subscription import.

### Cited Findings
**(a) In-process pure Rust**
- rustypipe 0.11.4: the latest crates.io release is dated 2025-04-23, and the license is **GPL-3.0**. The 0.11.x series ran from Feb to Apr 2025, MSRV 1.67.1. — [crates.io rustypipe](https://crates.io/crates/rustypipe)
- The rustypipe repo's latest commit is 2026-08-03, a license-identifier docs change. The newest release commit is "release rustypipe v0.11.4", dated 2025-04-23. — [Codeberg ThetaDev/rustypipe](https://codeberg.org/ThetaDev/rustypipe)
- Open rustypipe issues:
  - #75 "Please update the library" (2026-03-07)
  - #73 "Extraction error: Request contains an invalid argument" (2026-01-31)
  - #71 "channel_playlists returns no playlists and wrong info" (2025-12-02)
  - #68 "Recommendations for Rate Limiting?" (2025-10-11)

  — [Codeberg rustypipe issues](https://codeberg.org/ThetaDev/rustypipe/issues)
- rustypipe's features:
  - Player (streams, subtitles), video details with comments and recommendations, playlists, channels, ChannelRSS (feature `rss`), search with filters, suggestions, trending, a URL resolver.
  - Subscriptions and history behind the `userdata` feature.
  - Auth by OAuth (TV client) or browser cookies.
  - A cache file holding client versions, the deobfuscation JS and auth tokens. The README warns: "Never share the contents of the cache if you are using authentication."

  — [Codeberg rustypipe](https://codeberg.org/ThetaDev/rustypipe)
- rustypipe depends on `rquickjs ^0.9`, an embedded QuickJS that runs YouTube's deobfuscation JS in-process, along with `reqwest ^0.12`, `quick-xml ^0.37` (optional) and `fancy-regex`. — [crates.io rustypipe 0.11.4 dependencies](https://crates.io/crates/rustypipe/0.11.4/dependencies)
- PO tokens come from a separate CLI, `rustypipe-botguard`, found in PATH or set with `.botguard_bin(path)`. It is MIT, at 0.1.2 (2025-08-09), and embeds V8 via `deno_core`, `deno_web` and related crates. — [Codeberg rustypipe](https://codeberg.org/ThetaDev/rustypipe); [crates.io rustypipe-botguard](https://crates.io/crates/rustypipe-botguard)
- Other Rust YouTube crates are stale:
  - rusty_ytdl 0.7.4: last release 2024-08-10.
  - ytextract 0.11.2: 2023-01-29.
  - piped (SDK) 0.0.4: 2023-08-12.

  — crates.io ([rusty_ytdl](https://crates.io/crates/rusty_ytdl), [ytextract](https://crates.io/crates/ytextract), [piped](https://crates.io/crates/piped))
- Local measurement of raw InnerTube from curl, which has a non-browser LibreSSL TLS stack:
  - `POST /youtubei/v1/search`: 200 in 0.77–1.0 s, with roughly 760–800 KB of JSON, uncompressed.
  - `POST /youtubei/v1/browse` for a channel's videos: 200 in 0.35 s.
  - The search response mixed 15 `videoRenderer` items with 6 `lockupViewModel` items. That is evidence of renderer churn a hand-written parser must follow.

**(b) yt-dlp as a short-lived subprocess**
- yt-dlp releases: 2026.03.13, 2026.03.17, 2026.06.09, 2026.07.04 and 2026.08.19 (latest). Changes along the way:
  - 2026.08.19 added a `visionos` client, added `web_embedded` fallbacks, removed `android_vr` from the defaults and updated client versions.
  - 2026.06.09 raised the minimum JS runtimes to Deno 2.3.0 and Node 22+, and deprecated Bun.
  - 2026.07.04 raised the minimum *recommended* Python version to 3.11, since 3.10 reaches end of life in October 2026.

  — [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases)
- "`stable`… is often 'stale' and prone to external breakage". The `nightly` channel "is the **recommended channel for regular users**". `yt-dlp -U` updates release binaries, and `--update-to nightly` switches channel. "there is no verification done for binaries from different repositories", which implies the official ones are checked. — [yt-dlp README, UPDATE](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#update)
- Release files include GPG-signed `SHA2-256SUMS.sig` and `SHA2-512SUMS.sig`, with a published `public.key`. — [yt-dlp README, release files](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#release-files)
- yt-dlp is released under the **Unlicense** and requires Python 3.10 or later. — [PyPI yt-dlp](https://pypi.org/project/yt-dlp/)
- Since 2025.11.12, full YouTube support needs an external JS runtime plus the yt-dlp-ejs scripts. Deno is recommended and is the only runtime enabled by default; Node, QuickJS and Bun are opt-in "for security reasons". Without a runtime, extraction is "deprecated" and formats are limited. — [yt-dlp issue #15012](https://github.com/yt-dlp/yt-dlp/issues/15012); [GIGAZINE](https://gigazine.net/gsc_news/en/20251113-yt-dlp-required-deno-javascript-runtime)
- The yt-dlp-ejs scripts are bundled in official executables. The help text says `--remote-components` "is currently not needed if you are using an official executable". Deno itself is **not** bundled. — `yt-dlp --help` (2026.08.19); [README dependencies](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#dependencies)
- Official binary sizes for 2026.08.19:

  | Asset | Size |
  |---|---|
  | `yt-dlp_macos` (universal) | 35.4 MB |
  | `yt-dlp_linux` | 38.6 MB |
  | `yt-dlp_linux_aarch64` | 38.3 MB |
  | `yt-dlp_musllinux_aarch64` | 38 MB |
  | `yt-dlp.exe` | 17 MB |
  | `yt-dlp_arm64.exe` | 20.2 MB |
  | `yt-dlp` (zipimport, needs system Python) | 2.93 MB |

  — [yt-dlp 2026.08.19 assets](https://github.com/yt-dlp/yt-dlp/releases/tag/2026.08.19)
- Local measurement: the Deno binary is 129.8 MB, Bun 63.1 MB and mpv 4.5 MB (Homebrew arm64, dynamically linked, so libraries aren't counted).
- Local measurement of subprocess latency with `--ignore-config --no-plugin-dirs`:

  | Request | Wall time | CPU time |
  |---|---|---|
  | `-J` on one video | 1.40–2.19 s | 0.28–0.90 s user |
  | `-J --flat-playlist -I 1:30` on `@YouTube/videos` | 0.97 s (25.8 KB) | |
  | `-J --flat-playlist "ytsearch20:…"` | 1.10 s (23.4 KB) | |
  | `yt-dlp --version` | 0.19–0.23 s | |

  The verbose log showed `JS runtimes: deno-2.9.6`, "Solving JS challenges using deno", a Deno subprocess (`deno run … --no-remote … --cached-only -`), and "Detected experiment to bind GVS PO Token to video ID for web client".
- YouTube has been forcing SABR-only streaming for some clients. Formats without a direct URL are then skipped, a problem tracked in yt-dlp #12482. — [Mageia advisory MGAA-2025-0098](https://advisories.mageia.org/MGAA-2025-0098.html); [Mageia bug 34750](https://bugs.mageia.org/show_bug.cgi?id=34750)

**(c) A long-lived sidecar**
- yt-dlp's embedding guidance:
  - "Your program should avoid parsing the normal stdout… Instead, they should use options such as `-J`, `--print`…"
  - From Python: `YoutubeDL(opts).extract_info(url, download=False)`, then `ydl.sanitize_info(info)`, because the maintainers "do not guarantee the return value of `YoutubeDL.extract_info` to be json serializable".

  — [yt-dlp README, EMBEDDING YT-DLP](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#embedding-yt-dlp)
- Local measurement of one Python process that reuses one `YoutubeDL`:
  - `import yt_dlp`: 0.22 s; `YoutubeDL()`: 0.03 s.
  - `extract_info` for three videos in a row: 0.99 s, 1.74 s and 1.78 s.
  - Channel with `process=False`: 0.76 s.

  Compared with the subprocess numbers above, staying resident saves about 0.2–0.5 s per call. The rest is network and JS-challenge time.
- youtubei.js (LuanRT/YouTube.js):
  - **MIT**, 18.1.0 (2026-09-22). Earlier releases: 18.0.0 (2026-08-13), 17.2.0 (2026-06-24), 17.0.1 (2026-03-16).
  - Dependencies: `fflate`, `meriyah`, `@bufbuild/protobuf`.

  — [npm youtubei.js](https://www.npmjs.com/package/youtubei.js)
- YouTube.js "does not include a built-in interpreter" for deciphering streaming URLs: the app must supply `Platform.shim.eval`. — [ytjs.dev getting started](https://www.ytjs.dev/guide/getting-started)
- PO-token generation for JS is `bgutils-js` 4.0.3 (MIT, 2026-08-04). — [npm bgutils-js](https://www.npmjs.com/package/bgutils-js)
- Go options:
  - `github.com/kkdai/youtube/v2`: latest v2.10.6 (2026-03-21).
  - `github.com/lrstanley/go-ytdlp`, a yt-dlp CLI wrapper: v1.5.4 (2026-09-19).

  — [proxy.golang.org kkdai/youtube](https://proxy.golang.org/github.com/kkdai/youtube/v2/@latest); [proxy.golang.org go-ytdlp](https://proxy.golang.org/github.com/lrstanley/go-ytdlp/@latest)
- tuimeta's precedent:
  - It spawns `tuimeta-helper --data-dir <dir>` with stdin/stdout piped, stderr to null and `kill_on_drop(true)`.
  - It waits for the `hello` line under `HELLO_TIMEOUT` and refuses a mismatched `PROTOCOL_VERSION`.
  - A writer task and a reader task do the I/O. Lines over `MAX_LINE` end the session ("sent too much"), and pending requests fail when the helper exits (`MetaEvent::Gone`).

  — [/Users/eric/projects/Personal/tuimeta/src/meta.rs](/Users/eric/projects/Personal/tuimeta/src/meta.rs) (lines ~486–1010)
- The earlier report's argument for a sidecar: the helper "can crash and restart while the TUI shows a status line, can be sandboxed on its own, and can be rebuilt and re-released by itself", and a "generic, documented, versioned protocol" strengthens licence separation. — [/Users/eric/projects/Personal/tuigram/reports/Meta messengers terminal client.md](/Users/eric/projects/Personal/tuigram/reports/Meta%20messengers%20terminal%20client.md), section "A separate helper process beats linking Go into the binary"

**(d) Invidious / Piped / invidious-companion**
- Self-hosted Invidious needs:
  - PostgreSQL (Docker uses `postgres:14`), "at least 20GB disk space, 2GB of free RAM", and 2.5 GB to compile.
  - invidious-companion, which replaces inv-sig-helper and the trusted-session generator; without it, "Playback won't work".
  - An `invidious_companion_key` that matches the companion's `SERVER_SECRET_KEY`.
  - Restarts: "Invidious **must** be restarted often, at least once a day, ideally every hour."

  — [docs.invidious.io installation](https://docs.invidious.io/installation/)
- invidious-companion is **AGPL-3.0**, built on youtube.js, and runs on Deno (`deno task compile` produces a single file). Deployment is by Docker or systemd. — [GitHub iv-org/invidious-companion](https://github.com/iv-org/invidious-companion)
- The companion left beta in Invidious v2.20250913.0. v2.20260207.0 "hardens the Invidious companion pipeline". These dates come from a third-party mirror and are unverified against the official page. — [git.psf.lt mirror v2.20250913.0](https://git.psf.lt/midou/invidious/src/tag/v2.20250913.0); [mirror compare to v2.20260207.0](https://git.psf.lt/midou/invidious/compare/ai-policy...release-v2.20260207.0)
- The Rust `invidious` crate 0.7.8 (2025-05-09) is **AGPL-3.0**. — [crates.io invidious](https://crates.io/crates/invidious)
- Public Piped instances are reported as unreliable, with the main instance "completely, unusably slow" in an undated anecdote. A 2026 blog recommends "self-hosted, behind Tailscale, for personal use". — [Tildes](https://Tildes.net/~tech/1bgo/anyone_else_have_horrible_user_experiences_with_piped); [sumguy.com 2026 status](https://sumguy.com/invidious-piped-redlib-nitter-2026/)

**(e) YouTube Data API v3**
- Quota: "a default quota allocation of 100 search.list calls, 100 videos.insert calls, and 10,000 units per day combined for all other endpoints". search.list and videos.insert "have their own quota buckets… The quota cost is 1 per call". subscriptions.list, playlistItems.list and videos.list cost 1 unit. subscriptions.insert, playlistItems.insert and videos.rate cost 50. The page was last updated 2026-10-08. — [Quota calculator](https://developers.google.com/youtube/v3/determine_quota_cost)
- Crates:
  - google-youtube3 7.0.0+20251222 (2026-01-01, MIT), built on hyper-rustls 0.27 with optional yup-oauth2 ^12.
  - yup-oauth2 12.1.2 (2026-01-07, MIT/Apache-2.0).

  — [crates.io google-youtube3](https://crates.io/crates/google-youtube3); [crates.io yup-oauth2](https://crates.io/crates/yup-oauth2)
- Under a "Testing" publishing status, refresh tokens expire after 7 days. — [Google setup guide (Health API doc stating the general OAuth rule)](https://developers.google.com/health/setup)
- Sensitive scopes need verification, typically 3–5 business days. — [Google sensitive scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/sensitive-scope-verification)
- Watch Later (`WL`) and history (`HL`) playlists return empty lists through the API. — [jdf76/plugin.video.youtube #199](https://github.com/jdf76/plugin.video.youtube/issues/199); [App::WatchLater](https://metacpan.org/pod/App::WatchLater) (secondary sources)

### Inferences
- **Comparison matrix:**

  | | (a) rustypipe in-process | (b) yt-dlp per request | (c) sidecar (Python + yt-dlp lib) | (c') sidecar (Deno/Bun + youtubei.js) | (d) self-hosted Invidious | (e) Data API v3 |
  |---|---|---|---|---|---|---|
  | Fix YouTube breakage without a tuitube release | No: bump crate, rebuild | Yes: `yt-dlp -U` / package manager | Yes if yt-dlp is pip-updatable; helper itself rarely changes | Must re-release helper (npm dep bump) | Yes (server update) | N/A (official, stable) |
  | Maintenance signal Oct 2026 | Last release Apr 2025; "Please update" issue | 5 stable releases in 2026 + nightlies | same as (b) | youtubei.js 18.1.0 Sep 2026 | companion active; restarts hourly-daily | stable but no streams |
  | Latency per call (measured) | ~0.35–1.0 s (raw InnerTube) | ~1.0–2.2 s | ~0.76–1.8 s | not measured | LAN hop + server | ~1 RTT |
  | Packaging | single binary | user installs yt-dlp + deno (or bundle 35–39 MB + 130 MB) | Python env or PyInstaller-style bundle + deno | `deno compile`/`bun build --compile` (~60–130 MB runtime) | a server | single binary |
  | Licence effect on tuitube | GPL-3.0 binary | none (separate program, Unlicense) | helper can be MIT (yt-dlp is Unlicense) | helper MIT possible | AGPL server, HTTP only | none |
  | Crash isolation | in-process (QuickJS in-process) | per request | helper restart | helper restart | remote | in-process HTTP only |
  | Sandboxable | no | yes (child, env cleared) | yes | yes (Deno permissions) | yes | n/a |
  | Account data | cookies/OAuth in cache file | cookies only | cookies only | cookies/OAuth | Invidious account | OAuth, quota, WL/HL empty |

- (b) and (c) differ mainly in **state**, not startup. A resident process can keep paging state, such as yt-dlp's lazy `entries` generator or InnerTube continuation tokens, so "next page" costs one request. A per-request subprocess with `-I 31:60` has to re-walk earlier pages. Startup itself is only ~0.2–0.5 s of a ~1–2 s call. This is an inference from the measurements and the CLI shape; page re-walking was not measured.
- Suggested shape: start with (b), behind a Rust `Backend` trait whose requests and events match a future NDJSON protocol. Then move to (c), a small Python helper (MIT, in its own folder, `--fake` mode like tuimeta) speaking that protocol, once pagination or comments make subprocess re-walking painful. Keep RSS, thumbnails, SQLite and mpv control in Rust.
- A hand-written MIT InnerTube client in Rust for search and browse metadata is feasible: one request, no JS for metadata. But it would mean tracking renderer churn alone (`videoRenderer` → `lockupViewModel`, seen locally). Streams still need n/sig solving and PO tokens, so it can't replace yt-dlp for playback.
- (e) is worth at most a one-off "import my subscriptions" via OAuth, and a personal Testing-mode project would force re-consent every 7 days. Exporting subscriptions to OPML or CSV by other means is simpler. That alternative was not researched; see Gaps.

### Gaps
- Whether rustypipe 0.11.4 still works against YouTube in October 2026 was not tested. youtube-tui 0.9.4 depends on it, but I found no breakage report beyond issues #73 and #75.
- No first-party Invidious 2026 release page was read (GitHub API rate-limited). The dates come from a mirror.
- youtubei.js sidecar latency, and the `deno compile` output size for such a helper, were not measured.
- Whether YouTube Takeout or another export path gives subscriptions as a file for import was not researched.

## 2. How Rust projects invoke yt-dlp; JSON stability; startup cost and amortization

### Takeaway
Rust projects drive yt-dlp as a child process and deserialize `-J`. Existing crates are either GPL (`yt-dlp`, which auto-downloads binaries) or stale (`youtube_dl`, last release April 2024), so tuitube should write a small `tokio::process` wrapper with lenient serde structs: `Option` fields, unknown fields ignored. yt-dlp promises machine-readable `-J` output, but not a frozen schema. A call costs about 1–2 s, mostly network and Deno challenge-solving; Python startup is only ~0.2 s.

### Cited Findings
- `yt-dlp` crate (boul2gom):
  - 2.8.3 (2026-08-17), **GPL-3.0-only**, with releases about every two weeks (2.8.0 on 2026-07-27, then 2.8.1, 2.8.2 and 2.8.3 weekly).
  - Described as "a Rust asynchronous wrapper around the yt-dlp command line tool". yt-dlp and ffmpeg "are downloaded automatically", with no stated source or verification.
  - Has a Moka L1 cache plus optional JSON/redb/Redis L2. It treats stream URLs as valid for about 6 h.
  - Dependencies include reqwest 0.13, tar, xz2, zip and lofty.

  — [GitHub boul2gom/yt-dlp](https://github.com/boul2gom/yt-dlp); [crates.io yt-dlp](https://crates.io/crates/yt-dlp)
- `youtube_dl` crate:
  - 0.10.0 (2024-04-16), MIT/Apache-2.0. "Runs yt-dlp and parses its JSON output", with model types "mostly auto-generated from the JSON output format of youtube-dl".
  - `YoutubeDl::new(url).socket_timeout("15").run()`. Optional tokio and reqwest support a `download_yt_dlp` helper.
  - Dependencies: serde, serde_json, wait-timeout.

  — [docs.rs youtube_dl](https://docs.rs/youtube_dl/latest/youtube_dl/); [crates.io](https://crates.io/crates/youtube_dl)
- youtube-tui 0.9.4 (2026-03-17, GPL-3.0-or-later):
  - Depends on `rustypipe ^0.11` (required), with optional `invidious ^0.7`, `libmpv-sirno` (embedded mpv) and `viuer`. Uses ratatui ^0.29.
  - Describes itself as "an _app launcher_" that hands videos to `video-player: mpv`. Config is YAML in `~/.config/youtube-tui`.

  — [crates.io youtube-tui 0.9.4 deps](https://crates.io/crates/youtube-tui/0.9.4/dependencies); [GitHub siriusmart/youtube-tui](https://github.com/siriusmart/youtube-tui)
- ytermusic (Apache-2.0):
  - Its own InnerTube crate `ytpapi2` (reqwest 0.11 with `cookies`, `rustls-tls`), driven by a browser `Cookie` and `User-Agent` pasted into `headers.txt`.
  - Downloads use `rusty_ytdl` (optional git dependency) and playback uses `rodio`.

  — [GitHub ccgauche/ytermusic](https://github.com/ccgauche/ytermusic); [ytpapi2/Cargo.toml](https://raw.githubusercontent.com/ccgauche/ytermusic/master/crates/ytpapi2/Cargo.toml); [download-manager/Cargo.toml](https://raw.githubusercontent.com/ccgauche/ytermusic/master/crates/download-manager/Cargo.toml)
- yt-dlp's output stability guidance:
  - "avoid parsing the normal stdout since they may change… use options such as `-J`, `--print`, `--progress-template`".
  - The output-template field list is not exhaustive: "all the fields… are not listed below. Use `-j` to see such fields".

  — [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#embedding-yt-dlp)
- Local measurement of the JSON shape (2026.08.19):
  - `-J` on a video returns 78 top-level keys and about 92 KB, including `formats` (24–49 entries), `requested_formats`, `chapters`, `heatmap`, `subtitles`, `automatic_captions`, `live_status`, `availability`, `channel_id`, `channel_follower_count`, `timestamp`, `duration` and `_version` (`{"version":"2026.08.19",…}`).
  - `--flat-playlist` channel entries carry only `_type, availability, channel_url, creators, duration, id, ie_key, live_status, thumbnails, timestamp, title, uploader_url, url, view_count` (plus `__x_forwarded_for_ip`).
- Flags relevant to a hardened invocation, from `yt-dlp --help` (2026.08.19):
  - `--ignore-config` (alias `--no-config`)
  - `--no-plugin-dirs` ("Clear plugin directories to search, including defaults")
  - `--js-runtimes RUNTIME[:PATH]` / `--no-js-runtimes`
  - `--no-remote-components` (remote components are off by default)
  - `--cache-dir DIR` (default `${XDG_CACHE_HOME}/yt-dlp`, which stores "client ids and signatures")
  - `--socket-timeout`, `--extractor-retries`, `--sleep-requests`
  - `--flat-playlist`, `-I/--playlist-items`, `--lazy-playlist`
  - `--write-comments`, with extractor args `youtube:max_comments=…` and `comment_sort=top|new`
  - `youtube:player_client`, which defaults to `visionos,web`

  — `yt-dlp --help`; [README extractor args](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#youtube)
- Plugins: "**all** plugins are imported even if not invoked, and… **there are no checks** performed on plugin code". Plugins also load from "pip and other locations in `PYTHONPATH`", and from `.zip`/`.whl` files in config dirs. — [yt-dlp README, PLUGINS](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#plugins)
- The default config files are portable (next to the binary), home (`-P` or the current directory), user (`${XDG_CONFIG_HOME}`, `${APPDATA}`, `~/yt-dlp.conf`…) and system (`/etc/yt-dlp.conf`…). — [yt-dlp README, CONFIGURATION](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#configuration)
- Extractor arg `youtube-ejs:jitless`: "Run supported Javascript engines in JIT-less mode… better security at the cost of performance… `node` and `bun` are still considered insecure." — [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md)
- Startup measurements are in section 1: `--version` 0.19–0.23 s, `import yt_dlp` 0.22 s, and per-request wall time with a subprocess vs in-process.

### Inferences
- A tuitube wrapper would spawn `yt-dlp` via `tokio::process::Command` with:
  - `env_clear()` plus `PATH`, `HOME`/`USERPROFILE` and `TMPDIR`. Clearing the environment drops `PYTHONPATH`-based plugins and any `PYTHON*` variables. tuimeta's helper similarly reads no environment besides `HOME`/`TMPDIR` ([PROTOCOL.md](/Users/eric/projects/Personal/tuimeta/helper/PROTOCOL.md)).
  - `current_dir` set to a private folder, so a "home" `yt-dlp.conf` in the current directory isn't read. `--ignore-config` covers this anyway.
  - The flags `--ignore-config --no-plugin-dirs --no-warnings --cache-dir <data>/yt-dlp-cache --socket-timeout 15 -J`, stdout capped at N MB, stderr captured into a ring buffer of error kinds, and `kill_on_drop(true)`, so a cancelled request kills the child.
- Deserialize into structs with `#[serde(default)]` and `Option<T>` everywhere, without `deny_unknown_fields`. Keep `_version.version` so the UI can show "yt-dlp 2026.08.19". Run every string through `text::clean`, since titles and descriptions are untrusted.
- Amortization options, cheapest first:
  - Cache video JSON with a TTL. Stream URLs last about 6 h per the yt-dlp crate's docs.
  - Keep `--cache-dir` persistent, so player JS and challenge results are reused.
  - Prefetch `-J` for the selected item while the user reads the list.
  - Move to a resident sidecar. That saves ~0.2–0.5 s per call and enables stateful pagination.
- Don't use the `yt-dlp` crate: it is GPL-3.0-only and auto-downloads unverified binaries. If tuitube bundles yt-dlp, follow tuigram's release pattern and pin a SHA-256 per target in CI ([tuigram release.yml](/Users/eric/projects/Personal/tuigram/.github/workflows/release.yml) pins TDLib zips). Alternatively, verify `SHA2-256SUMS` against yt-dlp's GPG `public.key`.

### Gaps
- Does `--no-plugin-dirs` also stop plugins found through `PYTHONPATH` or site-packages namespace packages? The README doesn't say. Clearing the environment covers `PYTHONPATH` but not a pip-installed plugin in yt-dlp's own environment.
- The cost of `--write-comments` with `max_comments` limits was not measured, nor was how yt-dlp pages comments.
- I found no yt-dlp statement promising `-J` schema stability across versions, only the "use `-J`" guidance.

## 3. Subscriptions without an account: channel RSS vs InnerTube browse

### Takeaway
Channel RSS (`/feeds/videos.xml?channel_id=UC…`) is still the cheapest account-free way to get the latest ~15 uploads per channel. As of October 2026 it is flaky: intermittent 500/404 responses, no ETag or Last-Modified, conditional GET ignored, Shorts mixed in, and the `UULF` Shorts-free trick now 404s. Build it with reqwest (rustls) + feed-rs, a small concurrency cap, a ≥15-minute cadence, retries with backoff, and a fallback to yt-dlp `--flat-playlist` (or InnerTube browse) when a channel's feed fails.

### Cited Findings
- Local measurement on 2026-10-09 of `https://www.youtube.com/feeds/videos.xml?channel_id=UCBR8-60-B28hp2BmDPdntcQ`:
  - First request: 200 in 0.46 s, 19.4 KB, 15 `<entry>` elements.
  - Headers: `content-type: text/xml; charset=UTF-8`, `cache-control: public, max-age=900`, an `expires` 15 minutes later, `server: YouTube RSS Feeds server`, and **no `ETag` and no `Last-Modified`**.
  - `If-Modified-Since` still got a full 200, not a 304.
  - Eight back-to-back requests returned `500 200 200 200 404 500 200 200`. A second channel (UC-lHJZR3Gqxm24_Vd_AJ5Yw) returned 500 once.
  - `playlist_id=UULF<id>` returned 404 for all three channels tried.
- Reports of the feeds breaking:
  - The OpenRSS post "YouTube, your feeds are broken" was discussed on HN around May 2026 (archive snapshot 2026-05-06). Points: Shorts clutter feeds, channel pages have no feed link, "a 404/500 _regularly_", failures "for a few hours at about the same time each day", and a top-comment workaround of swapping `UC`→`UULF` with `playlist_id`. — [HN item](https://hn.nuxt.dev/item/48030964); [OpenRSS post](https://openrss.org/blog/youtube-your-feeds-are-broken)
  - NewsBlur users saw 500s, and Techlore users saw 404s. — [NewsBlur forum](https://forum.newsblur.com/t/youtube-feeds-broken/13227); [Techlore](https://discuss.techlore.tech/t/suddenly-lost-access-to-youtube-rss-feeds/6963)
  - The TubeFeed add-on uses the RSS endpoint first and otherwise scrapes the `/videos` listing. — [TubeFeed](https://addons.mozilla.org/firefox/addon/tubefeed/)
- Crates:
  - feed-rs 3.0.0 (2026-09-27, MIT; quick-xml ^0.42, chrono, optional ammonia). The 2.x series ended at 2.4.0 (2026-07-07).
  - quick-xml 0.42.0 (2026-08-22, MIT).

  — [crates.io feed-rs](https://crates.io/crates/feed-rs); [crates.io quick-xml](https://crates.io/crates/quick-xml)
- rustypipe ships a ChannelRSS feature (`rss` crate feature), showing RSS is the conventional cheap path. — [Codeberg rustypipe](https://codeberg.org/ThetaDev/rustypipe)
- yt-dlp `--flat-playlist -I 1:30` on a channel's `/videos` tab took 0.97 s and returned `timestamp`, `duration`, `view_count` and `live_status` per entry (local measurement). Raw InnerTube browse took 0.35 s (local measurement).

### Inferences
- Design:
  - A `feeds` task fetches with up to 4–6 concurrent requests (`futures::stream::buffer_unordered`) through one shared `reqwest::Client` (HTTP/2, gzip).
  - It honours `max-age=900` as a floor: never refetch a channel within 15 minutes.
  - On startup it refreshes the stalest first, then works round-robin in the background, with a default cadence of 30–60 minutes for 100+ subscriptions.
  - It retries 5xx/404 with jittered backoff (2–3 tries). After N consecutive failures it falls back to yt-dlp `--flat-playlist -I 1:15` for that channel.
  - It stores the per-channel `last_ok`, `failures` and `next_due` in SQLite.
- Conditional GET (ETag/If-None-Match) gives nothing here, since no validators are sent. Hash the body (`siphasher`, already a feed-rs dependency) to skip re-parsing unchanged feeds.
- Filter Shorts with a cheap heuristic: entries whose `link` is `/shorts/`, or durations ≤60–180 s once known from a later `-J` or browse. The RSS entry carries no duration, so "hide Shorts" needs a browse or flat-playlist pass. The shorts heuristic is an inference.
- RSS entries carry `yt:videoId`, `published`, `updated`, `media:group` (thumbnail, description, `media:statistics views`), as seen in the local `feed.xml`. That is enough for a subscription feed list without any extractor call.

### Gaps
- I found no official YouTube statement about the RSS feeds' future. The daily-window outage pattern is anecdotal (HN comments).
- Rate limits for one residential IP fetching hundreds of feeds per hour were not measured.

## 4. Local state: SQLite vs TOML/JSON; permissions; atomic writes

### Takeaway
Keep `settings.toml` exactly as tuigram and tuimeta do (0600, written to `.toml.new` then renamed, in a 0700 data dir). Put subscriptions, the feed cache, watch-later, history and resume positions in one bundled SQLite file via rusqlite. Those are growing, queried, concurrently updated records where a whole-file TOML/JSON rewrite on every playback tick would be wasteful and fragile.

### Cited Findings
- rusqlite 0.40.2 (2026-08-08, MIT), with releases 0.38.0 (2025-12-20), 0.39.0 (2026-03-15), 0.40.0 (2026-05-26) and 0.40.1 (2026-06-06). — [crates.io rusqlite](https://crates.io/crates/rusqlite)
- tokio-rusqlite 0.8.0 (2026-09-06, MIT). — [crates.io tokio-rusqlite](https://crates.io/crates/tokio-rusqlite)
- tempfile 3.27.0 (2026-03-11); atomicwrites 0.4.4 (2024-09-19). — crates.io ([tempfile](https://crates.io/crates/tempfile), [atomicwrites](https://crates.io/crates/atomicwrites))
- tuimeta's `Settings::save`:
  - Serializes with `toml`, removes a stale `settings.toml.new`, and opens it with `create_new(true)` and mode `0o600` (Unix).
  - Writes, calls `sync_all()`, then `rename`s over `settings.toml`.

  — [/Users/eric/projects/Personal/tuimeta/src/settings.rs](/Users/eric/projects/Personal/tuimeta/src/settings.rs) (lines 101–117)
- tuigram's settings file can hold API keys, and its test asserts `mode & 0o777 == 0o600` ("it can hold API keys"). — [/Users/eric/projects/Personal/tuigram/src/settings.rs](/Users/eric/projects/Personal/tuigram/src/settings.rs) (lines 146, 206)
- tuigram `config.rs` handles the data dir:
  - `dirs::data_local_dir()/tuigram` (macOS `~/Library/Application Support/tuigram`, Linux `~/.local/share/tuigram`, Windows `%LOCALAPPDATA%\tuigram`), set to `0o700` on every start.
  - An override via `TG_DATA_DIR` is refused if it points at a UNC or another machine, and on Unix is checked by `private_place`: every ancestor must be owned by you or root, not writable by others unless root-owned and sticky, and on macOS group-writable counts as others because "every account is in `staff`".
  - The path is then canonicalized.
  - `.env` is read only in debug builds, only its `TG_*` keys, and never into the process environment.

  — [/Users/eric/projects/Personal/tuigram/src/config.rs](/Users/eric/projects/Personal/tuigram/src/config.rs)
- tuimeta repeats this with `TM_DATA_DIR`, and the helper gets `<data dir>/helper`, writing everything 0600. The Go helper sets `umask(0o077)` and points stderr at /dev/null. — [/Users/eric/projects/Personal/tuimeta/CLAUDE.md](/Users/eric/projects/Personal/tuimeta/CLAUDE.md); [/Users/eric/projects/Personal/tuimeta/helper/sys_linux.go](/Users/eric/projects/Personal/tuimeta/helper/sys_linux.go)
- tuimeta's helper keeps WhatsApp and Messenger-E2EE stores in modernc SQLite at 0600, "not encrypted at rest yet". — [/Users/eric/projects/Personal/tuimeta/CLAUDE.md](/Users/eric/projects/Personal/tuimeta/CLAUDE.md)

### Inferences
- Suggested layout in `<data>/tuitube/` (0700):
  - `settings.toml` (0600, theme and keys as in tuigram/tuimeta)
  - `tuitube.db` (0600; created with the umask set to 077 first, or the file pre-created with mode 0600 before `Connection::open`, then `PRAGMA journal_mode=WAL`, so the `-wal` and `-shm` files inherit the umask)
  - `yt-dlp-cache/`
  - `thumbs/` (an LRU disk cache)
  - `run/`, a private socket dir if needed
- Tables:
  - `channels(id TEXT PK, title, handle, added_at, feed_etag_hash, last_ok, failures, next_due)`
  - `videos(id TEXT PK, channel_id, title, published, duration, views, thumb_url, is_short, fetched_at)`
  - `watch_later(video_id PK, added_at, position)`
  - `history(video_id, watched_at, position_s, duration_s)`
  - `resume(video_id PK, position_s, updated_at)`
  - `kv(key PK, value)`
- Use `rusqlite` with feature `bundled`, which compiles SQLite, so there's no system-library dependency on any of the six targets. Do DB work on one dedicated thread, or `tokio-rusqlite`, so the event loop never blocks. Resume positions arrive from mpv `time-pos` observations; write them debounced, every ~5–10 s and on pause or stop.
- Offer an `:export`/`:import` of subscriptions as OPML or plain `channel_id` lines, so the user's data isn't locked in SQLite.
- All of this is design inference. None of the sources benchmarked SQLite against TOML for this use.

### Gaps
- I found no source comparing rusqlite `bundled` build times or binary-size cost on Windows ARM64. Expect roughly 1–1.5 MB added, but this was not measured.

## 5. Credential storage, if tuitube ever holds a token or cookie

### Takeaway
The recommended architecture needs **no credentials**: RSS plus local subscriptions, and anonymous yt-dlp. If cookies (for age-gated videos or account feeds) or an OAuth refresh token are added later, follow the earlier report's pattern:
- Wrap a random key with `keyring` (v4 is now `keyring-core` + per-platform store crates) and encrypt a 0600 file.
- Fall back to an in-memory passphrase or an explicit opt-in, never a fixed key.
- Hand secrets to a helper over its stdin, never via argv or the environment.

Today tuigram and tuimeta use 0600 files in a 0700 dir and no keyring at all.

### Cited Findings
- keyring 4.2.0 (2026-08-29, MIT/Apache-2.0, MSRV 1.88) is a front end over `keyring-core ^1.0.0` plus optional stores:
  - `apple-native-keyring-store` 1.0.2 (2026-08-06)
  - `windows-native-keyring-store` 1.1.0 (2026-05-24)
  - `zbus-secret-service-keyring-store` 1.0.1 (2026-08-15) and a dbus variant
  - `linux-keyutils-keyring-store` 1.0.0 (2026-04-21)
  - `db-keystore`, and an Android store

  The docs advise apps needing control to "be linking to the keyring-core library and any specific credential stores they want to use". — [docs.rs keyring](https://docs.rs/keyring/latest/keyring/); crates.io ([keyring](https://crates.io/crates/keyring), [keyring-core](https://crates.io/crates/keyring-core))
- keyring-core 1.0.0 (2026-04-21) offers `set_default_store` / `get_default_store`. "the underlying credential stores may not handle access to a single credential from different threads reliably." — [docs.rs keyring-core](https://docs.rs/keyring-core/latest/keyring_core/)
- The Linux keyutils store is "completely in-memory and will not persist across reboots". It is "strongly recommend[ed]" for headless Linux, with the advice to "Consider the keyring a secure cache" and re-prompt on miss. — [docs.rs linux-keyutils-keyring-store](https://docs.rs/linux-keyutils-keyring-store/latest/linux_keyutils_keyring_store/)
- tuigram keeps API keys in `settings.toml` (0600) and the TDLib session in the 0700 data dir. Its release binaries carry an XOR-masked built-in API key passed from CI secrets, never in the repo. — [/Users/eric/projects/Personal/tuigram/src/config.rs](/Users/eric/projects/Personal/tuigram/src/config.rs)
- tuimeta:
  - Login takes pasted cookies (masked input), checks the cookie names, and hands them to the helper, "which keeps it only in its data folder".
  - Session files are written 0600 (`internal/session/session.go`: "Save replaces name with data (0600, synced to disk)").
  - No source file in tuigram or tuimeta references `keyring` or Keychain (grep).

  — [/Users/eric/projects/Personal/tuimeta/CLAUDE.md](/Users/eric/projects/Personal/tuimeta/CLAUDE.md); [/Users/eric/projects/Personal/tuimeta/helper/internal/session/session.go](/Users/eric/projects/Personal/tuimeta/helper/internal/session/session.go)
- The earlier report recommended a random 32-byte store key wrapped by Keychain, DPAPI/Credential Manager or Secret Service via `keyring`, recording which backend wrapped it, passed over stdin. Without a keystore, an Argon2id passphrase or an explicit opt-in, and "never fall back to a fixed key". — [Meta messengers report](/Users/eric/projects/Personal/tuigram/reports/Meta%20messengers%20terminal%20client.md), section "A copied session store is a working clone of the account"
- rustypipe's README warns its cache holds auth tokens and cookies: "Never share the contents of the cache if you are using authentication". — [Codeberg rustypipe](https://codeberg.org/ThetaDev/rustypipe)
- ytermusic stores the browser `Cookie` header in a plain `headers.txt`, with no stated protection. — [GitHub ccgauche/ytermusic](https://github.com/ccgauche/ytermusic)

### Inferences
- YouTube cookies are full Google-account bearer tokens, the same class of target as the Meta cookies in the earlier report. If supported, accept them only by in-app paste (as tuimeta does), and pass them to yt-dlp as a `--cookies` file. Create that file 0600 in a private temp dir and delete it after the call, or keep it encrypted at rest and decrypt it into a 0600 file per call. Never use `--cookies-from-browser` silently.
- Building keyring v4 with only the needed stores (apple-native, windows-native, zbus-secret-service, linux-keyutils) via `keyring-core` keeps the dependency tree small. That is an inference from the docs' advice.

### Gaps
- I didn't verify keyring v4's `v1`-feature API ergonomics or default features (the feature-flag page wasn't read).
- I didn't check whether Windows DPAPI is used directly or through Credential Manager in `windows-native-keyring-store`.

## 6. Controlling mpv from Rust

### Takeaway
Spawn mpv with `tokio::process` and talk JSON IPC yourself, in about 200–300 lines. The available crates are GPL (`mpvipc` 1.3.1), tiny and stale (`mpv-ipc` 0.1.7, MIT), or require linking libmpv (`libmpv2` 6.0.0, LGPL bindings plus a system libmpv). On Unix, use `--input-ipc-client=fd://N` with a `socketpair()`: there's no filesystem socket for another process to connect to, and mpv quits when tuitube's end closes. On Windows, use a randomly named pipe `\\.\pipe\tuitube-<random>` through `tokio::net::windows::named_pipe`. Let mpv resolve YouTube URLs through its ytdl_hook, pointed at the same yt-dlp with `ignore-config` and `no-plugin-dirs` passed in.

### Cited Findings
- mpv JSON IPC warning: "This is not intended to be a secure network protocol. It is explicitly insecure: there is no authentication, no encryption, and the commands themselves are insecure too. For example, the `run` command is exposed, which can run arbitrary system commands." — [mpv DOCS/man/ipc.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/man/ipc.rst)
- IPC is enabled "by specifying the path to a unix socket or a named pipe using the option `--input-ipc-server`, or the file descriptor number of a unix socket or a named pipe using `--input-ipc-client`". — [mpv ipc.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/man/ipc.rst)
- `--input-ipc-client=fd://<N>`: "no socket is created, and instead the passed FD is treated like a socket connection received from `accept()`… you could pass either a FD created by `socketpair()`, or a pipe… you must make sure that the FD is actually inherited by mpv (do not set the POSIX `CLOEXEC` flag). The player quits when the connection is closed." — [mpv DOCS/man/options.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/man/options.rst)
- `--input-ipc-server` on Windows uses named pipes (`\\.\pipe\<name>`), with the prefix added automatically. On Linux a leading `@` means an abstract socket ("unspecified behavior on other UNIX platforms"). — [mpv options.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/man/options.rst)
- Protocol details:
  - UTF-8 JSON, one message per line.
  - `request_id` (an integer) is echoed in replies; without one, it's 0.
  - Events (`{"event": …}`) can interleave before replies, and `"async": true` allows out-of-order replies.
  - `observe_property` produces `property-change` events (e.g. `{"event":"property-change","id":1,"data":52.0,"name":"volume"}`).

  — [mpv ipc.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/man/ipc.rst)
- The mpv ytdl_hook runs `yt-dlp --no-warnings -J --flat-playlist --sub-format ass/srt/best --format <fmt>`, plus each `--ytdl-raw-options` key as `--key [value]`. The default format is `bestvideo*+bestaudio/bestvideo+bestaudio/best`. Separate tracks become an EDL. It does **not** add `--ignore-config`. — [mpv player/lua/ytdl_hook.lua](https://github.com/mpv-player/mpv/blob/master/player/lua/ytdl_hook.lua) (lines ~895–935)
- `--ytdl-raw-options=<key>=<value>[,…]`: "Options without argument must include `=`… There is no sanity checking". `ytdl_hook-ytdl_path` sets the yt-dlp path; the defaults are `yt-dlp`, `yt-dlp_x86`, `youtube-dl`, looked up in PATH and mpv's config dir. — [mpv options.rst](https://github.com/mpv-player/mpv/blob/master/DOCS/man/options.rst)
- Crates:
  - mpvipc 1.3.1 (2026-02-15; the previous release was 1.3.0 on 2023-08-03), **GPL-3.0**, about 760 recent downloads.
  - mpv-ipc 0.1.7 (2025-02-09, MIT).
  - libmpv2 6.0.0 (2026-05-12, LGPL-2.1, wraps `libmpv2-sys` 4.0.1).
  - libmpv 2.0.1 (2020).
  - **No crate named `mpvipc-async` exists on crates.io** (API lookup 2026-10-09).
  - youtube-tui uses `libmpv-sirno` (a 2022 fork) optionally.

  — crates.io ([mpvipc](https://crates.io/crates/mpvipc), [mpv-ipc](https://crates.io/crates/mpv-ipc), [libmpv2](https://crates.io/crates/libmpv2)); [youtube-tui deps](https://crates.io/crates/youtube-tui/0.9.4/dependencies)
- tokio provides Windows named-pipe client and server types in `tokio::net::windows::named_pipe`. — [docs.rs tokio named_pipe](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/index.html)
- Local install: mpv v0.41.0 offers `--input-ipc-client` and `--input-ipc-server` (`mpv --list-options`).

### Inferences
- Unix launch sequence:
  1. Create a `UnixStream::pair()` (std) and clear `FD_CLOEXEC` on the child end in `pre_exec`, or dup it to a fixed fd such as 3.
  2. Run `mpv --input-ipc-client=fd://3 --idle=no --force-window=yes --ytdl-raw-options=ignore-config=,no-plugin-dirs= --script-opts=ytdl_hook-ytdl_path=<path> -- <url>`.
  3. Wrap tuitube's end in `tokio::net::UnixStream::from_std`.

  Because "the player quits when the connection is closed", a tuitube crash doesn't leave an orphaned remote-controllable player. That suits tuimeta's "helper sees stdin close and stops" philosophy.
- If a server socket is ever needed, for example to re-attach to an existing mpv, put it in `<data>/run/` (0700) or `$XDG_RUNTIME_DIR`, never shared `/tmp`, and remove it on exit. Data-dir paths must stay under `sun_path` limits (108 bytes on Linux per [unix(7)](https://man7.org/linux/man-pages/man7/unix.7.html); macOS is smaller). Windows: a random pipe name per launch; ACLs not researched.
- Also add `--no-terminal`, since mpv must not write to the TUI's terminal; stdin, stdout and stderr go to null. The user's mpv config, input.conf and scripts may load, since that's their own player config. Offer a setting to pass `--no-config` for a clean player.
- Observed properties for the UI and resume: `time-pos`, `duration`, `pause`, `eof-reached`, `media-title`, `idle-active`, `demuxer-cache-time`. The `end-file` event (with `reason`) records history and resume.
- Audio-only mode passes `--no-video` (mpv's ytdl_hook then uses `bestaudio/best`). Either way, tuitube needs no stream URLs of its own unless it plays inline.

### Gaps
- I didn't check the Windows named-pipe ACL mpv creates (who else can connect), or whether `--input-ipc-client` accepts an inherited handle on Windows.
- I didn't verify the macOS `sun_path` limit from a primary source.

## 7. HTTP client: reqwest + rustls vs native-tls; does TLS/HTTP fingerprinting matter?

### Takeaway
Use reqwest 0.13, whose default is now rustls with aws-lc-rs and the platform verifier, for RSS, thumbnails (i.ytimg.com) and any InnerTube metadata. There is no evidence that YouTube or InnerTube gates on JA3/JA4. yt-dlp's YouTube extractor doesn't impersonate a browser for InnerTube or player calls, only for subtitle downloads (since 2025.07). Non-browser TLS stacks (curl/LibreSSL, Python OpenSSL, rustypipe's and ytermusic's rustls) get answers. YouTube's actual gates are PO tokens, JS challenges and SABR, which TLS impersonation doesn't solve.

### Cited Findings
- reqwest 0.13.5 (2026-09-08), default features `charset, default-tls, http2, system-proxy`. `default-tls` → `rustls` → `__rustls-aws-lc-rs` + `rustls-platform-verifier`, while `native-tls` is opt-in. — [docs.rs reqwest features](https://docs.rs/crate/reqwest/latest/features); [crates.io reqwest](https://crates.io/crates/reqwest)
- rustls-platform-verifier 0.7.1 (2026-09-24). — [crates.io](https://crates.io/crates/rustls-platform-verifier)
- yt-dlp's YouTube extractor (2026.08.19 source): the only `impersonate` uses in `extractor/youtube/_video.py` are `'impersonate': True` on subtitle tracks, plus a test case for a generic embed page. InnerTube and player requests are not impersonated. — [yt-dlp _video.py](https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/youtube/_video.py) (read from the installed 2026.08.19 package)
- "Use impersonation for downloading subtitles (#13786)" and "Allow extractors to designate formats/subtitles for impersonation (#13778)" shipped in 2025.07.21. A contemporaneous report shows subtitle "HTTP Error 429". — [newreleases yt-dlp 2025.07.21](https://newreleases.io/project/github/yt-dlp/yt-dlp/release/2025.07.21); [yt-dlp #13831](https://github.com/yt-dlp/yt-dlp/issues/13831)
- yt-dlp's README: impersonation "may be required for some sites that employ TLS fingerprinting", via curl_cffi, and is "Currently included in most builds *except* `yt-dlp` (Unix zipimport binary) and `yt-dlp_x86`". 25 extractors use `impersonate=`, including vimeo, tiktok, reddit, soundcloud and patreon, but not the YouTube API paths. — [yt-dlp README, Impersonation](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#impersonation); local grep of the installed extractor directory
- Local measurement: curl 8.7.1 (SecureTransport/LibreSSL 3.3.6) got HTTP 200 from InnerTube `search` and `browse` and from the RSS endpoint. The yt-dlp verbose run's YouTube API calls succeeded through its default handlers (`urllib, requests, websockets, curl_cffi` available, none forced).
- Clients that work with non-browser TLS: ytermusic uses reqwest 0.11 + `rustls-tls` for InnerTube; rustypipe uses reqwest with rustls or native-tls features. — [ytpapi2 Cargo.toml](https://raw.githubusercontent.com/ccgauche/ytermusic/master/crates/ytpapi2/Cargo.toml); [Codeberg rustypipe](https://codeberg.org/ThetaDev/rustypipe)
- If impersonation were ever needed in Rust, `wreq` 0.16.1 (2026-08-27, Apache-2.0, MSRV 1.98) exists. Its predecessor `rquest` was yanked to 0.0.0. — [crates.io wreq](https://crates.io/crates/wreq); [crates.io rquest](https://crates.io/crates/rquest)
- yt-dlp's verbose log shows the current anti-bot surface: "PO Token Providers: none", "JS Challenge Providers: … deno", and "Detected experiment to bind GVS PO Token to video ID for web client" (local measurement). Missing URLs from SABR-only responses are tracked in #12482. — [Mageia advisory](https://advisories.mageia.org/MGAA-2025-0098.html)

### Inferences
- Keep tuitube's own HTTP to: feeds (www.youtube.com/feeds), thumbnails (i.ytimg.com, yt3.ggpht.com) and, optionally, InnerTube metadata. Only these hosts should be allowed, under the same "only fetch what the user asked for" rule tuimeta uses ("No URL from a message is ever fetched", [PROTOCOL.md](/Users/eric/projects/Personal/tuimeta/helper/PROTOCOL.md)). Leave googlevideo stream fetching to mpv/ffmpeg via yt-dlp.
- Adding reqwest (rustls + aws-lc-rs) is a new dependency for this app family. Neither tuigram's nor tuimeta's Cargo.toml has reqwest or any HTTP client today ([tuimeta Cargo.toml](/Users/eric/projects/Personal/tuimeta/Cargo.toml), [tuigram Cargo.toml](/Users/eric/projects/Personal/tuigram/Cargo.toml)). aws-lc-rs needs a C toolchain and CMake/NASM on some targets. The `rustls-no-provider` + `ring` feature combination avoids that. This is an inference, and the build cost on Windows ARM64 is unverified.

### Gaps
- I found no rustypipe or Rust-client issue reporting JA3/JA4 blocking by Google, and no Google statement either way. The "no fingerprint gating" conclusion rests on yt-dlp's code and local probes, which were low-volume and residential.
- Whether googlevideo media requests are fingerprint-gated was not tested. They go through mpv/ffmpeg in the recommended design.

## 8. If a sidecar is chosen: verbs, events, versioning, stale-answer handling (modeled on tuimeta's PROTOCOL.md)

### Takeaway
Copy tuimeta's protocol frame unchanged:
- One JSON object per line.
- A u64 `id` chosen by the front end, and exactly one `result` or `error` per request.
- A `hello` first line carrying `version`.
- Exit when stdin closes.
- Nothing but protocol on stdout.
- A closed set of error codes.

Make three changes:
- Keep YouTube's own public ids as strings, validated. There's no secret mapping to hide, unlike Meta.
- Add an explicit `cancel`.
- Make every list call return an opaque `continuation` so the helper holds the paging state.

Stale answers are dropped as tuimeta does: requests carry what was asked, the front end keeps the current "ask" tag and a helper-generation number, and anything that doesn't match is discarded.

### Cited Findings
- tuimeta framing:
  - `→ {"id": 7, "method": "send_text", "params": {...}}`, then `← {"id": 7, "result": {...}}`, `← {"id": 8, "error": {"code": "not_found", "message": "No such chat."}}` or `← {"event": "message", ...}`.
  - "Every request gets exactly one response… responses can arrive in any order, and events a request causes can arrive before its response."
  - Unknown methods return `unknown_method`. Unknown params fields are ignored, and `?` fields may be absent or null.
  - Error codes: `bad_request, unknown_method, not_found, not_logged_in, bad_cookies, checkpoint, network, unsupported, timeout, cancelled, internal`. `message` is "one sentence a person can act on".

  — [/Users/eric/projects/Personal/tuimeta/helper/PROTOCOL.md](/Users/eric/projects/Personal/tuimeta/helper/PROTOCOL.md)
- tuimeta's hello and lifecycle:
  - The first line is always `{"event":"hello","version":3,"helper":"<semver>","networks":[…]}`, and "tuimeta refuses to go on if `version` isn't one it knows". The version history is documented inline (v2 added `browser`, v3 added WhatsApp).
  - "When stdin closes… exits within two seconds".
  - "Nothing but protocol lines goes to stdout. The helper logs to `<data-dir>/helper.log` (connection states and error kinds only…)".

  — [PROTOCOL.md](/Users/eric/projects/Personal/tuimeta/helper/PROTOCOL.md)
- The `--fake` mode is deterministic with no network: same ids, names and texts every run, used by app tests. — [PROTOCOL.md](/Users/eric/projects/Personal/tuimeta/helper/PROTOCOL.md)
- On the front end, "Requests carry what was asked (chat id, `Page`, query) so stale answers can be dropped". A picker result is "kept only if it matches `App.finding` and nothing holds the keys". `login_link`/`cancel_login`/`login_code` carry an `attempt` so "answers or codes of a login given up are dropped". — [/Users/eric/projects/Personal/tuimeta/CLAUDE.md](/Users/eric/projects/Personal/tuimeta/CLAUDE.md)
- The earlier report: "each helper start gets a generation number, so events from a replaced helper are dropped exactly as `tg::Tagged` drops events from a replaced TDLib client". — [Meta messengers report](/Users/eric/projects/Personal/tuigram/reports/Meta%20messengers%20terminal%20client.md)
- yt-dlp exposes the pieces such a helper would wrap:
  - `extract_info(url, download=False)` with `process=False` for lazy entries (measured 0.76 s for a channel).
  - `youtube:max_comments` and `comment_sort` extractor args.
  - `ytsearchN:` and `ytsearchdate` prefixes.
  - `sanitize_info` for JSON.

  — [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md); local measurement

### Inferences
**Proposed `tuitube-helper` protocol v1.** All ids are YouTube's own strings, validated by the front end before display or reuse: video `^[A-Za-z0-9_-]{11}$`, channel `^UC[A-Za-z0-9_-]{22}$`, playlist `^(PL|UU|OL|RD|LL|FL)[A-Za-z0-9_-]+$`.

- Startup line: `{"event":"hello","version":1,"helper":"0.1.0","backend":{"yt_dlp":"2026.08.19","ejs":"0.8.0","js_runtime":"deno 2.9.6"|null}}`. The front end refuses unknown `version`s and warns when `js_runtime` is null ("formats limited"), or when yt-dlp is older than 90 days (yt-dlp's own threshold, per its README).
- Requests:

  | method | params | result |
  |---|---|---|
  | `search` | `query`, `kind?` (`video`/`channel`/`playlist`/`all`), `sort?` (`relevance`/`date`), `continuation?` | `{items:[Item], continuation?}` |
  | `suggest` | `query` | `{suggestions:[string]}` |
  | `channel` | `channel_id` or `handle`, `tab` (`videos`/`shorts`/`streams`/`playlists`/`about`), `continuation?` | `{channel: ChannelInfo, items:[Item], continuation?}` |
  | `playlist` | `playlist_id`, `continuation?` | `{playlist: PlaylistInfo, items:[Item], continuation?}` |
  | `video` | `video_id` | `{video: VideoDetail}` (title, description, chapters, channel, published, duration, views, likes, live_status, availability, thumbnails) |
  | `streams` | `video_id`, `audio_only?` | `{formats:[Format], expires_at, subtitles:[Track]}`; only needed if tuitube ever plays without mpv's ytdl_hook or downloads |
  | `comments` | `video_id`, `sort` (`top`/`new`), `continuation?`, `limit` | `{comments:[Comment], continuation?, total?}` |
  | `related` | `video_id` | `{items:[Item]}` |
  | `feed` | `channel_ids:[…]`, `limit_per_channel` | `{items:[Item], failed:[channel_id]}`; a fallback for channels whose RSS failed (RSS itself stays in Rust) |
  | `resolve` | `url` | `{kind:"video"|"channel"|"playlist"|"handle", id, start_s?}`; only `youtube.com`, `youtu.be`, `music.youtube.com` hosts |
  | `cancel` | `id` (the request to cancel) | `{}`; the cancelled request then answers `cancelled` |
  | `backend` | none | `{yt_dlp, ejs, js_runtime, python}`; for a `:doctor` screen |

- Objects:
  - `Item`: `{kind, id, title, channel?:{id,name}, duration_s?, published?, views?, live_status?, is_short?, thumbnails:[{url,width,height}]}`, with `description?` clipped to 300 characters as tuimeta clips snippets.
  - `continuation`: an opaque string issued by the helper for the run only. If the helper restarts, it's invalid, and the request answers `bad_request` → the front end reloads page 1.
- Events:
  - `hello`
  - `warning {message}`, e.g. yt-dlp's "formats may be missing" or SABR warnings, mapped to fixed phrases
  - `progress {id, done, total?}` for long comment pulls
  - `error {message}` for things no request caused
- Errors: reuse tuimeta's code list minus the login-specific codes, plus:
  - `unavailable`: private, removed or members-only
  - `age_restricted`
  - `rate_limited`: 429 or "confirm you're not a bot"
  - `backend_missing`: yt-dlp or Deno absent
  - `outdated`: the extractor needs an update, so tell the user to run `yt-dlp -U`
- Stale answers: every front-end view holds a `Want {view_id, query/page key, generation}`. Answers are routed via `Meta::then`-style closures that compare against the current `Want`, and mismatches are dropped. Typing in search sends `cancel` for the previous in-flight `search`, then a new one after a debounce, like tuimeta's `SEARCH_AFTER`.
- Lifecycle: `kill_on_drop`, and the helper exits when stdin closes. The front end restarts it on `Gone`, with backoff and a status-bar line, and bumps `generation`.
- Helper rules (a "must never" list):
  - Never download media.
  - Fetch only URLs on the YouTube host allowlist.
  - Never read cookies unless passed a cookie file path.
  - Never write outside `--data-dir`.
  - Never log titles, queries or ids, only error kinds, mirroring tuimeta's "No message text, names… in logs".
- Keep a `--fake` mode with deterministic channels, videos and comments, so ratatui `TestBackend` tests run with no network.

### Gaps
- yt-dlp has no first-class continuation-token API, so a Python helper would hold Python generators per continuation. Their memory use and the eviction policy (e.g. 50 live continuations, LRU) were not tested.
- How to cancel an in-progress `extract_info` inside Python (thread interruption) wasn't researched. Killing and restarting the helper is the fallback.

## 9. What transfers from tuigram/tuimeta vs needs rewriting

### Takeaway
About a third of tuimeta's 22.2k lines is infrastructure that transfers nearly as-is: text cleaning, config and data-dir safety, settings, theme, clipboard, notify, tmux, completion, help, images and viewer, and the helper-connection plumbing in `meta.rs`. The chat-domain modules must be rewritten as YouTube views (feeds, lists, video detail, comments, player bar): app state, chat list, messages, reactions, attachments and service messages.

### Cited Findings
- tuimeta's `src` line counts:

  | Module | Lines |
  |---|---|
  | `app.rs` | 5223 |
  | `ui/mod.rs` | 3270 |
  | `ui/messages.rs` | 2687 |
  | `messages.rs` | 1866 |
  | `meta.rs` | 1552 |
  | `demo.rs` | 996 |
  | `chats.rs` | 862 |
  | `images.rs` | 730 |
  | `attach.rs` | 636 |
  | `ui/chat_list.rs` | 628 |
  | `theme.rs` | 592 |
  | `notify.rs` | 451 |
  | `reactions.rs` | 419 |
  | `clipboard.rs` | 294 |
  | `ui/viewer.rs` | 266 |
  | `picker.rs` | 264 |
  | `ui/help.rs` | 258 |
  | `config.rs` | 202 |
  | `settings.rs` | 194 |
  | `main.rs` | 177 |
  | `complete.rs` | 167 |
  | `viewer.rs` | 137 |
  | `tmux.rs` | 116 |
  | `text.rs` | 93 |
  | `search.rs` | 74 |
  | `service.rs` | 74 |
  | **Total** | **22,228** |

  tuigram's is 45,646 total, including an Opus decoder under `src/opus/`, plus `voice.rs`, `sound.rs`, `stickers.rs` and `tg.rs` 2336. — local `wc -l` of [/Users/eric/projects/Personal/tuimeta/src](/Users/eric/projects/Personal/tuimeta/src) and [/Users/eric/projects/Personal/tuigram/src](/Users/eric/projects/Personal/tuigram/src)
- tuimeta's architecture:
  - One `tokio::select!` in `App::run` over helper events, image encodes, clipboard results, terminal events, signals and the quit deadline: "Each wake drains the event backlog, then redraws. All state lives in `App`".
  - Modes and keys route to the first modal layer (confirm, photo viewer, settings, …, Insert, Normal).
  - `:` commands only by full name, with Tab completion.
  - The rest (images, the viewer with `h`/`l`/`j`/`k` and `MAX_LARGE`, clipboard, themes, demo scenes) "works as in tuigram".

  — [/Users/eric/projects/Personal/tuimeta/CLAUDE.md](/Users/eric/projects/Personal/tuimeta/CLAUDE.md), "Architecture"
- `text.rs` removes:
  - control characters other than `\n`/`\t`
  - bidi overrides, isolates and marks
  - invisible characters terminals and layout disagree on (Hangul fillers, soft hyphen, ZWSP…)
  - U+2028 and U+2029
  - kitty's image placeholder character

  — [/Users/eric/projects/Personal/tuimeta/src/text.rs](/Users/eric/projects/Personal/tuimeta/src/text.rs)
- `images.rs`:
  - Decodes and encodes on a blocking thread, within `images::limits`, with at most `MAX_BUILDING` at once.
  - Decoder panics are contained (`panic_is_contained`), and sixel and iTerm2 images aren't painted under popups.
  - Today it fetches "a download through the helper". It uses `ratatui-image` 11.1 and `image` 0.25.

  — [/Users/eric/projects/Personal/tuimeta/src/images.rs](/Users/eric/projects/Personal/tuimeta/src/images.rs); [tuimeta/CLAUDE.md](/Users/eric/projects/Personal/tuimeta/CLAUDE.md); [tuimeta/Cargo.toml](/Users/eric/projects/Personal/tuimeta/Cargo.toml)
- Links open through `open_externally` (the `open` crate, ShellExecute on Windows), never a shell, and only http(s) is kept. Notification text goes through `notify::escape`. A panic restores the terminal and prints through `text::clean`. — [tuimeta/CLAUDE.md](/Users/eric/projects/Personal/tuimeta/CLAUDE.md), "Security"
- Shared stack in both Cargo.toml files: ratatui 0.30.2, crossterm 0.29 (event-stream), tokio 1.53 full, ratatui-image 11.1, ratatui-textarea 0.9.2, image 0.25, arboard 3.6, open 5.4.4, dirs 6, dotenvy, serde/serde_json, toml 1.1.6, unicode-width, textwrap, emojis, chrono, and libc on Unix. tuimeta is `publish = false` ("Distributed on GitHub only"). tuigram is on crates.io as `tuigram-cli`, with `cargo binstall` metadata pointing at GitHub release archives for all six targets. — [/Users/eric/projects/Personal/tuimeta/Cargo.toml](/Users/eric/projects/Personal/tuimeta/Cargo.toml); [/Users/eric/projects/Personal/tuigram/Cargo.toml](/Users/eric/projects/Personal/tuigram/Cargo.toml)
- tuigram's release workflow builds `x86_64/aarch64-unknown-linux-gnu`, `aarch64/x86_64-apple-darwin` and `x86_64/aarch64-pc-windows-msvc`, and pins the SHA-256 of each downloaded native dependency (TDLib zips). — [/Users/eric/projects/Personal/tuigram/.github/workflows/release.yml](/Users/eric/projects/Personal/tuigram/.github/workflows/release.yml)

### Inferences
- **Transfer almost as-is** (rename env prefix `TM_`→`TT_`, app name):
  - `text.rs`: YouTube titles, descriptions, comments and channel names are untrusted, and bidi or invisible tricks are common in spam comments.
  - `config.rs`: data dir, `private_place`, dev-only `.env`, `shown`.
  - `settings.rs`: atomic 0600 save. Replace the fields.
  - `theme.rs`
  - `clipboard.rs`: copying a video URL or timestamped link.
  - `tmux.rs`: image passthrough.
  - `complete.rs`: `:` command completion.
  - `ui/help.rs`: the `SHORTCUTS` table pattern.
  - `notify.rs`: `escape`, OSC 9/777/99 back ends, and `Notifier` batching for "N new videos from subscriptions"; drop the chat-read logic.
  - `main.rs`: panic hook and terminal restore.
- **Transfer with moderate edits:**
  - `images.rs`: replace "download through the helper" with a reqwest fetch from i.ytimg.com/yt3.ggpht.com into an LRU disk cache, keeping the decode limits and panic containment. It's the thumbnail grid's engine.
  - `viewer.rs`/`ui/viewer.rs`: a full-size thumbnail or channel-banner viewer.
  - `picker.rs`: fuzzy-find subscriptions locally plus remote `search` after a typing pause, the same two-phase design as tuimeta's `s` picker.
  - `meta.rs`: becomes `backend.rs` (helper spawn, hello/version check, writer and reader tasks, `MAX_LINE`, pending map, `Gone`, `detached()` and `sent()` for tests). The per-request `then` closures carry the "what was asked" tags. For architecture (b), the same API is backed by per-request subprocesses.
  - `demo.rs`: keep the pattern (scenes, never calls `App::run`) with YouTube fixtures.
- **Rewrite:**
  - `app.rs` state: keep the event-loop skeleton and modal-layer routing; replace chat state with views (Subscriptions feed, Channel, Playlist, Search, Video detail, Comments, Watch later, History, Player status).
  - `chats.rs`, `ui/chat_list.rs`: become `feed.rs`, `ui/list.rs`.
  - `messages.rs`, `ui/messages.rs`: become `ui/video.rs` and `ui/comments.rs`.
  - Drop `reactions.rs`, `attach.rs` and `service.rs`; there's no YouTube equivalent without an account.
  - New modules: `feeds.rs` (RSS), `db.rs` (rusqlite), `player.rs` (mpv IPC), `ytdlp.rs` (subprocess wrapper).
- Rough estimate (inference): 4–5k lines reusable from tuimeta (text, config, settings, theme, clipboard, notify, tmux, complete, help, images, viewer, picker, meta plumbing, main), out of 22k.
- Distribution follows tuigram rather than tuimeta: a single MIT crate on crates.io plus binstall archives for six targets, since there's no copyleft helper to keep out of the crate. For a later Python sidecar, ship it in the repo as MIT (yt-dlp is Unlicense) and document `pip install yt-dlp[default]` + Deno as prerequisites, or reuse the system yt-dlp.

### Gaps
- I didn't read every module's body (e.g. `notify.rs` internals, `picker.rs`), so "as-is" is based on CLAUDE.md descriptions and file headers. Some chat-specific coupling may surface, such as `images.rs` importing `chats::ChatPhoto` and `messages::Preview`.
- I didn't verify whether tuigram's `images.rs` and `ui/viewer.rs` (747 and 267 lines) differ materially from tuimeta's, to decide which copy to start from.
