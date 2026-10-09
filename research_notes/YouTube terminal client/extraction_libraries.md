# Unofficial YouTube extraction libraries (InnerTube clients) and YouTube's anti-bot countermeasures, as of 2026-10-09

Scope note: research done 2026-10-09. Sources are primary where possible: GitHub/Codeberg repos, release notes, changelogs, issue trackers, source code on default branches, and the crates.io/npm/PyPI registries. They are supplemented by **live smoke tests run from the user's own machine on 2026-10-09** (macOS, one residential IP, logged out). The tests used yt-dlp 2026.08.19 (Homebrew) with Deno 2.9.6, rustypipe 0.11.4 built from crates.io, and youtubei.js 18.1.0 from npm. Test results are labelled "Local smoke test 2026-10-09". They are one session from one IP, so treat them as a snapshot. YouTube runs A/B experiments per session, region and IP.

---

## 1. Which InnerTube client identities still return usable stream URLs in 2026 (with/without PO tokens, logged out vs logged in)?

### Takeaway
As of October 2026, logged-out playback across the whole ecosystem rests on a few "native app" identities that YouTube still serves direct googlevideo URLs to without a PO token:
- **VISIONOS** is the primary one, adopted by yt-dlp 2026.08.19, youtubei.js 18.0.0 and NewPipeExtractor's dev branch.
- **IOS** and **ANDROID_VR** still work, but only partially or intermittently.

WEB and TV are effectively SABR-only for guests. MWEB, WEB_CREATOR and WEB_MUSIC need a GVS PO token. WEB_EMBEDDED works only for embeddable videos and is drifting toward SABR-only. Logged-in extraction is currently *more* fragile than logged-out, because the app clients (visionos/ios/android/android_vr) do not accept cookies.

