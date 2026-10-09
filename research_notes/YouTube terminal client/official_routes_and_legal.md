# Official routes, account access, terms and legal exposure for a personal-use third-party YouTube client (as of 2026-10-09)

Research date: 2026-10-09. Every official Google/YouTube page cited below was fetched today. Each claim carries the "last updated" or effective date the page showed. Anything I could only confirm for an earlier date is flagged.

---

## 1. YouTube Data API v3 for a personal desktop/CLI app: OAuth client types, scopes, Testing status, verification

### Takeaway
A single-user desktop app can use the official API legally. It has two client types to choose from: a "Desktop app" client using the loopback redirect with PKCE (it can request any YouTube scope), or a "TVs and Limited Input devices" client using the device flow (only `youtube` and `youtube.readonly`). Because every YouTube scope triggers the unverified-app path, the app has two practical states. In **Testing** it is free, but the user has to sign in again every 7 days. In **In production, unverified** under Google's "personal use (<100 users)" exception, the user clicks through an "unverified app" warning but gets normal long-lived refresh tokens. Formal verification for a hobby app needs a domain, a privacy policy and a demo video, which is disproportionate for one user.

### Cited Findings
**Client types and redirects**
- Desktop app client type: recommended for "macOS, Linux, and Windows desktop (but not Universal Windows Platform) apps"; set application type to "Desktop app". Page last updated 2026-09-14. — [Google: OAuth 2.0 for iOS & Desktop Apps](https://developers.google.com/identity/protocols/oauth2/native-app)
- Loopback IP redirect is "the recommended mechanism" where the platform supports it. Loopback is "DEPRECATED for Android, Chrome app and iOS OAuth client types", but desktop is not listed as deprecated. `localhost` works in place of 127.0.0.1 but "may cause issues with client firewalls". — [same](https://developers.google.com/identity/protocols/oauth2/native-app)
- "Custom URI schemes are no longer supported due to the risk of app impersonation". The manual copy/paste (OOB) method "is no longer supported" (the page links a Feb 2022 deprecation post). — [same](https://developers.google.com/identity/protocols/oauth2/native-app)
- Installed apps "cannot keep secrets". `client_secret` is optional at token exchange. PKCE is supported and `code_challenge` is "Recommended" (S256 preferred). — [same](https://developers.google.com/identity/protocols/oauth2/native-app)
- Device flow: the client must be created as "TVs and Limited Input devices" (any other type gives `invalid_client`/401). The flow "is supported only for" `email`, `openid`, `profile`, `drive.appdata`, `drive.file`, **`https://www.googleapis.com/auth/youtube`** and **`https://www.googleapis.com/auth/youtube.readonly`**. `youtube.force-ssl` and `youtube.upload` are not on the list. Refresh tokens are always returned. Incremental auth is not supported. Polling has to respect `interval`, and polling too fast returns `slow_down`. — [Google: OAuth 2.0 for TV and Limited-Input Device Applications](https://developers.google.com/identity/protocols/oauth2/limited-input-device)

**Scopes that matter for tuitube**
- `captions.download` requires `youtube.force-ssl` or `youtubepartner`, and "requires the user to have permission to edit the video". It costs 200 units. So a device-flow client can never download captions, and nobody can download captions for videos they don't own. — [YouTube Data API: Captions: download](https://developers.google.com/youtube/v3/docs/captions/download)

**Testing status, refresh-token expiry and token limits**
- A project with an external user type and publishing status "Testing" "is issued a refresh token expiring in 7 days", "unless the only OAuth scopes requested are a subset of name, email address, and user profile". Refresh tokens also die after 6 months unused. There is "a limit of 100 refresh tokens per Google Account per OAuth 2.0 client ID", and minting a 101st "automatically invalidates the oldest refresh token without warning". Page last updated 2026-05-26. — [Google: Using OAuth 2.0 to Access Google APIs](https://developers.google.com/identity/protocols/oauth2)
- Testing-status projects are "limited to up to 100 test users listed in the OAuth consent screen". Test users see a warning before granting scopes. "Authorizations by a test user will expire seven days from the time of consent", and the refresh token expires with them. — [Google Cloud Help: Manage app audience (15549945)](https://support.google.com/cloud/answer/15549945?hl=es) (found via search snippet in several language versions; I did not open the page itself)

**Verification and the unverified-app path**
- "If the app is for your personal use (fewer than 100 users) … users will be allowed to click through 'unverified app' warning screens during sign-in". "Apps in development/testing/staging mode are not subject to verification". "the 100-user cap will be in effect when an app is in development/testing/staging … This cap is removed only after an app has been successfully verified." Every app must still follow the Google API Services User Data Policy. — [Google Cloud Help: When is verification not needed](https://support.google.com/cloud/answer/13464323)
- "An unverified app is an app or Apps Script that requests a sensitive or restricted OAuth scope". Unverified apps are capped at "100 new users in total, after the app presents the unverified app screen". — [Google Cloud Help: Unverified apps](https://support.google.com/cloud/answer/7454865)
- Sensitive/restricted-scope verification requires an "appropriate use case", a demo video of the full OAuth flow, limited data use, and "the narrowest scope(s)". Restricted scopes also need "an annual security assessment". Brand verification requires a homepage on a verified domain, a matching privacy policy and Search Console domain ownership. — [Google Cloud Help: Verification requirements (13464321)](https://support.google.com/cloud/answer/13464321)
- YouTube scopes as "sensitive": Google publishes no classification table that I could find. A user who added `auth/youtube.readonly` to a consent screen was told the "consent screen requires verification by Google before it's published". Third-party integrators state that all YouTube Data API user scopes are sensitive, and none are restricted. — [Volumio community thread](https://community.volumio.com/t/plugin-youtube-for-volumio/5990?page=27); [Phyllo (vendor blog)](https://www.getphyllo.com/post/youtube-oauth-scopes)

### Inferences
- **Best official-route setup for one user:** a Desktop-app client, loopback redirect, PKCE and `youtube.readonly` (add `youtube` only if tuitube writes: subscribe, rate, playlist edits), with the project set to **In production** while staying unverified under the personal-use exception. That avoids the 7-day re-consent of Testing, at the cost of one click-through warning per consent. Keeping it in Testing works too, but means re-running the browser consent at least weekly.
- The device flow suits an SSH/headless terminal (show a code, approve on the phone), but it limits the app to `youtube`/`youtube.readonly`. It needs its own client type and has the same Testing-mode 7-day expiry.
- If tuitube is published, each user should create their **own** Google Cloud project and client ID, as many self-hosted tools do. Shipping one embedded client ID would put every user on the 100-new-user cap and in the author's quota, and would make the author the "API Client" operator under the Developer Policies.
- The 100-refresh-tokens-per-client limit is irrelevant for one user, but repeated re-auth during development silently revokes old tokens.

### Gaps
- I couldn't fetch Google's own page for the "100 test users" cap (only a search snippet), and couldn't find an official table that classifies each YouTube scope as sensitive.
- I found no Google statement on whether a "personal use" unverified app with sensitive scopes can stay in production indefinitely. The help page implies yes, below 100 users.

---

## 2. Quota: default allocation, per-call costs, and whether it is enough for a 100–300-channel subscription feed

### Takeaway
The default is 10,000 units/day plus, since **1 June 2026**, separate 100-calls/day buckets for `search.list` and `videos.insert`. Every read that matters costs 1 unit, so a 300-channel feed built from uploads playlists costs about 310 units per full refresh, or roughly 30 full refreshes a day. Search is now the scarce resource, at 100 searches a day.

### Cited Findings
- Costs (page last updated **2026-10-08**): `search.list` 1 unit per call, **capped at 100 calls/day**. `videos.list`, `subscriptions.list`, `playlistItems.list`, `playlists.list`, `activities.list`, `commentThreads.list`, `comments.list`, `channels.list` and `videos.getRating` are 1 unit each. `captions.list` is 50. `playlistItems.insert`/`update`/`delete`, `subscriptions.insert`/`delete`, `videos.rate`, `comments.insert` and `playlists.insert` are 50 each. `captions.insert` is 400 and `captions.update` 450. `videos.insert` is 1 unit, capped at 100 calls/day in its own bucket. — [YouTube Data API: Quota calculator](https://developers.google.com/youtube/v3/determine_quota_cost)
- Default allocation: "10,000 units per day combined for all endpoints other than search.list and videos.insert, plus 100 calls per day each for search.list and videos.insert". Quotas reset at midnight Pacific Time. — [same](https://developers.google.com/youtube/v3/determine_quota_cost)
- `captions.download` costs 200 units. — [Captions: download](https://developers.google.com/youtube/v3/docs/captions/download)
- Revision history: **1 June 2026**, granular quota buckets began, with `search.list` and `videos.insert` each getting their own bucket. **3 June 2026**, `videos.batchGetStats` was added (1 unit, own bucket, 10,000/day default). **4 Dec 2025**, upload cost fell from ~1,600 to ~100 units. **1 July 2021**, projects that pass a compliance audit can exceed 10,000 units. — [YouTube Data API: Revision history](https://developers.google.com/youtube/v3/revision_history)
- A quota extension requires "an API Compliance Audit" (Developer Policies III.D.3), and the API ToS forbid attempts to "exceed or circumvent use or quota restrictions" (§15). — [Developer Policies](https://developers.google.com/youtube/terms/developer-policies); [API Services ToS](https://developers.google.com/youtube/terms/api-services-terms-of-service)

### Inferences
- **Feed arithmetic for 300 channels:** `subscriptions.list?mine=true` at 50 per page is 6 units. Resolving uploads-playlist IDs through `channels.list` (50 IDs per call) is 6 units, once, then cached. `playlistItems.list` on each channel's uploads playlist is 300 units. `videos.list` for durations, live status and Shorts heuristics on new items is about 1–6 units. That makes **≈310 units per full refresh**, so ≈32 full refreshes a day, about one every 45 minutes. For 100 channels it is ≈105 units, so ≈95 refreshes a day. Staggered refreshes (hot channels more often) or RSS-first with the API as backfill stretch this further.
- Search is the binding limit: 100 `search.list` calls a day, now separate from the 10,000 pool. A search-heavy TUI needs caching or another search path (RSS can't search).
- Writes are expensive at 50 units: 10 subscribes plus 20 playlist edits plus 20 ratings is 2,500 units.
- One user can't realistically pass a compliance audit for more quota, and doesn't need to.

### Gaps
- The quota page doesn't say whether the per-call `search.list` cap counts paginated calls separately. I assumed each call counts.

---

## 3. What the Data API cannot do (Watch Later, history, home feed, subscriptions feed, stream URLs, others' captions)

### Takeaway
The official API covers metadata, your subscriptions and playlists, comments and ratings. It has none of the things that make a client feel like YouTube: no watch history, no Watch Later, no home/recommendations, no subscriptions-feed endpoint, no media streams, and no captions for other people's videos. All of these have been gone since 2016 or were never offered. Playback under the API terms means the embedded YouTube player, which a terminal can't host.

### Cited Findings
- **Watch history and Watch Later:** deprecation announced 11 Aug 2016, effective 12 Sept 2016. From 15 Sept 2016, `relatedPlaylists.watchHistory`/`watchLater` return the fixed values `HL`/`WL`, and `playlistItems.list` on them "will return an empty list". On 9 Sept 2020, `playlistItems.insert`/`delete` support for them was fully deprecated and the properties removed from docs. On 28 Jan 2021, the `watchHistoryNotAccessible`/`watchLaterNotAccessible` errors were removed from docs. — [Revision history](https://developers.google.com/youtube/v3/revision_history)
- **Home feed:** `activities.list` `home` parameter deprecated 12 Sept 2016. From 15 Sept 2016, `home=true` returns items "similar to what a logged-out user sees". On 2 Nov 2016, the `homeParameterDeprecated` (403) error was added. — [same](https://developers.google.com/youtube/v3/revision_history)
- **Subscriptions feed:** the revision history has no entry for any subscriptions-feed method. Related `activities` changes: channel bulletins were removed 4 June 2020, and `contentDetails.like`/`favorite` were removed from activities docs on **6 Aug 2026**. — [same](https://developers.google.com/youtube/v3/revision_history)
- **Captions:** `captions.download` "requires the user to have permission to edit the video" (scope `youtube.force-ssl`), at 200 units. — [Captions: download](https://developers.google.com/youtube/v3/docs/captions/download)
- **Streams and playback:** the API ToS grant "no rights or licenses … to reproduce or distribute audiovisual content" (§16.3). The Developer Policies forbid downloading or caching AV content without written approval (III.E.1), modifying or blocking any part of a YouTube player (III.I.6), separating audio and video (III.I.7), background playback (III.I.9), blocking ads (III.I.5), using "any technology other than YouTube API Services to access or retrieve API Data" (III.I.14), and "undocumented APIs without express permission" (III.D.7). Last updated 2026-09-14. — [API Services ToS](https://developers.google.com/youtube/terms/api-services-terms-of-service); [Developer Policies](https://developers.google.com/youtube/terms/developer-policies)
- **Embedded player rules** (RMF, last updated 2026-09-14): embedded players need a viewport of "at least 200px by 200px". No overlays "in front of any part of a YouTube embedded player". No changes to the player "not explicitly described by the API documentation". Desktop apps "must provide identification through the HTTP Referer request header", using their app ID such as a reverse-DNS string, and should use OS WebViews. Autoplay may start only when ">half of the player is visible". — [Required Minimum Functionality](https://developers.google.com/youtube/terms/required-minimum-functionality)
- The Developer Policies contain no single sentence saying "play only via the embedded player"; III.I.6/III.I.14 come closest. — [Developer Policies](https://developers.google.com/youtube/terms/developer-policies) (per my fetch of the page)
- **Official ways to get history and Watch Later data out:** Google Takeout gives `subscriptions.csv` and `watch-history.json`/`.html` (see §6). The Google **Data Portability API** has a `dataportability.myactivity.youtube` scope ("Move a copy of your YouTube activity") and a subscriptions scope, but it "provides access to sensitive or restricted data scopes" that need app verification and possibly a security assessment. Its stated purpose is EEA users under the DMA, and availability depends on the user's country. — [Data Portability API](https://developers.google.com/data-portability); [Data Portability policy](https://developers.google.com/data-portability/policy); [scopes guide](https://developers.google.com/data-portability/user-guide/scopes?hl=en)

### Inferences
- An official-API-only tuitube would be a **metadata browser**: subscriptions list, an uploads-based feed, search (100/day), playlists you own or that are public, comments, likes and subscribes. Playback would have to hand off to a browser or the official app via a `youtube.com/watch?v=` URL. Piping streams to mpv is outside the API terms, because it means using undocumented endpoints, a non-YouTube player and possibly blocked ads.
- Watch Later and history can only be **imported** once (Takeout or Data Portability) or kept locally by tuitube. They can't be synced back.
- Shorts filtering isn't an API field. It needs heuristics such as duration, or the unofficial `UULF` uploads-playlist prefix (see §6).

### Gaps
- I didn't fetch the full Data API method index to show that no "subscription feed" method exists. That conclusion rests on the revision history and long-standing behavior.
- I didn't confirm the Data Portability API's current country list or the exact subscriptions scope string.

---

## 4. YouTube API Services Terms, Developer Policies, and YouTube's user Terms of Service

### Takeaway
Both rulebooks forbid what an unofficial client does. The API terms forbid downloading, caching AV content, separating audio, background play, ad blocking, scraping and undocumented APIs. The user ToS (effective **15 Dec 2023**) forbid accessing or downloading content except as the Service authorizes, interfering with any part of the Service, and access "using any automated means". The ToS remedy is suspension or termination of the Google account for material or repeated breach. The Paid Service Terms (effective **16 Mar 2026**) separately forbid accessing Premium "other than by means authorized by Google".

### Cited Findings
**Developer Policies** (last updated **2026-09-14**) — [Developer Policies](https://developers.google.com/youtube/terms/developer-policies)
- III.E.1.a: must not "download, import, backup, cache, or store copies of YouTube audiovisual content without YouTube's prior written approval". III.E.1.b also bars offline playback.
- III.I.7: must not "separate, isolate, or modify the audio or video components of any YouTube audiovisual content". III.I.8 bars promoting the components separately.
- III.I.9: must not "create, include, or promote features that play content, including audio or video components, from a background player".
- III.I.5: must not "modify, interfere with, replace, or block advertisements placed or served by YouTube". III.I.6: must not "modify, build upon, or block any portion or functionality of a YouTube player".
- III.E.4.c: stored API data kept "for no longer than 30 calendar days. After 30 calendar days, the API Client must either delete or refresh the stored data". III.E.4.b: non-authorized statistics also max 30 days. III.D.2: delete authorized data "within 7 calendar days of the revocation". III.E.4.g: user deletion requests within 7 days.
- III.F.2.a/c: "make clear to the viewer that YouTube is the source" and "display applicable YouTube Brand Features and any other YouTube-provided attribution".
- III.D.3: quota extension through "API Compliance Audit". III.H: YouTube may "survey, monitor, and/or audit".
- III.E.6: must not "scrape YouTube Applications". III.D.7: "You must not use undocumented APIs without express permission". III.I.14: must not use "any technology other than YouTube API Services to access or retrieve API Data".
- III.G: "You may distribute or sell API Clients" (subject to restrictions). There is no personal-use carve-out.
- III.C.1: must comply with the Required Minimum Functionality.

**API Services Terms of Service** (last updated **2026-09-14**) — [API Services ToS](https://developers.google.com/youtube/terms/api-services-terms-of-service)
- §6: YouTube "may monitor, review and inspect your API Client(s)" "without further notice". §24.2: may "suspend or terminate access" with no obligation to notify. §16.3: "no rights or licenses are granted to reproduce or distribute audiovisual content". §15: no quota circumvention. §3.1(ii): access only "in accordance with the documentation".

**YouTube Terms of Service** (**"Effective as of December 15, 2023"**) — [YouTube ToS (en-US)](https://www.youtube.com/t/terms?hl=en&gl=US)
- Permissions & Restrictions: you may not "access, reproduce, download, distribute, transmit, broadcast, display, sell, license, alter, modify…" the Service or Content unless "expressly authorized by the Service" or with prior written permission. You may not "circumvent, disable, fraudulently engage with, or otherwise interfere with any part of the Service". You may not "access the Service using any automated means (such as robots, botnets or scrapers) except (a) in the case of public search engines, in accordance with YouTube's robots.txt file" or (b) with prior written permission.
- Terminations: "YouTube reserves the right to suspend or terminate your Google account or your access to all or part of the Service if (a) you materially or repeatedly breach this Agreement; (b) … legal requirement or a court order; or (c) … conduct that creates (or could create) liability or harm…".
- The Vietnamese-locale copy I was served first shows an effective date of **5 Jan 2022**, so localized versions may lag. — [YouTube ToS (served in vi)](https://www.youtube.com/t/terms)

**YouTube Paid Service Terms** (**"Effective as of March 16, 2026"**) — [Paid Service Terms](https://www.youtube.com/t/terms_paidservice?hl=en)
- §6 prohibits accessing "the Paid Services other than by means authorized by Google". §2.3: "will not attempt to circumvent any restrictions on access to or availability of the Paid Services". Premium "may vary by geographical location, device, and operational system", and individual plans can't be shared.

### Inferences
- An unofficial (InnerTube/yt-dlp-style) client breaches the **user ToS** whether or not it is logged in. It "accesses" and "downloads" stream data outside the player, and arguably interferes with ads. The API Developer Policies bind only developers who accept the API ToS. Invidious argued exactly this in 2023 (see §7).
- The only hard contractual sanction against an individual user is account suspension or termination. Logged-out use takes the account off the table.
- An official-API tuitube must show YouTube attribution and branding, refresh or delete cached metadata within 30 days, and offer data deletion on revocation. For a local single-user cache, that means a 30-day TTL on stored API data.

### Gaps
- I couldn't retrieve the exact text of the April 2024 "Enforcement on Third Party Apps" forum post. Quotes in §5 come from press coverage.

---

## 5. Enforcement 2023–2026: ad-block crackdown, bot checks, third-party client breakage, account risk, cookie login, Premium

### Takeaway
YouTube enforces against **traffic, not people**. It uses playback blocks, "Sign in to confirm you're not a bot", PO tokens, SABR-only streams, JS challenges and IP and datacenter blocks. I found **no confirmed case of a Google account being terminated solely for using yt-dlp, NewPipe, FreeTube, Invidious or SmartTube**, only warnings and anecdotes. yt-dlp itself warns that logging in risks a temporary or permanent ban. YouTube's own OAuth has been closed to these tools since November 2024, so logging in means exporting browser cookies. That is fragile because YouTube rotates them, and it is risky because it ties automated traffic to the user's real Google identity.

### Cited Findings
**Ad-blocker crackdown**
- June 2023: YouTube began disabling playback for ad-block users as a "small experiment globally". Around **31 Oct 2023** it confirmed a "global effort" to get users to allow ads or buy Premium, and said ad blockers violate its ToS (statement by Christopher Lawton to The Verge). — [Tom's Guide](https://www.tomsguide.com/news/youtubes-anti-ad-block-efforts-have-now-gone-global-what-you-need-to-know); [iPhone in Canada, 31 Oct 2023](https://www.iphoneincanada.ca/2023/10/31/youtube-ad-blockers-global/)
- **15 Apr 2024:** a YouTube Community post, "Enforcement on Third Party Apps", said YouTube was strengthening enforcement against third-party apps that block ads. Users may see buffering or "The following content is not available on this app". Third-party apps may use the API only if they follow the API ToS, and "the only way" to go ad-free is Premium. — [9to5Google, 15 Apr 2024](https://9to5google.com/2024/04/15/youtube-app-block-ads/); [Gigazine](https://gigazine.net/gsc_news/en/20240416-youtube-ad-blocker-crackdown-third-party-apps/)
- **2026 (weakly sourced):** reports from Feb 2026 say YouTube hides comments and descriptions for ad-block users. One report describes a warning citing suspension for "repeatedly or egregiously" violating policies, while noting no reports of bans solely for ad blocking. — [GamingBible, Feb 2026](https://www.gamingbible.com/news/youtube-update-ad-blocker-restrictions-061377-20260218); [SecurityOnline](https://securityonline.info/googles-new-youtube-warning-no-ad-blockers-or-lose-your-account/); [Ghostery](https://www.ghostery.com/blog/whats-happening-with-youtube-ads)

**Bot checks, PO tokens, SABR and JS challenges (logged-out breakage)**
- From about mid-2024, "Sign in to confirm that you're not a bot" hit Invidious, Piped, Cobalt and NewPipe users, apparently A/B tested. FreeTube was reportedly less affected at the time. — [Techlore forum thread](https://discuss.techlore.tech/t/youtube-breaks-third-party-clients-once-again/8944)
- Invidious docs: YouTube runs detection against non-official clients and "block[s] datacenter and VPN IP addresses". The fix is a PO token plus visitor-data pair generated from the same public IP. — [Invidious: YouTube errors explained](https://docs.invidious.io/youtube-errors-explained/)
- yt-dlp wiki (edited 11 Jun 2025): "YouTube is gradually enforcing the use of a 'PO Token'… yt-dlp cannot generate them". The guest rate limit is "~300 videos/hour (~1000 webpage/player requests per hour)", and the account limit "~2000 videos/hour". It recommends a 5–10 s delay between downloads. — [yt-dlp wiki: Extractors](https://github.com/yt-dlp/yt-dlp/wiki/Extractors)
- yt-dlp announced in Sept 2025 (issue #14404) that YouTube support would need an external JS runtime. From **yt-dlp 2025.11.12 (12 Nov 2025)** one is strongly recommended. Deno is the default, and Node, Bun and QuickJS are supported. Without one, formats may be limited. — [Gigazine, 13 Nov 2025](https://www.gigazine.net/gsc_news/en/20251113-yt-dlp-required-deno-javascript-runtime)
- Nov 2025: a YouTube rollout removed `adaptiveFormats` URLs from the web client's player response, leaving only the SABR streaming URL. yt-dlp warns "YouTube is forcing SABR". yt-dlp 2026.03.13 shipped yt-dlp-ejs 0.7.0 to fix n/sig challenge solving. — [Mageia advisory MGAA-2025-0098](https://advisories.mageia.org/MGAA-2025-0098.html); [VideoHelp forum](https://forum.videohelp.com/threads/419212-YouTube-SABR-Protection); [VRChat feedback thread](https://feedback.vrchat.com/feature-requests/p/update-to-yt-dlp-20260313-yt-dlp-ejs-070-to-improve-youtube-playback-compatibili)
- By Aug 2025, ProtonVPN dropped Piped from its alternatives list "because it appears to no longer work". — [ProtonVPN blog (zh-tw, update note Aug 2025)](https://protonvpn.com/zh-tw/blog/youtube-alternatives)
- 2026 user reports: "NewPipe breaks every few weeks". Invidious gets "blocked easily". Watching several videos in a row can soft-block an IP for about an hour (anecdotal). — [HN discussion](https://news.ycombinator.com/item?id=47020218)

**Logged-in use, cookies and account risk**
- yt-dlp: "By using your account with yt-dlp, you run the risk of it being banned (temporarily or permanently)… Use it only when necessary, or consider using a throwaway account". "YouTube rotates account cookies frequently on open YouTube browser tabs as a security measure". It recommends exporting cookies from a private window that is never reopened. — [yt-dlp wiki: Extractors](https://github.com/yt-dlp/yt-dlp/wiki/Extractors)
- yt-dlp **2024.11.18** shipped "youtube: remove broken OAuth support" (#11558): YouTube changes stopped the TV-client OAuth login from working, and cookies are now the only login. — [openSUSE yt-dlp changelog](https://packagehub.suse.com/update-infos/openSUSE-2025-127/); [commit mirror](https://bytes.keithhacks.cyou/etc/yt-dlp/commit/52c0ffe40ad6e8404d93296f575007b05b04c686)
- Account-ban evidence is anecdotal only. A mid-2024 Linux Mint forum post claims cookie users were "banned" (no names, dates or Google statement). Searches found no Google statement linking yt-dlp or cookie use to bans. — [Linux Mint forum](https://forums.linuxmint.com/viewtopic.php?t=421647)
- **SmartTube compromise (late Nov–early Dec 2025):** malware (`libalphasdk.so`) was injected into official builds, apparently via an infected build machine or stolen signing key. Play Protect disabled the app, Amazon pulled it from Fire TV, and users were told to review their Google accounts. The clean build was v30.56. — [BleepingComputer](https://bleepingcomputer.com/news/security/smarttube-youtube-app-for-android-tv-breached-to-push-malicious-update); [PCWorld](https://www.pcworld.com/article/2997507/malware-found-in-popular-smarttube-app-on-smart-tvs-heres-what-to-do-about-it.html); [Android Authority](https://www.androidauthority.com/smarttube-malware-fix-3620773/)

**YouTube Premium in unofficial clients**
- In its April 2024 third-party-app enforcement message, YouTube pointed users who want an ad-free experience to Premium as the sanctioned route. — [9to5Google](https://9to5google.com/2024/04/15/youtube-app-block-ads/)
- "1080p Premium" (enhanced bitrate, about 13 vs 8 Mbps in one test) reached Premium desktop users in 2023. In yt-dlp it can appear as format **616** (Premium) when Premium-account cookies are passed, but other users couldn't see it, and a newer 1080p60 Premium A/B variant doesn't appear at all. Anecdotal. — [Engadget](https://www.engadget.com/youtubes-enhanced-1080p-playback-option-is-rolling-out-to-premium-users-on-the-web-130058566.html); [VideoHelp forum](https://forum.videohelp.com/showthread.php?p=2785676)
- Paid Service Terms §6 forbid accessing the Paid Services "other than by means authorized by Google" (effective 16 Mar 2026). — [Paid Service Terms](https://www.youtube.com/t/terms_paidservice?hl=en)

### Inferences
- **Account-ban risk ranking** (highest to lowest): cookie-based logged-in unofficial access with heavy or automated traffic (downloads, bulk feed polling through InnerTube) > cookie login with light interactive viewing > logged-out unofficial access (no account at stake, only IP soft-blocks) > official Data API with OAuth (sanctioned; the worst case is losing API access or quota).
- Premium is an account flag, so a logged-in unofficial client may get ad-free responses and Premium formats. Premium doesn't make the client authorized, though: the Paid Service Terms forbid non-authorized access, so it carries no legal or ToS protection. Logged-out clients can't use Premium at all.
- A cookie jar from the user's main Google account is a high-value secret: it grants full account access beyond YouTube. tuitube's security rules would need OS-keystore storage, no logging, and ideally a dedicated YouTube-only (Brand/secondary) account. The SmartTube incident shows the supply-chain risk when a client holds account credentials.
- Logged-out reliability now depends on keeping up with PO tokens, SABR, n/sig JS challenges and a JS runtime (Deno). That is a maintenance treadmill only large projects (yt-dlp, YouTube.js) keep pace with.

### Gaps
- I found no verified report from 2023–2026 of a Google account terminated **solely** for third-party-client use, and no official YouTube statement on cookie-based login from third-party apps.
- I couldn't access the April 2024 forum thread or the 2026 warning screenshots directly.
- Whether Premium accounts avoid PO-token or SABR friction in unofficial clients is undocumented.

---

## 6. Channel RSS feeds and Google Takeout as no-auth / import paths

### Takeaway
`https://www.youtube.com/feeds/videos.xml?channel_id=UC…` (and `?playlist_id=…`) still works in Oct 2026 and needs no key or account. It returns only about the **15 most recent** public uploads, mixes in Shorts (the `UULF` playlist trick filters most of them, unofficially), and is undocumented. It had real reliability problems: HTTP 500s from Oct 2025, intermittent 404s from Dec 2025, and daily multi-hour outages and stale or republished entries reported in May 2026. It was reportedly stable by May 2026, with occasional dips. Takeout gives a one-time `subscriptions.csv` import.

### Cited Findings
- Feed format and limits: the channel-ID form works, while `@handle` URLs don't resolve to feeds. The feed caps at about the 15 most recent uploads (one older article said 10). Private and unlisted videos never appear, and Shorts share the feed. — [WP RSS Aggregator feed finder](https://finder.wprssaggregator.com/rss-feeds/platform/youtube); [Gingerbeardman blog, 9 Jan 2023](https://blog.gingerbeardman.com/2023/01/09/working-around-the-youtube-channel-rss-limit); [Vivaldi forum](https://forum.vivaldi.net/post/636036)
- Outages: NewsBlur users reported feeds returning HTTP 500, and adding feeds stopped working on **25 Oct 2025**. One aggregator reports "intermittent 404s starting December 2025" that appeared stable "as of May 2026" while an uptime tracker still showed dips. A Firefox add-on claims the endpoint was "broken globally since early 2026" and falls back to scraping (single-vendor claim, conflicting). — [NewsBlur forum](https://forum.newsblur.com/t/youtube-feeds-broken/13227); [WP RSS Aggregator](https://finder.wprssaggregator.com/rss-feeds/platform/youtube); [TubeFeed add-on](https://addons.mozilla.org/firefox/addon/tubefeed/); [Techlore forum](https://discuss.techlore.tech/t/suddenly-lost-access-to-youtube-rss-feeds/6963)
- "YouTube, your RSS feeds are broken" (openrss.org) reached HN in about **May 2026** (340 points). Commenters reported outages of several hours at about the same time daily, Shorts included, old videos republished with new dates, and unreliable livestream entries. The `UC`→`UULF` playlist-ID trick lists "only normal videos" (some Shorts leak; unofficial). `UUSH` was suggested for Shorts-only but unverified. There was no YouTube or Google comment. — [HN item 48030964](https://news.ycombinator.com/item?id=48030964) (the openrss.org article URL returned 404 when I fetched it)
- Takeout: choose YouTube, then "subscriptions", to get `subscriptions.csv` (path like `YouTube and YouTube Music/subscriptions/subscriptions.csv`; names vary by locale). It is a CSV with a header row; community parsers read col 0 = channel ID and col 2 = title (I couldn't confirm the full header from an official source; col 1 is presumably the channel URL). Watch history comes only as JSON or HTML (`watch-history.json`). — [Invidious docs: export subscriptions](https://docs.invidious.io/export-youtube-subscriptions/); [takeoutday.org guide](https://takeoutday.org/guides/how-to-export-youtube-data); [community gist](https://gist.github.com/jeosadn/e52366441add782acc25043622e468a9)
- Data Portability API is an official programmatic alternative for history and subscriptions, but it needs app verification (see §3). — [Data Portability API](https://developers.google.com/data-portability)

### Inferences
- **RSS-first feed design:** poll the RSS feed per channel with ETag/If-Modified-Since, jitter and a polite interval of ≥15–30 min. Use the `UULF…` playlist feed to drop most Shorts. Fall back to the Data API's `playlistItems.list` (1 unit) for backfill beyond 15 items and during RSS outages. That keeps API quota for search and metadata.
- RSS is the lowest-risk accountless path. The URL is public, linked from channel pages and made for feed readers, and nothing is circumvented. It is still "automated access" under the literal ToS, but feed polling at a human scale is how every feed reader works. It isn't covered by the API ToS or the 30-day caching rule, because it isn't a "YouTube API Service" (inference; no source addresses this).
- Takeout covers onboarding: import `subscriptions.csv` once, then keep subscriptions locally with no account at all.

### Gaps
- No official YouTube documentation or status page covers the RSS endpoint. Its continued existence is unguaranteed, and I found no deprecation notice.
- I couldn't confirm whether the 15-item cap applies equally to `playlist_id` feeds.

---

## 7. Legal exposure: youtube-dl takedown, Uberspace, Yout, Invidious and NewPipe actions, GitHub DMCA notices 2023–2026, and personal vs published use

### Takeaway
Legal action has targeted **distributors and hosts of download/circumvention tools** (RIAA vs youtube-dl on GitHub in 2020, the labels vs Uberspace in Germany 2022–2024, RIAA vs Yout in the US, still pending), plus one YouTube contract-based C&D to a **hosted** frontend (Invidious, June 2023). I found no action against an individual end user, and no GitHub DMCA notice in 2023–Oct 2026 against yt-dlp, NewPipe, Invidious, Piped, FreeTube, rustypipe or YouTube.js. The key legal hook is the "rolling cipher" / signature as a technological protection measure. A German appeals court accepted that in Nov 2024 and the US question is unresolved. Its trafficking prongs (§1201(a)(2)/(b); §95a(3) UrhG) bite on **publishing** a tool, and a stream-only design doesn't avoid the decipher step.

### Cited Findings
**youtube-dl / RIAA (US, 2020)**
- GitHub removed youtube-dl on **23 Oct 2020** after an RIAA DMCA notice framed under §1201 (circumvention), which also cited unit tests that referenced copyrighted songs. — [BleepingComputer](https://www.bleepingcomputer.com/news/software/youtube-dl-removed-from-github-after-riaa-dmca-notice/)
- EFF's letter of **15 Nov 2020** argued that YouTube's "signature" code ("rolling cipher") "isn't a protected digital lock", and that if it were, youtube-dl "doesn't 'circumvent' it but simply uses it as intended". It added that youtube-dl doesn't decrypt DRM such as Widevine, and that the unit-test references were fair use. — [EFF Deeplinks](https://www.eff.org/deeplinks/2020/11/github-reinstates-youtube-dl-after-riaas-abuse-dmca); [EFF letter PDF](https://kittens.eff.org/files/2020/11/17/eff_letter_to_github_re_youtube-dl_11152020.pdf). A secondary summary of the letter dates the RIAA demand to 21 Sept 2020, which conflicts with the 23 Oct takedown date; the notice may have been sent earlier than GitHub processed it.
- GitHub reinstated the repo on **16 Nov 2020**. It announced technical and legal expert review of every §1201 claim, a default to "err on the side of the developer, and leave up the repository unless there is clear evidence of illegal circumvention", notice to owners before takedown, and a **$1M developer defense fund**. — [GitHub blog, 16 Nov 2020](https://github.blog/news-insights/policy-news-and-insights/standing-up-for-developers-youtube-dl-is-back/)

**Uberspace (Germany, 2022–2024)**
- The German arms of Sony, Universal and Warner sued Uberspace in early 2022 for hosting youtube-dl.org, which only **linked** to the GitHub code. LG Hamburg ruled against Uberspace in **March 2023** (one source gives the reference 310 O 316/21). Uberspace complied in **Aug 2023** after the labels posted a €20,000 deposit, facing a €250,000 fine or jail if it didn't. — [heise, 27 Nov 2024](https://heise.de/-10179284); [Gigazine, 29 Nov 2024](https://gigazine.net/gsc_news/en/20241129-youtube-dl-hosting-provider-uberspace); [ferner-alsdorf commentary](https://www.ferner-alsdorf.de/entscheidung-des-olg-hamburg-zu-youtube-dl/)
- **OLG Hamburg, 21 Nov 2024, 5 U 54/23** dismissed the appeal. It held YouTube's "rolling cipher" is "an effective measure" (an average user can't bypass it with browser dev tools), said other uses of the tool are irrelevant, and found that youtube-dl users act in "bad faith" because the multi-step process shows them a protection is being circumvented. "The OLG has not allowed an appeal" (no Revision). — [heise, 28 Nov 2024](https://heise.de/-10181247); [heise, 27 Nov 2024](https://heise.de/-10179284)
- Uberspace and GFF were weighing a Nichtzulassungsbeschwerde (complaint against non-admission of appeal) as of 28 Nov 2024. I found no report of a filing or a BGH decision through Oct 2026, and GFF's case page shows no later update. — [netzpolitik.org, 28 Nov 2024](https://netzpolitik.org/2024/entscheidung-des-olg-hamburg-youtube-dl-org-bleibt-gesperrt/); [GFF case page](https://freiheitsrechte.org/en/themen/demokratie/uberspace-youtube-dl-eng)
- §95a(1) UrhG bars circumventing effective TPMs without consent where the actor knows or must know the purpose. That covers the act itself, with no personal-use exemption in the provision. §95a(3) bars the manufacture, import, distribution, sale, rental and advertising of circumvention devices, and possession for commercial purposes. — [§95a UrhG](https://www.gesetze-im-internet.de/urhg/__95a.html)

**Yout v. RIAA (US, pending)**
- D. Conn. (Judge Underhill, late 2022) dismissed Yout's declaratory suit, finding it didn't plausibly allege that it avoids circumventing "the YouTube TPM" under §1201(a), and called repleading futile. The 2nd Circuit appeal **22-2760** was argued **5 Feb 2024**, with GitHub and EFF as amici. In Oct 2025 the court allowed a Suno/Udio amicus brief, with supplemental briefs due 10 Nov 2025. After *Cox v. Sony* (SCOTUS, decided **25 Mar 2026**) the parties exchanged letters. The docket shows a last filing of 31 Mar 2026 and **no decision as of its 9 Sept 2026 update**. — [TorrentFreak, 2 Oct 2022](https://torrentfreak.com/riaa-thwarts-youts-attempt-to-declare-youtube-ripping-legal-221002/); [TorrentFreak, 9 Feb 2024](https://torrentfreak.com/appeals-court-hears-riaa-and-yout-in-high-stakes-streamripper-case-240209/); [TorrentFreak, Oct 2025](https://torrentfreak.com/suno-udio-wade-into-youtube-ripper-circumvention-lawsuit-appeal-251008/); [TorrentFreak on Cox letters](https://torrentfreak.com/yout-com-hopes-supreme-courts-cox-ruling-helps-its-case-riaa-disagrees/); [CourtListener docket](https://www.courtlistener.com/docket/66697744/yout-llc-v-recording-industry-association-of-america-inc/)
- 17 U.S.C. §1201(a)(1)(A): "No person shall circumvent a technological measure that effectively controls access to a work". (a)(2) and (b)(1) bar anyone who would "manufacture, import, offer to the public, provide, or otherwise traffic" in circumvention tech. §1201(b) (copy controls) bans trafficking only, not the act itself. — [17 U.S.C. §1201 (Cornell LII)](https://www.law.cornell.edu/uscode/text/17/1201)

**YouTube vs frontends, and music labels vs NewPipe**
- **8 June 2023:** YouTube's legal team emailed Invidious that it appeared to violate the YouTube API Services ToS and Developer Policies (citing ad suppression and downloads) and asked it to stop within 7 days. Invidious replied that it "never agreed" to those terms and doesn't use the API, declined to comply, and kept running. — [Wikipedia: Invidious](https://en.wikipedia.org/wiki/Invidious); [AlternativeTo, June 2023](https://alternativeto.net/news/2023/6/youtube-legal-team-asked-invidious-developers-to-take-down-the-service-within-7-days); [Michael Tsai, 12 Jun 2023](https://mjtsai.com/blog/2023/06/12/youtube-tries-to-shut-down-invidious)
- **July 2023:** a DMCA notice from French label Because Music got newpipe.net delisted from Google Search in some regions, apparently over NewPipe's download feature. — [Gigazine, 13 Jul 2023](https://gigazine.net/gsc_news/en/20230713-newpipe-dmca-google-search)

**GitHub DMCA repository, 2023 to 9 Oct 2026** (my own grep of a sparse clone of github/dmca for 2023–2026: no notice names yt-dlp, youtube-dl as a target, Invidious, NewPipe/NewPipeExtractor, Piped, FreeTube, rustypipe, rusty_ytdl, YouTube.js/youtubei.js or SmartTube)
- **6 Mar 2024, Google:** a copyright (not §1201) notice; the TPM question was answered "No". It claimed the "YouTube (Main) app in the Apple App Store" and targeted release IPAs of modified YouTube iOS apps: uYouEnhanced, uYouPlus, iVanced, YTLitePlus, YouTubeRebornPlus, YTKillerPlus, YTMusicUltimate and others. — [github/dmca 2024-03-06-google.md](https://github.com/github/dmca/blob/master/2024/03/2024-03-06-google.md)
- **24 Jun 2025 and 7 Jul 2025, Google:** explicit anti-circumvention (§1201) notices against Play Integrity "pairip" protection removers (void-eth/pairip-protection-remover, TechnoIndian/RKPairip). — [2025-06-24-google.md](https://github.com/github/dmca/blob/master/2025/06/2025-06-24-google.md); [2025-07-07-google.md](https://github.com/github/dmca/blob/master/2025/07/2025-07-07-google.md)
- **9 Jun 2026, Google:** a notice against a repo of "Google's proprietary protocol buffers" (artem/google3-protos). — [2026-06-09-google.md](https://github.com/github/dmca/blob/master/2026/06/2026-06-09-google.md)
- The ReVanced-related notices (22 Sept 2023, 18 Apr 2024, 12 May 2026 vs MorpheApp) and the YouTube tweak notices (GoodTube 12 Feb 2026, YouTube Search Fixer 18 Mar 2026, YTBackground 7 May 2026) are **developer-vs-developer** copyright and licence disputes, not Google enforcement. — [2023-09-22-revanced.md](https://github.com/github/dmca/blob/master/2023/09/2023-09-22-revanced.md); [2026-05-12-revanced.md](https://github.com/github/dmca/blob/master/2026/05/2026-05-12-revanced.md); [2026-02-12-goodtube.md](https://github.com/github/dmca/blob/master/2026/02/2026-02-12-goodtube.md); [2026-05-07-ytbackground.md](https://github.com/github/dmca/blob/master/2026/05/2026-05-07-ytbackground.md)

### Inferences
- **Streaming-only doesn't remove the circumvention element.** To play a stream outside the official player, a client must solve the same signature/"n" challenge (the "rolling cipher") that the OLG Hamburg called an effective TPM and that the Yout court treated as a §1201(a) "process". The legal difference between "stream" and "download" is thin. Leaving out a download button mainly lowers profile and the "stream-ripping" framing that labels and the courts focused on (and NewPipe's download feature drew the Because Music notice). It is not a legal safe harbour.
- **Personal/unpublished use vs publishing:** the US trafficking bans and German §95a(3) hit whoever "offers to the public" or "distributes" circumvention tools. A private, unpublished build avoids those prongs and leaves only (a) the act-of-circumvention question (§1201(a)(1); §95a(1), which the OLG suggested users breach), with no recorded enforcement against individuals, and (b) ToS breach (account risk). Publishing on GitHub or crates.io with release binaries adds takedown risk. Historically that risk falls on code that itself solves YouTube's cipher (youtube-dl) or redistributes Google's binaries or protos, not on frontends that call an external tool.
- **Lowest-exposure publication shape:** a public MIT TUI that contains **no YouTube cipher or InnerTube code** and only (1) uses the official Data API and RSS, and (2) optionally shells out to a **user-installed** yt-dlp and mpv (`mpv --ytdl`) for playback. That is the same separation tuimeta uses (MIT front end, separately licensed helper). It keeps circumvention code in a project that has survived since 2020, and keeps tuitube looking like a client of public interfaces. Not using YouTube's logo or name as branding, beyond what Developer Policies III.F requires on the API path, avoids trademark-based notices like WhatsApp's 2014 GitHub notice against yowsup (see the "Every official door opens onto a business inbox" section of the tuigram Meta report).
- Google's 2025–2026 use of §1201 and proprietary-protobuf notices shows willingness to target code that reproduces its internal protocol artifacts. Vendoring InnerTube `.proto` definitions into tuitube's repo would be the riskiest choice.
- No route here carries meaningful criminal exposure for a single personal user. The realistic worst cases are account termination (logged-in unofficial), loss of API access (official), or a takedown of the public repo (if it embeds circumvention code).

### Gaps
- I didn't read the full OLG Hamburg judgment, so whether it squarely held that **private users** violate §95a, rather than mentioning bad faith in passing, comes from heise's summary.
- I found no BGH outcome for Uberspace, and no 2nd Circuit decision in Yout as of the 9 Sept 2026 docket update. Check both before publishing.
- My github/dmca check covered 2023–Oct 2026 only, by keyword. Notices that redact repo names, or that sit in Lumen rather than github/dmca, would be missed. I did not survey Codeberg, GitLab or crates.io takedowns.
- I found no 2024–2026 C&D or lawsuit against NewPipe, Piped, FreeTube, rustypipe or YouTube.js.

---

## 8. Licenses of key components and implications for an MIT Rust front end

### Takeaway
Spawning **yt-dlp (Unlicense)** and **mpv (GPLv2+ by default)** as separate processes leaves tuitube free to stay MIT. **Linking** rustypipe (GPL-3.0) or a GPL libmpv build into the binary would make the shipped binary GPL. AGPL projects (Invidious, Piped, FreeTube, invidious-companion) are usable only as separate programs or servers, never as linked code. MIT-licensed YouTube.js (Node) and rustypipe-botguard are the permissive options, and rusty_ytdl is stale.

### Cited Findings
| Component | Licence (from LICENSE / registry) | Activity signal | Source |
|---|---|---|---|
| yt-dlp | Unlicense ("released into the public domain") | Releases through 2026 (2025.11.12, 2026.03.13 cited above) | [LICENSE](https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/LICENSE) |
| NewPipeExtractor (Java) | GPL-3.0 | — | [LICENSE](https://raw.githubusercontent.com/TeamNewPipe/NewPipeExtractor/dev/LICENSE) |
| YouTube.js / youtubei.js (TS) | MIT (© 2021 LuanRT) | Basis of invidious-companion | [LICENSE](https://raw.githubusercontent.com/LuanRT/YouTube.js/main/LICENSE) |
| rustypipe (Rust) | GPL-3.0 | Last release 0.11.4 on **2025-04-23**; last functional commit 2025-06-18; only a docs commit (2026-08-03) since | [crates.io](https://crates.io/crates/rustypipe); [Codeberg](https://codeberg.org/ThetaDev/rustypipe) |
| rustypipe-downloader | GPL-3.0 | 0.3.1, 2025-02-26 | [crates.io](https://crates.io/crates/rustypipe-downloader) |
| rustypipe-botguard (PO-token helper) | MIT | 0.1.2, 2025-08-09 | [crates.io](https://crates.io/crates/rustypipe-botguard) |
| rusty_ytdl (Rust) | MIT OR Apache-2.0 | Last release 0.7.4 on **2024-08-10** | [crates.io](https://crates.io/crates/rusty_ytdl) |
| ytextract (Rust) | MIT OR Apache-2.0 | Last release 2023-01-29 | [crates.io](https://crates.io/crates/ytextract) |
| Invidious / invidious-companion | AGPL-3.0 | — | [LICENSE](https://raw.githubusercontent.com/iv-org/invidious/master/LICENSE); [companion LICENSE](https://raw.githubusercontent.com/iv-org/invidious-companion/master/LICENSE) |
| Piped (frontend and backend) | AGPL-3.0 | Reported non-functional by Aug 2025 | [LICENSE](https://raw.githubusercontent.com/TeamPiped/Piped/master/LICENSE); [Backend](https://raw.githubusercontent.com/TeamPiped/Piped-Backend/master/LICENSE) |
| FreeTube | AGPL-3.0 | — | [LICENSE](https://raw.githubusercontent.com/FreeTubeApp/FreeTube/development/LICENSE) |
| SmartTube | MIT | Compromised builds Nov–Dec 2025 | [LICENSE](https://raw.githubusercontent.com/yuliskov/SmartTube/master/LICENSE) |
| mpv / libmpv | "GPLv2+ … by default"; LGPLv2.1+ "if built without using any GPL only files" (`-Dgpl=false`, which "does not in itself create a LGPLv2.1" binary; you must check deps) | — | [mpv Copyright](https://raw.githubusercontent.com/mpv-player/mpv/master/Copyright) |
| libmpv2 (Rust bindings) | LGPL-2.1 | 6.0.0, 2026-05-12 | [crates.io](https://crates.io/crates/libmpv2) |
| FFmpeg | LGPL v2.1+ by default; GPL v2+ only with `--enable-gpl` | — | [LICENSE.md](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/LICENSE.md) |

### Inferences
- **Separate-process pattern (MIT-safe):** tuitube (MIT) spawns `yt-dlp` (Unlicense; tuitube could even vendor it) and `mpv` (GPL/LGPL) through command-line arguments, pipes or mpv's JSON IPC socket. Under the FSF's own GPL FAQ position on "mere aggregation", programs that talk through pipes, sockets and command-line arguments are normally separate works ([GNU GPL FAQ](https://www.gnu.org/licenses/gpl-faq.html#MereAggregation); I couldn't re-fetch it, the site returned 429/403, so the paraphrase is from prior knowledge). This mirrors tuimeta's MIT front end plus AGPL helper.
- **Linking libmpv:** linking a default (GPL) libmpv makes the distributed tuitube binary GPLv2+. Linking an LGPL build (`-Dgpl=false`, with LGPL FFmpeg) dynamically is compatible with MIT, given LGPL relinking obligations. libmpv2's Rust bindings are LGPL-2.1 themselves. Release binaries for Windows/macOS would also have to bundle or locate libmpv. Spawning the `mpv` binary avoids all of this.
- **rustypipe:** the only substantial native-Rust InnerTube client, but GPL-3.0 (linking it would make tuitube's binary GPL-3.0) and close to dormant since mid-2025. A GPL-3.0 helper binary (the tuimeta pattern) is possible, but its maintenance status is the bigger problem given how often YouTube has broken things (§5).
- **AGPL components** (Invidious, companion, Piped, FreeTube) only make sense as separately run programs. AGPL-3.0 §13 obligations arise when a *modified* version is offered to users over a network, which a local single-user helper doesn't trigger (from the licence text as generally understood; not re-fetched this session).

### Gaps
- GitHub API rate limits stopped me from collecting star counts and last-push dates for the GitHub-hosted projects. The library evaluation is presumably covered by another researcher.

---

## 9. Route-by-route summary: official Data API vs accountless vs logged-in unofficial

### Takeaway
Only the **official Data API plus RSS** is fully sanctioned, but it can't play video in a terminal, and it can't show Watch Later, history or recommendations. **Accountless unofficial playback** (yt-dlp + mpv, local subscriptions from Takeout/RSS) gives the real TUI experience with no account at risk, but it breaches the ToS and needs constant upkeep. **Logged-in unofficial access** (cookies) adds Watch Later, history, recommendations and Premium, at the cost of the user's real Google account being the thing at risk, plus a dangerous long-lived credential.

### Cited Findings
- Official API: sanctioned. Desktop-app OAuth with loopback and PKCE ([native-app](https://developers.google.com/identity/protocols/oauth2/native-app)). 10,000 units/day plus 100 searches/day ([quota](https://developers.google.com/youtube/v3/determine_quota_cost)). No WL, HL, home feed or streams ([revision history](https://developers.google.com/youtube/v3/revision_history); [Developer Policies](https://developers.google.com/youtube/terms/developer-policies)). The worst case is suspension of API access ([API ToS §24.2](https://developers.google.com/youtube/terms/api-services-terms-of-service)).
- Accountless unofficial: breaches the ToS "automated means" and "download" clauses ([YouTube ToS](https://www.youtube.com/t/terms?hl=en&gl=US)). Enforcement is technical: PO tokens, SABR, JS runtime, IP blocks, ~300 videos/hour guest limit ([yt-dlp wiki](https://github.com/yt-dlp/yt-dlp/wiki/Extractors); [Invidious docs](https://docs.invidious.io/youtube-errors-explained/)). There is no account to lose.
- Logged-in unofficial: yt-dlp warns of a temporary or permanent ban risk and recommends a throwaway account. OAuth login has been impossible since Nov 2024, so cookies are the only option, and they rotate ([yt-dlp wiki](https://github.com/yt-dlp/yt-dlp/wiki/Extractors); [openSUSE changelog](https://packagehub.suse.com/update-infos/openSUSE-2025-127/)). The ToS sanction is suspension or termination of the Google account ([YouTube ToS](https://www.youtube.com/t/terms?hl=en&gl=US)).

### Inferences
| Route | Allowed by Google? | Account-ban risk | Legal exposure (personal, unpublished) | Legal exposure if published | Features it reaches |
|---|---|---|---|---|---|
| Data API (OAuth, own client ID) | Yes (API ToS + RMF) | None beyond API revocation | None | Low (needs attribution, 30-day cache rule) | Subs list, uploads feed, search (100/day), playlists, comments, rate/subscribe; **no playback in TUI**, no WL/HL/home |
| RSS + Takeout import (no auth) | Undocumented and tolerated (feed links are public) | None | Negligible | Negligible | Latest ~15 uploads per channel; local subs; no search |
| Accountless unofficial playback (spawn yt-dlp + mpv) | No (ToS: automated access, download, interference) | None (IP soft-blocks only) | Theoretical §1201(a)(1)/§95a(1) act-of-circumvention question; no recorded user enforcement | Low if tuitube only shells out to user-installed yt-dlp; higher if it embeds cipher/InnerTube code or Google protos | Full playback, search, channel pages, comments via InnerTube |
| Logged-in unofficial (cookies) | No (ToS; Paid Service Terms for Premium) | **Real but unquantified**: no verified terminations found; yt-dlp warns of temp or perm bans | As above, plus the credential-security burden | As above | Adds WL, history, home feed, Premium formats and ad-free, private playlists |

- A defensible hybrid that fits tuigram/tuimeta's security posture: **accountless by default** (Takeout/RSS subscriptions stored locally, playback via a spawned user-installed `yt-dlp`/`mpv`). Add an **optional official-API OAuth mode** (`youtube.readonly`, user's own client ID) for syncing subscriptions and playlists. Treat **cookie login as an opt-in, documented-risk mode** at most, ideally with a secondary account, keystore-only storage and conservative request pacing. That keeps the user's main Google account out of the unofficial traffic path.

### Gaps
- Account-ban probability for light, interactive, logged-in unofficial use can't be quantified from public evidence.
- Whether YouTube distinguishes "personal" from bulk unofficial traffic in enforcement is undocumented. The only published thresholds are yt-dlp's observed rate limits.