### Cited Findings
- **yt-dlp master defaults (Oct 2026).**
  - Logged out: `visionos,web`. With no JS runtime it is `visionos` only (`_DEFAULT_CLIENTS = ('visionos','web')`, `_DEFAULT_JSLESS_CLIENTS = ('visionos',)`).
  - Logged in, free account: `web_embedded,tv_downgraded,web`. Logged in, Premium: `web_creator,tv_downgraded,web`.
  - `web_music` is added for music.youtube.com with cookies. `web_embedded` and `web_creator` are added for age-restricted videos.
  - [yt-dlp README, player_client](https://github.com/yt-dlp/yt-dlp/blob/master/README.md); [yt-dlp _video.py](https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/yt_dlp/extractor/youtube/_video.py)
- **yt-dlp's documented client list (Oct 2026):** `web, web_safari, web_embedded, web_music, web_creator, mweb, ios, visionos, android, android_vr, tv, tv_downgraded, tv_simply`. It notes that `web_creator` and `web_music` "require a po_token for their formats to be downloadable", that `web_creator` "will only work with authentication", and that "Not all clients support authentication via cookies". — [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md)
- **yt-dlp `INNERTUBE_CLIENTS` source (client versions dated 2026-07), with in-code comments:**
  - `web_safari`: "Since 2026.07, HLS formats are only returned with some logged-in or 'trusted' sessions."
  - `web_creator`: "now requires sign-in for every video."
  - `android_vr` (clientVersion 1.65.10, Oculus Quest 3): "Using a clientVersion>1.65 may return SABR streams only"; "Since 2026.07, intermittent/selective POT enforcement has been observed for non-HLS formats"; "Since 2026.08.17, ALL formats (including live HLS and itag 18) are 403'd with version 1.65.10."
  - `ios`: "HLS Livestreams require POT 30 seconds in."
  - `visionos` (clientName VISIONOS, version 1.02) has `REQUIRE_JS_PLAYER: False` and no GVS PO-token policy. Both visionos and android_vr carry the comment "'Made for kids' videos aren't available with this client".
  - Cookie support (`SUPPORTS_COOKIES: True`) exists only on web, web_safari, web_embedded, web_music, web_creator, mweb, tv and tv_downgraded. The default for others is False, so android/ios/visionos/android_vr/tv_simply take no cookies.
  - [yt-dlp _base.py](https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/yt_dlp/extractor/youtube/_base.py)
- **PO-token requirement policy in yt-dlp source.** The `WEB_PO_TOKEN_POLICIES` (web, web_safari) require a GVS token for HTTPS and DASH (`not_required_for_premium=True`), but not for HLS. mweb, web_music, web_creator and tv_simply require GVS for HTTPS/DASH. android and ios require GVS unless a player token is supplied (`not_required_with_player_token=True`). — [yt-dlp _base.py](https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/yt_dlp/extractor/youtube/_base.py)
- **yt-dlp PO Token Guide wiki, "Current PO Token enforcement" table** (page edited 2026-07-12; the visionos addition on 2026.08.19 post-dates it):

  | Client | PO token required for | Notes |
  |---|---|---|
  | web | Subs, GVS | "Only SABR formats available" |
  | web_safari | GVS | HLS doesn't need GVS "for now" |
  | mweb | GVS | |
  | tv | none | "Formats are DRM'd without cookies; only SABR formats in some cases" |
  | tv_simply | GVS | "Account cookies not supported" |
  | web_embedded | none | "Only embeddable videos available" |
  | web_music | GVS | |
  | web_creator | GVS | "Requires account cookies" |
  | android | GVS or Player | Account cookies not supported |
  | android_vr | none | "Made for kids" unavailable |
  | ios | GVS or Player | Account cookies not supported |

  The guide also says GVS isn't required for Premium subscribers, and HLS live streams need no token except on ios. — [yt-dlp wiki: PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide)
- **yt-dlp 2026.08.19 release:** "Add `visionos` player client", "Add `web_embedded` client fallbacks", "Remove `android_vr` from default clients". — [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases/tag/2026.08.19)
- **youtubei.js 18.0.0 (2026-08-13):** "Session: Add the `VISIONOS` client" and "add User-Agent header override for ANDROID_VR client". youtubei.js 17.0.0 (2026-03-16) added `ANDROID_VR`. — [YouTube.js CHANGELOG](https://github.com/LuanRT/YouTube.js/blob/main/CHANGELOG.md)
- **NewPipeExtractor dev branch (Oct 2026).** `YoutubeStreamExtractor.onFetchPage()` calls `fetchVisionOsClient(...)` for streams and `fetchWebClientMetadataAndSetThumbnails(...)` for metadata. `setPoTokenProvider` is documented as: "This method currently doesn't do anything, as the extractor doesn't use any client supporting poTokens until SABR support is added to the extractor." — [NewPipeExtractor YoutubeStreamExtractor.java (dev)](https://github.com/TeamNewPipe/NewPipeExtractor/blob/dev/extractor/src/main/java/org/schabi/newpipe/extractor/services/youtube/extractors/YoutubeStreamExtractor.java)
- **mweb and web_embedded going SABR-only.** yt-dlp issue #17666 (opened 2026-09-10, open): "Both `mweb` and `web_embedded` have become SABR-only in some sessions (except for format 18)", with or without cookies. On 2026-09-16 maintainer gamer191 said that as of a master commit `web_embedded` "should also get formats 91-96" (HLS, up to 1080p, worse codecs), while "mweb still only gets format 18 when this occurs". — [yt-dlp #17666](https://github.com/yt-dlp/yt-dlp/issues/17666)
- **Logged-in default is fragile.** yt-dlp issue #17389 (opened 2026-08-07, still open): `tv_downgraded` "(the default client for logged in users) has some problems for some people right now and it cannot be used". On 2026-08-19 maintainer bashonly said the embedded-player fallback is "a temporary workaround… since the actual solution to this demands some major changes to the Youtube extractor and EJS". — [yt-dlp #17389](https://github.com/yt-dlp/yt-dlp/issues/17389)
- **web_embedded limits.** yt-dlp issue #17497 (opened 2026-08-20, open, 38 comments): "Video unavailable. Playback on other websites has been disabled by the video owner (web_embedded…)". This shows the embed client cannot play non-embeddable videos. — [yt-dlp issue search, 2026 site-bug](https://github.com/yt-dlp/yt-dlp/issues/17497)
- **Local smoke test 2026-10-09, youtubei.js 18.1.0, logged out, `getBasicInfo` per client** (videos dQw4w9WgXcQ and kJQP7kiw5Fk; first 1 MiB of the best audio fetched):
  - VISIONOS, IOS and ANDROID_VR returned direct URLs; HTTP 206 OK.
  - MWEB returned URLs but HTTP 403 (no PO token).
  - TV and WEB returned `adaptive_formats` with **0 URLs** (SABR-only; every client also advertised `server_abr_streaming_url`).
  - WEB_EMBEDDED: "This video is unavailable" for both videos.
  - Search (20 results) and comments (20) worked.
  - Source: local test (no URL).
- **Local smoke test 2026-10-09, yt-dlp 2026.08.19 + Deno 2.9.6.** It requested only the `visionos` player JSON, listed 360p–1080p DASH plus HLS formats tagged "VISI", downloaded the player JS and solved JS challenges via Deno ("[jsc:deno] Solving JS challenges using deno"). `--test` downloads of 137+251 (dQw4w9WgXcQ) and 399+251 (kJQP7kiw5Fk) succeeded. Source: local test.
- **Local smoke test 2026-10-09, rustypipe 0.11.4.** The player used the **IOS** client.
  - dQw4w9WgXcQ: 12 MiB of video and the full 3.4 MB audio stream downloaded with HTTP 206.
  - kJQP7kiw5Fk: video itag 399 (1080p AV1) was fine, but **audio itag 251 returned HTTP 403** on the first byte. This is consistent with "selective POT enforcement" on app clients.
  - Stream URL expiry was 21,540 s (~6 h).
  - Source: local test.
- **Third-party report (low reliability).** A September 2026 PR in an unrelated project (opened by a bot account) claims `visionos, web` are "aggressively bot-checked on datacenter IPs" and that `tv` "needs no PO token and is rarely bot-checked". This is unverified and conflicts with the wiki statement that tv is DRM/SABR for guests. — [svimran46/AnyDown PR #4](https://github.com/svimran46/AnyDown/pull/4)

### Inferences
- In October 2026 the single most important identity for a logged-out personal client is VISIONOS, with IOS and ANDROID_VR as degraded fallbacks. Every major extractor converged on it within days in August 2026 (youtubei.js on 08-13, yt-dlp on 08-19, NewPipeExtractor dev). That is a single point of failure: when YouTube closes it, every "no PO token, no SABR" path breaks at once, as happened with ios (2024–25) and android_vr (2026-08-17).
- The "web" family (WEB, MWEB, WEB_SAFARI, WEB_CREATOR, WEB_MUSIC) needs BotGuard PO tokens, SABR support, or both. Any long-term-robust backend therefore needs either (a) a SABR implementation plus PO tokens, or (b) a maintainer team that keeps rotating to whichever app client still serves URLs. yt-dlp does (b) fast and is building (a) (PR #13515). youtubei.js plus googlevideo plus BgUtils already offers (a) in JS.
- For logged-in features, tuitube should separate the two concerns. Use cookies only for account *feeds* (subscriptions, Watch Later, history) and resolve *streams* logged out (visionos), unless the video is age-restricted or members-only. This follows from the cookie-less app clients and the current tv_downgraded/web_embedded breakage.

### Gaps
- YouTube does not document client gating. All the client behaviour above is reverse-engineered, and it varies by session/A-B experiment, region, IP reputation and video type (made-for-kids, music/VEVO, live, premieres).
- No 2026 source gives a full logged-in matrix per client. The wiki table only partially notes cookie behaviour.
- The local tests were not repeated over time or from other IPs, and did not cover livestreams, age-restricted, members-only or made-for-kids videos.

---

## 2. Countermeasure timeline 2024–2026, with dates and how fast libraries recovered

### Takeaway
YouTube has escalated in waves:
- **2024:** bot checks plus the first PO tokens; OAuth killed in November.
- **H1 2025:** content-bound PO tokens, the SABR-only web client, and constant player-JS churn.
- **H2 2025:** the JS challenges became too complex for regex interpreters, so yt-dlp has required Deno/Node plus yt-dlp-ejs since 2025-11-12.
- **2026:** trending removed, STS/player blocks, android_vr killed on 2026-08-17, SABR spreading to mweb/web_embedded, forced pre-roll waits.

yt-dlp's time-to-fix for outright breakage in 2026 has typically been **same day to 3 days** (nightly first, stable 1–2 days later). youtubei.js/FreeTube has taken roughly 1–2 weeks for comparable events. rustypipe has not shipped a release since April 2025.

### Cited Findings
**2024**
- **2024-05-26:** yt-dlp removed `android` from its default clients. **2024-07-01:** it began skipping formats if nsig decoding fails. **2024-07-16:** "Avoid poToken experiment player responses". — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- **2024-06-07:** yt-dlp issue #10128 "[youtube] Sign in to confirm you're not a bot" opened. Commonly attributed to IP reputation (datacenter/VPN/shared IPs). — [yt-dlp #10128](https://github.com/yt-dlp/yt-dlp/issues/10128); [Invidious docs: YouTube errors explained](https://docs.invidious.io/youtube-errors-explained/)
- **August 2024:** rustypipe-botguard: "Since August 2024 YouTube requires PO tokens to access streams with web-based clients. Otherwise streams will return a 403 error." Default clients churned as a result: 2024.08.01 → `ios,tv`; 2024.08.06 → `ios,web_creator`; 2024.10.07 → `ios,mweb`. — [rustypipe-botguard README](https://codeberg.org/ThetaDev/rustypipe-botguard); [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- **September 2024:** YouTube blocked publicly hosted Invidious instances; datacenter IPs are blocked while residential may still work. (Secondary source.) — [Wikipedia: Invidious](https://en.wikipedia.org/wiki/Invidious). The Invidious docs confirm "YouTube blocks datacenter and VPN IP addresses". — [Invidious docs](https://docs.invidious.io/youtube-errors-explained/)
- **2024-09-27:** yt-dlp added `po_token`, `visitor_data` and `data_sync_id` extractor args. — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- **OAuth:** yt-dlp added "Support logging in with OAuth" on **2024-10-22**, then on **2024-11-18** shipped "Login with OAuth is no longer supported for YouTube… Due to a change made by the site" (Remove broken OAuth support). — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md). ytmusicapi docs: "As of November 2024, YouTube Music requires a Client Id and Secret for the YouTube Data API to connect to the API" (the user must create their own "TVs and Limited Input devices" OAuth client). — [ytmusicapi OAuth docs](https://ytmusicapi.readthedocs.io/en/stable/setup/oauth.html)
- **2024-12-23:** yt-dlp "Skip iOS formats that require PO Token". — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)

**2025**
- **2025-01-12:** yt-dlp defaulted to `tv` instead of `mweb`. **2025-01-26:** "Use different PO token for GVS and Player". — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- **2025-02-26:** yt-dlp issue #12482 "`web` only has SABR formats" opened. YouTube removed direct `adaptiveFormats` URLs from WEB player responses, leaving only a SABR URL. Still open in Oct 2026. — [yt-dlp #12482](https://github.com/yt-dlp/yt-dlp/issues/12482)
- **March 2025:** repeated player-JS breakage of signature/nsig extraction (players `643afba4`, `363db69b`, `4fcd6e4a`), fixed in 2025.03.21, 2025.03.25, 2025.03.26 and 2025.03.27. — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- **2025-05-22:** yt-dlp "Add a PO Token Provider Framework" and "Add PO token support for subtitles". — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- **May 2025:** NewPipe 0.27.7 addressed YouTube serving only 360p and said "in the long run the SABR video protocol needs to be implemented". — [F-Droid NewPipe listing](https://f-droid.org/en/packages/org.schabi.newpipe/) (via search snippet)
- **2025-07-21:** "Do not require PO Token for premium accounts". **2025-08-20:** "Handle required preroll waiting period", "Add `playback_wait` extractor-arg". **2025-08-22:** "Replace `ios` with `tv_simply` in default clients". **2025-09-26:** "Replace `tv_simply` with `web_safari` in default clients". — [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- **2025-09-23:** yt-dlp announcement #14404 (bashonly). The challenge functions are now "spread out all over the player"; regex extraction "would be far too brittle and far too costly"; the plan is Deno (default) or Node/Bun/QuickJS, an AST-based solver in yt-dlp/ejs, and no PhantomJS/Selenium/headless browsers. — [yt-dlp #14404](https://github.com/yt-dlp/yt-dlp/issues/14404)
- **2025-10-12:** youtubei.js 16.0.0, "Use AST-based JS extraction with side-effect safe code emission" (breaking; async evaluator). **2025-10-14:** yt-dlp "Detect experiment binding GVS PO Token to video id". **~2025-10-16:** FreeTube 0.23.12 "Fixes video playback by using a video-ID-bound poToken". — [YouTube.js CHANGELOG](https://github.com/LuanRT/YouTube.js/blob/main/CHANGELOG.md); [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases); [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases) (year inferred from the youtubei.js 16.0.1 date)
- **2025-10-22:** yt-dlp "stopgap release with a TEMPORARY partial fix"; "Some formats may still be unavailable, especially if cookies are passed". — [yt-dlp 2025.10.22](https://github.com/yt-dlp/yt-dlp/releases/tag/2025.10.22)
- **2025-11-12:** "An external JavaScript runtime is now required for full YouTube support" ("Implement external n/sig solver"). Without a runtime, YouTube support is "deprecated", format availability is restricted "severely so in some cases (e.g. for logged-in users)", and the maintainers expect it to become impossible. — [yt-dlp #15012](https://github.com/yt-dlp/yt-dlp/issues/15012)

**2026**
- **2026-01-15 → 01-18:** `web_safari` m3u8 HTTP 403 (#15569, 71 comments) closed in 3 days. Livestream 403 with cookies (#15587) closed 01-18. — [yt-dlp issues](https://github.com/yt-dlp/yt-dlp/issues/15569)
- **2026-01-29 → 01-31:** regressions after 2026.01.29 (403 with `ios_downgraded`, #15782) fixed in 2026.01.31, which removed broken `ios_downgraded` and `tv_embedded`. 2026.01.29 also added "Solve n challenges for manifest formats" and "Support comment subthreads". — [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases)
- **2026-01-30:** rustypipe channel `channel_videos_order` continuation started failing (issue #73; workaround is to use `channel_videos`). — [rustypipe #73](https://codeberg.org/ThetaDev/rustypipe/issues/73)
- **2026-02-04:** yt-dlp "Default to `tv` player JS variant". **2026-02-21:** "Remove broken `ytsearchdate` support" (date-sorted search). — [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases)
- **Early March 2026:** FreeTube 0.23.14 (~03-05) forced player `9f4cc5e4` to fix "[object Object]" playback errors, warning it "could stop working at any point". yt-dlp 2026.03.03 also "Force player `9f4cc5e4`". — [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases); [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases)
- **2026-03-07:** rustypipe issue #75: "The library doesn't work anymore, it errors with `extraction error: deobfuscation error: could not extract sig fn name`" (unanswered). — [rustypipe #75](https://codeberg.org/ThetaDev/rustypipe/issues/75)
- **2026-03-11 → 03-13:** "UNPLAYABLE / The page needs to be reloaded" (#16212, 80 comments). bashonly: "YouTube blocking the player (STS) that was pinned… as a temporary workaround". `android_vr` kept working for logged-out users. Fixed 03-13 via ejs 0.7.0. A follow-up Deno n-challenge TypeError (#16256) was fixed the same day (03-17, ejs 0.8.0). youtubei.js fixed the same symptom in 17.0.0 (2026-03-16), reaching FreeTube 0.23.15 around 03-18. — [yt-dlp #16212](https://github.com/yt-dlp/yt-dlp/issues/16212); [YouTube.js CHANGELOG](https://github.com/LuanRT/YouTube.js/blob/main/CHANGELOG.md); [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases)
- **March–April 2026:** YouTube removed the Trending feed. youtubei.js 17.0.0: "Remove getTrending as YouTube removed the trending feed". FreeTube 0.24.0 (~04-01) now sources trending from the Gaming/Sports/Podcasts channels. rustypipe community PR #84 (2026-08-21): "use YouTube's Live channel as trending()'s source, not dead FEtrending". — [YouTube.js CHANGELOG](https://github.com/LuanRT/YouTube.js/blob/main/CHANGELOG.md); [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases); [rustypipe PRs](https://codeberg.org/ThetaDev/rustypipe/pulls)
- **~2026-04-01:** FreeTube 0.24.0 added SABR playback via its local API (youtubei.js + googlevideo) and removed its built-in downloader, which "became completely unusable following the introduction of SABR". It also notes "Daily YouTube RSS outages also affect the Subscription page". — [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases)
- **May–June 2026:** NewPipe 0.28.7 (late May) hotfix "to make YouTube usable again". 0.28.8 (June) "work around SABR enforcement by using another player client". NewPipeExtractor v0.26.3 (2026-06-09): "Works around SABR enforcement by switching to another player client". — [F-Droid NewPipe](https://f-droid.org/en/packages/org.schabi.newpipe/) (secondary, via search); [NewPipeExtractor releases](https://github.com/TeamNewPipe/NewPipeExtractor/releases)
- **2026-06-09:** yt-dlp raised the runtime minimums: Deno ≥2.3.0, Node ≥22, Bun deprecated (1.2.11–1.3.14 only). — [yt-dlp 2026.06.09](https://github.com/yt-dlp/yt-dlp/releases/tag/2026.06.09); [yt-dlp wiki: EJS](https://github.com/yt-dlp/yt-dlp/wiki/EJS)
- **July 2026:** web_safari HLS only for "logged-in or 'trusted' sessions"; android_vr intermittent POT enforcement (yt-dlp source comments). FreeTube 0.25.2 (2026-08-11) fixed "Reloading player according to SABR request" errors and bumped bgutils-js 3.2.0 → 4.0.2, warning the changes "may increase the likelihood of triggering YouTube's bot-protection mechanisms". — [yt-dlp _base.py](https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/yt_dlp/extractor/youtube/_base.py); [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases)
- **2026-08-17 → 08-19:** android_vr: all formats 403 (#17456, 120 comments; #17404 "100% 403 Forbidden On all YouTube Videos", opened 08-11, closed 08-17). Fixed in nightly 08-18 ("Solution: `yt-dlp --update-to nightly`"); stable 2026.08.19 added visionos and removed android_vr from the defaults. — [yt-dlp #17456](https://github.com/yt-dlp/yt-dlp/issues/17456); [yt-dlp 2026.08.19](https://github.com/yt-dlp/yt-dlp/releases/tag/2026.08.19)
- **2026-08-07 onward (open):** logged-in `tv_downgraded` UNPLAYABLE (#17389). **2026-09-10 onward (open):** mweb/web_embedded SABR-only in some sessions (#17666). — [yt-dlp #17389](https://github.com/yt-dlp/yt-dlp/issues/17389); [yt-dlp #17666](https://github.com/yt-dlp/yt-dlp/issues/17666)
- **Forced pre-roll wait (2025–2026).** yt-dlp computes `available_at` from pre-roll ad placements (summing ad duration, or the skip offset if skippable). Asked whether "NOT observing the delay until `available_at` is likely to throw a 403 error?", bashonly answered "Yes" (2026-08-18). `use_ad_playback_context` skips the wait but works only with mweb/web_music and loses Premium formats. — [yt-dlp _video.py](https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/yt_dlp/extractor/youtube/_video.py); [yt-dlp #17389](https://github.com/yt-dlp/yt-dlp/issues/17389); [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md)
- **PO-token binding.** The PO Token Guide says tokens are content-bound to "Visitor ID or account Session ID" or to the video ID; "most tokens, such as web GVS/Player, are bound to the video ID". Lifetimes are reported as "possibly as short as 12 hours", while other reports say "several months" (unresolved). rustypipe-botguard describes the player token as content-bound (video ID) and the `pot` URL param as session-bound (visitor data). Invidious docs: tokens must be generated "on the same public IP address as the one blocked by YouTube" and make the session "more easily traceable". — [yt-dlp wiki: PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide); [rustypipe-botguard README](https://codeberg.org/ThetaDev/rustypipe-botguard); [Invidious installation docs](https://docs.invidious.io/installation/)
- **IP-bound URLs.** yt-dlp FAQ: extracted URLs often must be fetched "from the same IP address and with the same cookies and/or HTTP headers"; "YouTube throttles any request with an http chunk size of > 10MB". — [yt-dlp FAQ](https://github.com/yt-dlp/yt-dlp/wiki/FAQ)
- **DRM.** The PO Token Guide says `tv` formats are "DRM'd without cookies". rustypipe main contains a `drm_license()` function ("Requires authentication"). — [yt-dlp wiki: PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide); [rustypipe player.rs](https://codeberg.org/ThetaDev/rustypipe/src/branch/main/src/client/player.rs)
- **Debunked "2026 legal threat".** Pages dated August 2026 claiming YouTube's legal team emailed Invidious are reposts of the **June 2023** cease-and-desist story; no new 2026 legal action was found. — [Tuta blog (2023 story)](https://tuta.com/blog/google-youtube-invidious-privacy-alternative); [PBX Science (misdated repost)](https://pbxscience.com/youtube-asks-open-source-apps-to-stop-serving-developers-refuse-to-obey/)

### Inferences
- **Breakage cadence:** yt-dlp shipped YouTube-related fixes in nearly every 2026 release (01-29, 01-31, 02-04, 02-21, 03-03, 03-13, 03-17, 06-09, 07-04, 08-19). Major user-facing outages happened about every 1–2 months: Jan 15, Jan 29, Mar 11, Aug 17, plus the open logged-in and SABR issues. Expect roughly 6–8 breaking events a year.
- **Recovery:**
  - yt-dlp: typically 1–3 days to nightly, 1–2 more to stable. The structural JS change of Sept–Nov 2025 took about 7 weeks but had stopgaps.
  - youtubei.js: about 1–2 weeks for the March 2026 STS event.
  - rustypipe: no release fix at all for the March 2026 deobfuscation break. Its iOS path survives only because iOS needs no JS.
- The trend is clearly toward SABR plus PO tokens as the only "official-web" path, with app identities being picked off one by one: android (2024), ios (2024–25), tv_embedded/ios_downgraded (Jan 2026), android_vr (Aug 2026).

### Gaps
- YouTube's change dates are inferred from issue-report dates; YouTube publishes nothing.
- I could not confirm the exact start date of PO-token enforcement for the WEB client (sources say "August 2024").
- I found no primary source for a "403 after N seconds of playback" rule beyond the iOS HLS "POT 30 seconds in" comment and the pre-roll wait rule.
- Grayjay's YouTube plugin breakage history for 2026 was not found.

---

## 3. Library-by-library assessment (language, license, activity, features, PO token/JS handling, break/fix record)

### Takeaway
- **yt-dlp** (Python, Unlicense) is the best-maintained and fastest-recovering extractor by a wide margin: 3 active core maintainers plus 1 maintainer plus 6 triage, monthly stable releases, daily nightlies, about 3.4M PyPI downloads a week. It now needs Deno/Node and yt-dlp-ejs.
- **youtubei.js** (TypeScript, MIT) is the most complete *library* API (search, channels, comments, account feeds, music, live chat, SABR via googlevideo, PO tokens via BgUtils). It is mostly one maintainer, and the caller must supply a JS evaluator.
- **rustypipe** (Rust, GPL-3.0) is feature-rich but effectively unmaintained since mid-2025: last release 2025-04-23, unanswered 2026 issues, 8+ unmerged community PRs. Its iOS stream path still worked in a 2026-10-09 test, but only partially.
- **Other Rust crates** are dead (rustube, ytextract, youtubei-rs, rusty_ytdl), wrappers (youtube_dl, yt-dlp), or brand-new and unproven (innertube-rs, created 2026-08-24).

### Cited Findings

**yt-dlp + yt-dlp-ejs (Python)**
- **Repo stats:** 196k stars; last push 2026-09-27; stable releases in 2026 were 01-29, 01-31, 02-04, 02-21, 03-03, 03-13, 03-17, 06-09, 07-04 and 08-19. License: Unlicense. — [GitHub API: yt-dlp](https://github.com/yt-dlp/yt-dlp/releases)
- **PyPI:** 3,442,089 downloads in the last week and 12.9M in the last month (pypistats, 2026-10-09). — [pypistats yt-dlp](https://pypistats.org/packages/yt-dlp)
- **Maintainers:** core coletdjnz, bashonly and Grub4K; maintainer doe1080; triage gamer191, garret1317, pzhlkj6612, DTrombett, grqz and InvalidUsernameException; seproDev and pukkandan inactive. — [yt-dlp Maintainers.md](https://github.com/yt-dlp/yt-dlp/blob/master/Maintainers.md)
- **Release channels:** stable is "(mostly) monthly". Nightly is "the **recommended channel for regular users**", and users must try nightly before reporting bugs. — [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md)
- **yt-dlp-ejs:**
  - Unlicense; 0.8.0 released 2026-03-17 (0.4.0 Jan → 0.5.0 Feb 21 → 0.6/0.7 Mar 13 → 0.8 Mar 17).
  - It is bundled in the official executables and the `yt-dlp[default]` extra. Each yt-dlp release pins a version and "old versions will be ignored".
  - Runtimes: Deno (recommended, enabled by default, sandboxed with no FS/network), Node ≥22, QuickJS/QuickJS-NG (pre-2025-04-26 builds "can lead to execution times of several minutes"), and Bun (deprecated).
  - Remote fetch is possible via `--remote-components ejs:npm|ejs:github`. An Apple WebKit JSC plugin (yt-dlp-apple-webkit-jsi) is "Maintained by a yt-dlp maintainer".
  - [GitHub API: yt-dlp/ejs](https://github.com/yt-dlp/ejs/releases); [yt-dlp wiki: EJS](https://github.com/yt-dlp/yt-dlp/wiki/EJS)
- **YouTube features:** search (`ytsearch:`, search URLs with filters), channels/tabs, playlists, comments (subthreads since 2026.01.29), live (live adaptive formats since 2026.07.04), Shorts, music search URLs. Cookie-only feeds: `:ytsubs`, `:ytwatchlater`, `:ythis`, `:ytfav` (liked), `:ytnotif`; `:ytrec` is recommended. — [yt-dlp supportedsites.md](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md); [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases)
- **PO tokens:** yt-dlp "cannot generate them"; they come from plugins via the PO Token Provider Framework (since 2025.05.22). Featured plugins:
  - **bgutil-ytdlp-pot-provider** (Brainicism, GPL-3.0, uses LuanRT BgUtils; "maintained by a yt-dlp maintainer"). Latest 2.0.2 (2026-10-07), 835 stars. Needs Node ≥22 or Deno ≥2.0 or Docker.
  - **yt-dlp-getpot-wpc** (coletdjnz, browser-based fallback).
  - bgutil's README cautions: "Providing a PO token does not guarantee bypassing 403 errors or bot checks."
  - bgutil **2.0.0 (2026-09-08) was a security release** for an RCE (GHSA-qpv9-8xfj-xx9m) via its default 0.0.0.0 binding; it now binds to localhost.
  - [yt-dlp wiki: PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide); [bgutil README](https://github.com/Brainicism/bgutil-ytdlp-pot-provider); [bgutil releases](https://github.com/Brainicism/bgutil-ytdlp-pot-provider/releases)
- **SABR downloader:** PR #13515 "[fd/sabr] Add YouTube SABR protocol downloader" (coletdjnz) is still open. bashonly (2026-08-21): "This is a massive PR and I need to carve out some serious time". Reviews were posted 2026-08-21 (tcely) and 2026-10-04 (gamer191); 2 approvals are needed. By default it prioritizes https over sabr, live is disabled by default, and "for web: you will need to provide a PO Token". — [yt-dlp PR #13515](https://github.com/yt-dlp/yt-dlp/pull/13515)
- **Embedding guidance:** "should be callable from any programming language… use options such as `-J`, `--print`, `--progress-template`… From a Python program, you can embed yt-dlp in a more powerful fashion". — [yt-dlp README: Embedding](https://github.com/yt-dlp/yt-dlp/blob/master/README.md)
- **mpv's ytdl_hook:** runs `yt-dlp --no-warnings -J --flat-playlist --sub-format ass/srt/best [--format …]` and builds an EDL from the separate audio and video formats. A TUI can therefore hand mpv a watch URL and let mpv call yt-dlp. — [mpv ytdl_hook.lua](https://github.com/mpv-player/mpv/blob/master/player/lua/ytdl_hook.lua)
- **Contribution policy:** yt-dlp enforces a "NO AI / NO LLM POLICY" on contributions; a bot flagged a comment in #17456 on 2026-08-18. — [yt-dlp #17456](https://github.com/yt-dlp/yt-dlp/issues/17456)
- **Local timing 2026-10-09:** `yt-dlp --flat-playlist -J "ytsearch5:…"` took 1.42 s wall; a 5-item channel `/videos` listing took 0.93 s wall (each includes Python startup). Source: local test.

**Rust crates**
- **rustypipe** (Codeberg ThetaDev/rustypipe, GPL-3.0).
  - Last release v0.11.4 on 2025-04-23. The last substantive main-branch commit was 2025-06-18 (a 2026-08-03 commit only changed a license identifier). 38 stars; crates.io 46,571 total / 18,328 recent downloads.
  - The unreleased `feat/deobf-extractor` branch (ThetaDev, 2026-06-08 → 06-12) moves the deobfuscator into a separate crate and adds "support for SABR streams without stream URL", a TV-client visitor-data fix, and a TV-with-login retry for age-restricted videos.
  - A WIP `abr-streaming` branch was last touched 2025-07-06.
  - Eight community fix PRs (raycheung, Aug 20–22, 2026; also #78 and #87) are open and unmerged. No maintainer replies to 2026 issues are visible (#73, #75, #76).
  - [Codeberg API/repo](https://codeberg.org/ThetaDev/rustypipe); [rustypipe issues](https://codeberg.org/ThetaDev/rustypipe/issues); [rustypipe PRs](https://codeberg.org/ThetaDev/rustypipe/pulls); [crates.io rustypipe](https://crates.io/crates/rustypipe)
- **rustypipe features (README):** Player (streams, subtitles), VideoDetails (metadata, comments, recommendations), Playlist, Channel (videos, shorts, livestreams, playlists, info, search), ChannelRSS, Search with filters, suggestions, Trending, URL resolver, Subscriptions, Playback history. YouTube Music: playlists, albums, artists, search, radio, lyrics, charts, library, history. The `userdata` feature covers account data. It has a report system that dumps unparseable responses. — [rustypipe README](https://codeberg.org/ThetaDev/rustypipe)
- **rustypipe JS and PO tokens:**
  - Deobfuscation runs in-process via **rquickjs (QuickJS)**: `rquickjs = "0.12.0"` on the branch, behind a `deobfuscator` feature.
  - PO tokens come from a **separate CLI, rustypipe-botguard** (MIT, v0.1.2, 2025-08-09). It uses "a stripped-down version of the Deno JavaScript runtime along with JSDOM", is auto-detected on PATH, and has a `--snapshot-file` cache. It outputs `valid_until`.
  - Client order on main (0.11.4): `[Desktop, Ios, Tv]` with botguard, else `[Ios, Tv]`. On the branch: `[Tv, AndroidVr, Desktop]` / `[Tv, AndroidVr]` / `[AndroidVr]`. The branch relies on AndroidVr, which YouTube broke on 2026-08-17.
  - [rustypipe Cargo.toml (branch)](https://codeberg.org/ThetaDev/rustypipe/src/branch/feat/deobf-extractor/Cargo.toml); [rustypipe-botguard README](https://codeberg.org/ThetaDev/rustypipe-botguard); [player.rs main](https://codeberg.org/ThetaDev/rustypipe/src/branch/main/src/client/player.rs); [player.rs branch](https://codeberg.org/ThetaDev/rustypipe/src/branch/feat/deobf-extractor/src/client/player.rs)
- **rustypipe auth:** OAuth (TV device-code flow, "only works with the TV client… you can only fetch videos and not access any user data") and cookies (fresh incognito session; the README warns that YouTube rotates cookies). Issue #61 (2025-07) "Cookie login fails: USER_SESSION_ID not found" was resolved on main. — [rustypipe README](https://codeberg.org/ThetaDev/rustypipe); [rustypipe #61](https://codeberg.org/ThetaDev/rustypipe/issues/61)
- **rustypipe 0.11.4 local smoke test 2026-10-09:**
  - Player via IOS. One video: full audio and 12 MiB of video downloaded OK. A second video: video OK, audio itag 251 → 403.
  - `channel_videos` OK (30 items); `search` OK.
  - Build time about 28 s (release).
  - Source: local test.
- **Dead or stale crates:**
  - rusty_ytdl 0.7.4 (2024-08-10; MIT/Apache); youtui ships a vendored fork, `youtui-vendored-rusty_ytdl` 0.7.4-…4 (2026-02-07), "DO NOT USE".
  - rustube 0.6.0 (2022-10-16); ytextract 0.11.2 (2023-01-29); youtubei-rs 1.3.5 (2022-07-05).
  - `invidious` crate 0.7.8 (2025-05-09; AGPL-3.0; Invidious API wrapper); `piped` 0.0.4 (2023).
  - [crates.io API](https://crates.io/crates/rusty_ytdl); [crates.io keyword youtube](https://crates.io/keywords/youtube)
- **yt-dlp wrapper crates:**
  - `youtube_dl` 0.10.0 (2024-04-16, MIT/Apache-2.0): "Runs yt-dlp and parses its JSON output"; 169k downloads.
  - `yt-dlp` (boul2gom) 2.8.3 (2026-08-17, **GPL-3.0-only**): "auto dependencies installation"; 45k downloads.
  - [crates.io youtube_dl](https://crates.io/crates/youtube_dl); [crates.io yt-dlp](https://crates.io/crates/yt-dlp)
- **Newcomers:**
  - `innertube-rs` 0.9.0 (MIT; created 2026-08-24; 5 versions; 1,176 downloads), "A fast, lightweight Rust port of YouTube.js". It embeds rquickjs for sig/n decipher and covers player, search, browse, music, comments, live chat and transcripts. The README does not mention PO tokens or SABR, and the project has no track record.
  - `ytmusicapi` (Rust, 0.5.0, 2026-07) and `ytmusic-api` (0.2.1, 2026-06) are small YouTube Music clients.
  - [innertube-rs README](https://github.com/caya8205-2/innertube-rs); [crates.io innertube-rs](https://crates.io/crates/innertube-rs)
- **Peer Rust TUIs:**
  - youtube-tui (0.9.4, 2026-03) is "like an app launcher… launches other programs… mpv… yt-dlp".
  - youtui (0.0.39, 2026-08) uses its own InnerTube client (ytmapi-rs), a native downloader (vendored rusty_ytdl) with an optional `po_token.txt`, optional `downloader_type = "YtDlp"`, and OAuth with a user-created "TVs and Limited Input devices" client.
  - [youtube-tui README](https://github.com/Siriusmart/youtube-tui); [youtui README](https://github.com/nick42d/youtui)

**youtubei.js (LuanRT, TypeScript, MIT) + BgUtils + googlevideo**
- **Repo stats:** 5,369 stars; v18.1.0 (2026-09-22), v18.0.0 (08-13), v17.2.0 (06-23), v17.1.0 (06-22), v17.0.0 (03-16), v16.0.1 (2025-10-16). There was **no release between 2025-10-16 and 2026-03-16**. npm: 177,079 downloads a week. Recent commits are mostly by LuanRT. — [GitHub releases](https://github.com/LuanRT/YouTube.js/releases); [npm youtubei.js](https://www.npmjs.com/package/youtubei.js); [commits](https://github.com/LuanRT/YouTube.js/commits/main)
- **No built-in JS interpreter.** The library "does **not** include a built-in interpreter for this purpose, so you must provide your own" (`Platform.shim.eval`). — [ytjs.dev Getting Started](https://ytjs.dev/guide/getting-started.html)
- **2026 features:** threaded comments (18.0.0), collaborators, VISIONOS, ANDROID_VR, search refinement chips, show tabs, heartbeat nodes; trending removed. 14.0.0 (2025-06) added the `is_sabr` streaming option and the TV_SIMPLY client. 15.0.0 (2025-07) added "content bound PoTokens". — [YouTube.js CHANGELOG](https://github.com/LuanRT/YouTube.js/blob/main/CHANGELOG.md)
- **Companion libraries:**
  - BgUtils (MIT) 4.0.3 (2026-08-04) is a reverse-engineered BotGuard/WAA PO-token minter.
  - googlevideo (MIT) 4.1.1 (2026-07-13; npm about 9.9k a week) handles "YouTube's custom UMP format and SABR streaming protocol".
  - [BgUtils](https://github.com/LuanRT/BgUtils); [googlevideo](https://github.com/LuanRT/googlevideo)
- **FreeTube (AGPL-3.0)** is built on youtubei.js with an Invidious-API fallback. 0.25.3-beta (2026-08-28) falls back to extracting "the potoken challenge data and config from the YouTube homepage", but "there will always be a chance that YouTube will return a CAPTCHA page". — [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases)

**NewPipeExtractor (Java, GPL-3.0)**
- v0.26.5 (2026-08-15), v0.26.4 (07-20), v0.26.3 (06-09), v0.26.2 (05-23), v0.26.1 (04-10), v0.26.0 (02-22), v0.25.x (Jan–Feb 2026). 1,999 stars.
- v0.25.0 added a custom error for "Sign in to confirm". v0.25.1 removed the TVHTML5 client and fixed "page reload required". v0.26.3 added the SABR client-switch workaround.
- The dev branch uses VISIONOS for streams and has no PO-token use "until SABR support is added". On 2026-08-20 it merged "yt_remove-broken-unused-clients".
- [NewPipeExtractor releases](https://github.com/TeamNewPipe/NewPipeExtractor/releases); [commits](https://github.com/TeamNewPipe/NewPipeExtractor/commits/dev); [YoutubeStreamExtractor.java](https://github.com/TeamNewPipe/NewPipeExtractor/blob/dev/extractor/src/main/java/org/schabi/newpipe/extractor/services/youtube/extractors/YoutubeStreamExtractor.java)

**Invidious (Crystal, AGPL-3.0) + invidious-companion (TypeScript/Deno, AGPL-3.0)**
- Invidious v2.20260804.1 (2026-08-05), 25k stars, active commits (Sept 2026).
- Companion "handle[s] all the video stream retrieval from YouTube servers". It replaces inv-sig-helper and youtube-trusted-session-generator, is built on youtube.js (cache dir `/var/tmp/youtubei.js`), and moved youtube.js to v18.0.0 on 2026-09-05. It uses a rolling `release-master` tag.
- "**Playback won't work without Invidious companion configured.**"
- Invidious docs: YouTube blocks datacenter/VPN IPs, and the remedies (IP change, IPv6 rotation, proxy) "do not guarantee" recovery.
- [Invidious releases](https://github.com/iv-org/invidious/releases); [invidious-companion](https://github.com/iv-org/invidious-companion); [Invidious installation docs](https://docs.invidious.io/installation/); [Invidious errors doc](https://docs.invidious.io/youtube-errors-explained/)

**Piped (Java backend on NewPipeExtractor, AGPL-3.0)**
- Piped-Backend was last pushed 2026-09-20. Reliability evidence for 2026 is only weak secondary commentary ("technically alive but on life support for some users"; instance counts unverified). — [GitHub API: Piped-Backend](https://github.com/TeamPiped/Piped-Backend); [sumguy.com 2026](https://sumguy.com/invidious-piped-redlib-nitter-2026/)

**Grayjay (FUTO)**
- YouTube support is an auto-updating JS plugin. The license is source-available (not OSI). No 2026 SABR/PO-token details were found. — [Grayjay GitLab tags](https://gitlab.futo.org/videostreaming/grayjay/-/tags/209)

**Go**
- kkdai/youtube (MIT): v2.10.6 (2026-03-21), v2.10.5 (2025-11-21).
- Open issues include "unexpected status code: 403" (2026-01-28), "GOOGLE_ABUSE_EXEMPTION is blocking any functionality" (2025-10-15) and "403 error again on v2.10.4" (2025-07-11).
- The maintainer opened "Proposal: Transferring This Project to a New Organization or Maintainer" (2025-05-05).
- [GitHub API: kkdai/youtube](https://github.com/kkdai/youtube/releases); [kkdai/youtube issues](https://github.com/kkdai/youtube/issues)

**ytmusicapi (Python, MIT)**
- 1.12.3 (2026-09-16); actively maintained.
- It covers YouTube Music metadata and library only, with no stream URLs. OAuth needs a user-created Google Cloud client (since Nov 2024).
- [GitHub API: ytmusicapi](https://github.com/sigma67/ytmusicapi/releases); [ytmusicapi OAuth docs](https://ytmusicapi.readthedocs.io/en/stable/setup/oauth.html)

### Inferences
- **For streams**, yt-dlp is the only option with a demonstrated 2026 record of fixing every major break within days, and with an explicit plan (SABR PR #13515 plus EJS) for the next wave.
- **For metadata** (search, channels, playlists, comments, feeds), breakage is less frequent and less severe; it is mostly parser drift such as lockupViewModel changes. A native Rust InnerTube layer is feasible there, but someone must maintain the parsers.
- **rustypipe:**
  - It would hand the user a large GPL-3.0 dependency whose single maintainer has been silent for about 15 months. That conflicts with the MIT-frontend pattern used in tuimeta, where linking a GPL crate in-process would make tuitube GPL. A separate helper process avoids that, but rustypipe offers no stdio protocol.
  - Its current stream success depends entirely on IOS, which yt-dlp dropped from its defaults in 2025 and which already shows per-itag 403s.
  - Forking rustypipe and merging the pending community PRs is possible but means taking over maintenance.
- **youtubei.js sidecar:** youtubei.js plus BgUtils plus googlevideo is the only stack offering SABR plus PO tokens as reusable library code today. But it needs a JS runtime anyway, its fixes lag yt-dlp's by about 1–2 weeks, and mpv cannot play SABR without a custom proxy that reassembles UMP into a plain stream.
- **The yt-dlp `yt-dlp` crate** is GPL-3.0-only; the `youtube_dl` crate is MIT/Apache but stale (2024), though it is only a thin JSON-schema wrapper. A hand-written `serde` model over `yt-dlp -J` is easy and avoids both problems.

### Gaps
- I did not check per-library maintainer counts beyond yt-dlp's Maintainers.md and the commit feeds.
- I found no 2026 data on youtubei.js issue volume or time-to-fix beyond the release dates.
- The Grayjay plugin and Go alternatives other than kkdai/youtube were not researched in depth.
- innertube-rs was not run: it is a new, unvetted crate, so building it was deliberately avoided.

---

## 4. Logged-in use in 2026: cookies vs OAuth, which features need login, ban warnings

### Takeaway
- OAuth/TV device login for InnerTube was killed for yt-dlp on 2024-11-18. ytmusicapi and youtui need a user-created Google Cloud OAuth client. rustypipe still documents a TV OAuth flow, but it gives video access only, not user data.
- In 2026, **cookies from a fresh incognito session are the only practical login method**.
- Login is required for the subscriptions feed, Watch Later, history, liked videos, notifications, private playlists, age-restricted and members-only videos, and Premium formats. Home recommendations work without login but are not personalized.
- yt-dlp warns that account use risks a temporary or permanent ban. I found no documented case of a ban caused purely by low-volume personal cookie use.

### Cited Findings
- **yt-dlp wiki:** "By using your account with yt-dlp, you run the risk of it being banned (temporarily or permanently). Be mindful with the request rate and amount of downloads you make with an account. Use it only when necessary, or consider using a throwaway account." Cookies are "only necessary for content that requires an account to access, such as private playlists, age-restricted videos and members-only content." — [yt-dlp wiki: Extractors → YouTube](https://github.com/yt-dlp/yt-dlp/wiki/Extractors)
- **Cookie rotation:** "YouTube rotates account cookies frequently on open YouTube browser tabs as a security measure." The workaround is to log in from a private window, navigate to `https://www.youtube.com/robots.txt`, export, then close the window "so that the session is never opened in the browser again". Do not use `--cookies-from-browser` to export these, because it captures the main profile, not the incognito one. — [yt-dlp wiki: Extractors → YouTube](https://github.com/yt-dlp/yt-dlp/wiki/Extractors)
- **OAuth is gone:** "Due to new restrictions enacted by YouTube, logging in with OAuth no longer works with yt-dlp. You should use cookies instead." Removed 2024-11-18. — [yt-dlp wiki: Extractors](https://github.com/yt-dlp/yt-dlp/wiki/Extractors); [yt-dlp Changelog](https://github.com/yt-dlp/yt-dlp/blob/master/Changelog.md)
- **Visitor data without cookies:** possible via `player_skip=webpage,configs;visitor_data=…`, but "not recommended… less stable extraction". — [yt-dlp wiki: Extractors](https://github.com/yt-dlp/yt-dlp/wiki/Extractors)
- **yt-dlp features marked "(requires cookies)":** `:ytsubs` subscriptions feed, `:ytwatchlater`, `:ythis` history, `:ytfav` liked videos, `:ytnotif` notifications. `:ytrec` (recommended) is not marked. — [yt-dlp supportedsites.md](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md)
- **Account-only and Premium formats:**
  - Premium accounts use `web_creator,tv_downgraded,web`. `web_creator` "requires sign-in for every video". GVS POT is not required for Premium.
  - Open issue #17049 (2026-06-23): "Unable to download premium quality format (721) for YouTube membership videos".
  - Age-restricted: `web_embedded` "only successfully works around the age-restriction sometimes", and `web_creator` is added "if account age-verification is required". Issue #17603 ("Add `mweb` to the list of default clients for an age-restricted video") was closed 2026-09-24.
  - [yt-dlp README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md); [yt-dlp _base.py](https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/yt_dlp/extractor/youtube/_base.py); [yt-dlp issue search 2026](https://github.com/yt-dlp/yt-dlp/issues/17049)
- **Cookies currently degrade yt-dlp extraction:**
  - 2025.10.22: "Some formats may still be unavailable, especially if cookies are passed".
  - Without a JS runtime, logged-in users are restricted "severely".
  - In 2026, the logged-in default `tv_downgraded` is broken for some users (#17389, open), and `web_embedded` fails on non-embeddable videos (#17497, open).
  - [yt-dlp 2025.10.22](https://github.com/yt-dlp/yt-dlp/releases/tag/2025.10.22); [yt-dlp #15012](https://github.com/yt-dlp/yt-dlp/issues/15012); [yt-dlp #17389](https://github.com/yt-dlp/yt-dlp/issues/17389)
- **rustypipe auth:** cookies enable "subscribed channels, playlists and your music collection" and private videos; OAuth works only with the TV client and provides no user data. rustypipe "may automatically use authentication in case a video is age-restricted or your IP address is banned". — [rustypipe README](https://codeberg.org/ThetaDev/rustypipe)
- **ytmusicapi OAuth:** "As of November 2024, YouTube Music requires a Client Id and Secret for the YouTube Data API". The user creates an OAuth client of type "TVs and Limited Input devices". youtui uses the same approach. — [ytmusicapi OAuth docs](https://ytmusicapi.readthedocs.io/en/stable/setup/oauth.html); [youtui README](https://github.com/nick42d/youtui)
- **Bans and IP bans:** the Pinchflat wiki says yt-dlp "no longer recommends using YouTube cookies since they can cause YouTube to issue IP bans", and that the risk is unclear. I found no primary report of an account terminated for low-volume personal use. (A search hit, yt-dlp #13445, turned out to be an age-restriction cookie problem, not a ban.) — [Pinchflat wiki: YouTube Cookies](https://github.com/kieraneglin/pinchflat/wiki/YouTube-Cookies); [yt-dlp #13445](https://github.com/yt-dlp/yt-dlp/issues/13445)
- **Login-free subscriptions:** FreeTube keeps subscriptions locally and fetches them via RSS or batched channel fetches (0.25.0: "Use batched fetching instead of forcing RSS with sub count > N"). It notes "Daily YouTube RSS outages" (0.24.0). rustypipe has a `ChannelRSS`/`rss` feature. — [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases); [rustypipe README](https://codeberg.org/ThetaDev/rustypipe)

### Inferences
- Lowest ban risk and highest robustness come from a **hybrid**:
  - Keep a local subscription list (importable from Google Takeout) and refresh it via RSS plus channel-tab fetches as a guest.
  - Use cookies, from a dedicated or throwaway account, only for features that truly need them (Watch Later, history, liked, members-only, age-restricted).
  - Always resolve streams for normal videos without cookies.
- Writes such as posting comments, liking, subscribing and adding to Watch Later go through authenticated InnerTube "action" endpoints. yt-dlp does not do writes at all. Only youtubei.js (and rustypipe partially) model these. A write-capable client is therefore either a youtubei.js sidecar or custom InnerTube code, carrying higher ban risk.

### Gaps
- No primary source quantifies account-ban rates for cookie use.
- No 2026 source confirms whether rustypipe's TV OAuth flow still works.
- No source covers YouTube's tolerance for write actions from non-browser clients.

---

## 5. Rate limits and ban risk for one personal user (guest vs logged in, residential IP)

### Takeaway
yt-dlp documents a soft per-session limit of about **300 videos/hour (~1,000 webpage/player requests/hour) for guests** and about **2,000 videos/hour (~4,000 requests/hour) for accounts**. A single interactive TUI user is one to two orders of magnitude below that. The real risks for a residential user are IP-reputation bot checks (rare off datacenter/VPN IPs), session-level A/B experiments that force SABR, and per-client 403 waves, not rate limits.

### Cited Findings
- **Rate limit:** "With the default yt-dlp settings, the rate limit for guest sessions is ~300 videos/hour (~1000 webpage/player requests per hour). For accounts, it is ~2000 videos/hour (~4000 webpage/player requests per hour)." The symptom is "This content isn't available, try again later"; the advice is a 5–10 s delay between downloads. — [yt-dlp wiki: Extractors → YouTube](https://github.com/yt-dlp/yt-dlp/wiki/Extractors)
- **429/402 soft blocks:** solving a CAPTCHA in a browser and passing cookies (same IP) is the documented recovery. — [yt-dlp FAQ](https://github.com/yt-dlp/yt-dlp/wiki/FAQ)
- **"Sign in to confirm you're not a bot":** primarily an IP-reputation problem (cloud/shared IPs). Invidious docs list IP change, IPv6 rotation or a proxy, "not guarantee[d]". bgutil was originally used "to bypass the 'Sign in to confirm you're not a bot' message when invoking yt-dlp from an IP address flagged by YouTube". — [yt-dlp #10128](https://github.com/yt-dlp/yt-dlp/issues/10128); [Invidious docs](https://docs.invidious.io/youtube-errors-explained/); [bgutil README](https://github.com/Brainicism/bgutil-ytdlp-pot-provider)
- **Library-level handling:** rustypipe 0.11.4 has explicit errors for "VPN ban and captcha required"; NewPipeExtractor 0.25.0 has a custom "Sign in to confirm" error. — [rustypipe commits](https://codeberg.org/ThetaDev/rustypipe/commits/branch/main); [NewPipeExtractor releases](https://github.com/TeamNewPipe/NewPipeExtractor/releases)
- **Side effects of PO-token workarounds:** FreeTube warns that its PO-token changes "may increase the likelihood of triggering YouTube's bot-protection mechanisms" and that YouTube may return a CAPTCHA page. — [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases)
- **Local smoke test 2026-10-09:** about 25 YouTube requests across yt-dlp, rustypipe and youtubei.js from one residential IP in about 15 minutes produced no bot-check or CAPTCHA. The only failures were client-specific 403s (MWEB without POT; one IOS audio itag). Source: local test.

### Inferences
- A personal TUI should cache aggressively (channel lists, video metadata, stream URLs until expiry at about 6 h), avoid prefetching streams for whole lists, and serialize player requests. These habits keep it far from the guest limit.
- Logged-in browsing raises the per-session limit, but the cost is coupling the account to any anomaly. Guest by default is the safer posture.
- Running from a VPN or datacenter (for example, a home-server Invidious on a VPS) is the main way a single user would hit bot checks.

### Gaps
- The yt-dlp numbers are community-measured, not official, and their date is unknown (possibly stale).
- There is no data on InnerTube browse/search rate limits separate from player requests.

---

## 6. Which option is most robust for a personal Rust TUI in October 2026?

### Takeaway
**Most robust: yt-dlp (official nightly or stable binary with bundled yt-dlp-ejs, plus Deno) run as a subprocess, at least for stream resolution and playback.**
- Hand stream URLs to mpv, or simply hand mpv the watch URL and let its ytdl_hook call yt-dlp.
- Use yt-dlp's `-J`/`--flat-playlist` for search, channels, playlists, comments and cookie feeds, or optionally a thin native InnerTube layer for snappy browsing.

**Second choice: a youtubei.js sidecar** (Deno/Node, NDJSON over stdio like tuimeta's Go helper). It gives the richest API (writes, live chat, SABR via googlevideo, PO tokens via BgUtils), but fixes lag yt-dlp's by about 1–2 weeks and SABR playback through mpv needs extra work.

**Not recommended as the sole backend:**
- rustypipe in-process: unmaintained since 2025, GPL-3.0, IOS-only stream path.
- A public Invidious instance: datacenter IPs are blocked and few instances survive.

A self-hosted Invidious on a residential IP works but adds Crystal, Postgres and Deno companion plus maintenance for no robustness gain over yt-dlp.

### Cited Findings
- **yt-dlp 2026 time-to-fix evidence:**
  - Jan 15 → Jan 18 (web_safari 403).
  - Jan 29 → Jan 31 (ios_downgraded 403).
  - Mar 11 → Mar 13 (STS block; android_vr kept working meanwhile).
  - Mar 17, same day (Deno n-challenge error).
  - Aug 17/18 → nightly Aug 18, stable Aug 19 (android_vr 403 → visionos).
  - Still open: logged-in tv_downgraded (since Aug 7) and session-level SABR on mweb/web_embedded (since Sep 10).
  - [yt-dlp #15569](https://github.com/yt-dlp/yt-dlp/issues/15569); [yt-dlp #16212](https://github.com/yt-dlp/yt-dlp/issues/16212); [yt-dlp #17456](https://github.com/yt-dlp/yt-dlp/issues/17456); [yt-dlp #17389](https://github.com/yt-dlp/yt-dlp/issues/17389); [yt-dlp #17666](https://github.com/yt-dlp/yt-dlp/issues/17666)
- **youtubei.js / FreeTube:**
  - March 2026 STS break: workaround Mar 5 (forced player), library fix Mar 16, app release Mar 18.
  - SABR playback shipped Apr 2026; SABR reload errors fixed Aug 11.
  - There was a five-month release gap (2025-10-16 → 2026-03-16).
  - [YouTube.js CHANGELOG](https://github.com/LuanRT/YouTube.js/blob/main/CHANGELOG.md); [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases)
- **rustypipe:**
  - No release since 2025-04-23.
  - The 2026-03-07 deobfuscation break is unanswered; the fix sits on an unreleased June 2026 branch that relies on android_vr (broken 2026-08-17).
  - Community PRs from Aug–Sep 2026 are unmerged.
  - It still worked partially via IOS on 2026-10-09 (local test).
  - [rustypipe #75](https://codeberg.org/ThetaDev/rustypipe/issues/75); [rustypipe PRs](https://codeberg.org/ThetaDev/rustypipe/pulls)
- **Invidious:** playback requires companion, and YouTube blocks datacenter/VPN IPs. Public instances were blocked in Sept 2024 (secondary source). — [Invidious installation docs](https://docs.invidious.io/installation/); [Invidious docs: errors](https://docs.invidious.io/youtube-errors-explained/); [Wikipedia: Invidious](https://en.wikipedia.org/wiki/Invidious)
- **SABR blocks direct-URL players.** FreeTube's own downloader "became completely unusable following the introduction of SABR". yt-dlp's SABR downloader is not yet merged, and it notes ffmpeg can't consume SABR. Any client that needs plain URLs for mpv therefore depends on app clients (visionos, ios) for now. — [FreeTube releases](https://github.com/FreeTubeApp/FreeTube/releases); [yt-dlp PR #13515](https://github.com/yt-dlp/yt-dlp/pull/13515)
- **Packaging pitfall:** distro or Homebrew yt-dlp packages lag and can't `-U` to nightly. The maintainers' advice on 2026-08-18 was `yt-dlp --update-to nightly`, and for Homebrew `brew uninstall yt-dlp && brew install --HEAD yt-dlp`. The user's machine currently has Homebrew yt-dlp 2026.08.19. — [yt-dlp #17456](https://github.com/yt-dlp/yt-dlp/issues/17456); local check
- **The yt-dlp runtime requirement is real:** without Deno/Node, YouTube support is "deprecated" and degrades; the minimum Deno is 2.3.0 since 2026.06.09. — [yt-dlp #15012](https://github.com/yt-dlp/yt-dlp/issues/15012); [yt-dlp 2026.06.09](https://github.com/yt-dlp/yt-dlp/releases/tag/2026.06.09)

### Inferences
**Recommended architecture:**
1. **Playback and streams** come from a managed yt-dlp. Ship or locate the official zipimport/PyInstaller binary, which bundles ejs. Require Deno ≥2.3 (or Node ≥22). Offer an in-app "update yt-dlp to nightly" action. Either let mpv's ytdl_hook resolve streams, or call `yt-dlp -J -f …` and pass the URLs plus HTTP headers to mpv from the same process/IP.
2. **Browsing** (search, channel, playlist, comments) can start on `yt-dlp --flat-playlist -J`. Latency is about 1–1.5 s per call, mostly Python startup. If that is too slow, add a persistent sidecar: a small Python process using `yt_dlp.YoutubeDL` as a library (ending the per-call startup), or a youtubei.js/Deno helper speaking NDJSON. That mirrors tuimeta's helper-process pattern and keeps the Rust front end MIT, because yt-dlp is Unlicense and youtubei.js MIT; GPL/AGPL code stays out of process.
3. **Account features:** use a dedicated account's incognito-exported cookies only for `:ytsubs`, `:ytwatchlater`, `:ythis` and `:ytfav`, and default to guest playback. Keep local subscriptions with RSS polling as the zero-risk default.
4. **Optional PO-token provider** (bgutil ≥2.0.0, localhost only) as a fallback for sessions where visionos breaks and mweb/web need tokens. Don't make it a hard dependency: the maintainers caution it does not guarantee success.
5. **Watch:** yt-dlp PR #13515 (SABR). Once merged, it removes the dependency on app-client loopholes for downloading, but mpv playback of SABR will still need yt-dlp piping (`-o -`) or a local proxy.

**Why not rustypipe-only:** it works today only because of one client (IOS) that yt-dlp abandoned in 2025. It already returns per-itag 403s, has no maintainer response in 2026, is GPL-3.0 in-process, and has no SABR/PO path in any released version. It remains an interesting *metadata* layer if forked, since its parsers mostly still work: search and channel succeeded on 2026-10-09.

### Gaps
- No longitudinal success-rate data compares the libraries; the conclusions rest on release and issue timelines plus one day of local tests.
- I did not measure mpv startup latency through ytdl_hook vs pre-resolved URLs.
- I did not test a long continuous playback session (> 10 min), live streams, or the pre-roll-wait 403 in practice.
