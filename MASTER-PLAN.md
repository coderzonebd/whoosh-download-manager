# Master Plan — Open-Source Download Ecosystem

> Working name: **PROJECT** (final name TBD — must not contain "IDM"; run a trademark search first).
> Owner: CoderZone BD. License: see §10. Status: planning, 2026-11-10.

---

## 0. Vision in one paragraph

One download engine, written once in Rust, running everywhere: a desktop app on Windows/macOS/every Linux, a browser extension that works with *or without* the desktop app, Android and iOS apps, a headless daemon for servers/NAS, a CLI, a web remote, and an API/MCP surface for scripts and AI assistants. It must (a) saturate whatever bottleneck exists faster and more reliably than anything else, (b) carry the union of every good feature in IDM, JDownloader, FDM, Motrix/Rayburst, Gopeed, AB DM, MDM, XDM, uGet, Persepolis and FileCentipede, and (c) add things none of them have.

**Honest framing:** no software can download faster than the slower of (your link, the server's cap). "Fastest on the planet" therefore means: **reach the ceiling first, stay there without stalls, recover instantly, and start before the user finishes clicking.** Everything in §1 serves that definition, and we prove it with public, reproducible benchmarks.

---

## 1. The Speed Doctrine (how we actually win on speed)

Each point is an engineering commitment with a measurable test in the benchmark harness (§8, Phase 1).

| # | Commitment | What it beats | How |
|---|---|---|---|
| 1 | **Never idle a byte** | IDM/aria2 fixed segment schemes | Work-stealing segment scheduler. When a connection finishes, it splits the *largest remaining* range of the *slowest* connection. Min split 1 MiB (no thrash). Every connection always has work until the last byte. |
| 2 | **Hedged requests** | IDM's timeout→retry | Per-connection EWMA throughput + RTT. A connection that falls below X% of the median for N seconds gets a *shadow* connection on the same range; keep whichever delivers first, cancel the other. Tail latency disappears. |
| 3 | **Learned connection count** | IDM's static 8/16/32 | Start at 4, hill-climb while aggregate throughput rises, back off on 429/403/RST/connection-refused. Persist a **host profile** (max safe connections, Range support, H2/H3 support, avg per-conn speed). Second download from the same host starts at the optimum. |
| 4 | **Best transport per host** | Everyone (they use OS TCP + H1 only) | H3/QUIC (userspace, `quinn`) when Alt-Svc advertises it → we control congestion control (BBR, experimental in quinn) even on Windows where the OS default is CUBIC. Else H2 with *multiple* connections (one H2 connection shares one congestion window, so we open several). Else H1.1 keep-alive. TLS 1.3 + session resumption + 0-RTT. |
| 5 | **Connect fast** | All | Happy Eyeballs (RFC 8305) across *all* resolved A/AAAA records; for CDNs, race the first 3 IPs and keep the fastest. Optional DoH/DoT. |
| 6 | **Predictive preconnect** | Nobody has this | The extension tells the app when the user *hovers* a download-looking link → app pre-resolves DNS and opens TLS. On click the first bytes flow in <100 ms. MDM measures IDM at ~3.1 s to first byte; we target "instant" without the browser-handoff gap. |
| 7 | **Disk is never the bottleneck** | aria2/IDM on slow disks | Preallocate (`fallocate` / `SetFileInformationByHandle`), 4–16 MiB per-segment write buffers, `io_uring` on Linux (tokio-uring), overlapped I/O on Windows, write coalescing, backpressure between net and disk. Journal fsync every 2 s or 64 MiB — never per write. |
| 8 | **Power-cut-proof resume** | Everyone who corrupts on hard reset | Segment journal with CRC, written atomically. On restart: re-fetch the last 64 KiB of each segment's tail and byte-compare before trusting it. Verify ETag/Last-Modified/Content-Length against the server before resuming; if the remote file changed, say so and restart. Designed for load-shedding. |
| 9 | **Multi-source is native** | Everyone except aria2 (partial) | The scheduler is source-agnostic: any "range provider" can feed it. Sources: Metalink (RFC 5854), user mirrors, BitTorrent web seeds (BEP 19), BitTorrent peers, **LAN peers** (another PROJECT instance on the same network that already has the file — mDNS discovery). Each source is scored; the slowest get fewer ranges. |
| 10 | **Prove it, continuously** | Marketing claims | `labserver` (fault-injecting HTTP test server: latency, loss, per-connection throttles, connection caps, random resets, slow start) + `bench` harness. CI fails on regression. Monthly published results vs IDM, aria2, Motrix/Rayburst, Gopeed, AB DM, MDM — same machine, same files, raw data in the repo. |

Also in the engine: "first-byte priority" (download the file header first so it is openable/previewable immediately), sequential mode for media preview, **polite mode** (LEDBAT-style: back off when RTT inflates so video calls and games don't suffer — nobody in the market has this), metered-connection awareness.

---

## 2. Architecture — one core, many shells

```
                       ┌────────────────────────────────────────────────┐
                       │                 PROJECT core (Rust)             │
                       │  net: H1/H2/H3, TLS, DNS, host profiles         │
                       │  sched: segments, hedging, multi-source         │
                       │  store: preallocation, journal, SQLite state    │
                       │  rules: categories, queues, scheduler, limits   │
                       │  ext: torrent (librqbit), media (yt-dlp/ffmpeg) │
                       │       extract (zip/7z/rar), plugins (WASM)      │
                       └──────┬───────────┬───────────┬──────────┬───────┘
          native lib (tauri)  │   UniFFI  │   WASM    │  daemon  │  CLI
                              ▼           ▼           ▼          ▼
   ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌───────────────────┐
   │ Desktop app  │  │ Android/iOS  │  │ Extension    │  │ Headless daemon   │
   │ Tauri 2      │  │ (Tauri 2     │  │ Lite mode    │  │ systemd/launchd/  │
   │ Win/mac/Linux│  │ mobile, or   │  │ (no install) │  │ Win service/Docker│
   └──────┬───────┘  │ native+FFI)  │  └──────┬───────┘  └────────┬──────────┘
          │          └──────────────┘         │                   │
          │   native messaging                │  WebSocket API    │
          └────────────── Extension Pro mode ─┴───────────────────┘
                              │
                 ┌────────────┴─────────────┐
                 │ Web remote UI · MCP · REST│  ← shared TS UI package
                 │ SDKs (TS/Python/Rust)     │
                 └──────────────────────────┘
```

**Rules that keep this sane**

- The core is a library with *no* UI assumptions. The daemon is the only process that owns downloads; every UI (desktop window, tray, extension, phone, web) is a client of the same local WebSocket/JSON-RPC API. Desktop app = the UI, which starts the daemon as a bundled sidecar when none is running (amended 2026-11-14, [ADR 0007](docs/adr/0007-phase2-plan.md); first planned with the core linked in).
- One TypeScript UI package (Svelte 5 recommended; Vue 3 acceptable) renders in the desktop window, the mobile shell, the extension popup/side panel, and the web remote. One design system, four surfaces.
- Mobile: start with **Tauri 2 mobile** (reuses the whole UI). If native quality or background behavior is insufficient after a 6-week spike, fall back to native shells (Kotlin Compose / SwiftUI) over the same core via **UniFFI**. The core does not change either way.
- Alternative stack considered: Flutter + flutter_rust_bridge (proven by Gopeed/LocalSend). Rejected for v1 because it would split the UI from the extension/web surfaces. Revisit only if Tauri mobile fails the spike.

**Monorepo layout**

```
crates/   core  daemon  cli  ffi(uniffi)  wasm  torrent  media  extract  plugins  labserver
apps/     desktop(tauri)  mobile  extension  web-remote
packages/ ui(svelte)  api-client(ts)  design-tokens
tools/    bench  release  i18n
docs/     website/
```

---

## 3. Feature matrix

### 3.1 Union of the market (we ship all of these)

**Capture & input**
- Browser capture of every download with cookies/headers/referer forwarded (IDM) · media sniffer with on-page "Download video" button (IDM, MDM, XDM) · "download all links / images / selected" context menus (IDM) · clipboard monitor (uGet) · drag-and-drop basket (IDM) · protocol handlers `magnet:`, `ed2k:`, `thunder:`, `project:` and `.torrent`/`.metalink` associations (Motrix) · batch patterns `file[001-100].zip` (IDM) · **site grabber** (crawl a site, filter by type/depth) (IDM) · link crawler + decrypter + hoster plugins (JDownloader/pyLoad) · RSS feeds for torrents and pages (JDownloader, MDM) · debrid accounts via official APIs (MDM) · IDM-compatible command-line switches (MDM) so existing scripts work.

**Protocols**
- HTTP/1.1, HTTP/2, HTTP/3 · FTP/FTPS, SFTP/SSH (FileCentipede) · WebDAV (FileCentipede) · BitTorrent + magnet + DHT/PEX/encryption + selective files + sequential + seeding limits (Motrix/Rayburst/FDM/MDM) · ed2k (Gopeed/Rayburst) · HLS/DASH with track/subtitle selection, live recording, MP4/MKV mux (Rayburst) · Metalink · yt-dlp + ffmpeg for 1,800+ video sites (Persepolis/MDM/OmniGet) · gallery-dl for galleries (MDM).

**Management**
- Categories with rules + auto-folders · queues · scheduler with wake-from-sleep and shutdown/sleep after (IDM/MDM) · global/per-task/per-schedule speed limits, "only while I'm active" (MDM) · pause/resume/restart · duplicate detection · disk-space check · archive extraction incl. passwords and multi-part (JDownloader/MDM) · reconnect/IP-reset for hoster limits (JDownloader) · post-download virus scan hook + Windows Zone.Identifier mark-of-the-web (IDM) · history with search · favorite/recent folders (Rayburst) · preview media while downloading (FDM).

**Network**
- Proxy per download/global/SOCKS5 · system proxy detection · bind to one adapter + VPN kill switch (MDM) · encrypted DNS (MDM) · GeoIP flags (MDM) · UPnP for torrents (Rayburst).

**Ecosystem**
- Headless/NAS/Docker (MDM/pyLoad) · web remote from phone (MDM/JDownloader) · CLI · local REST/WebSocket API · MCP server for AI assistants (MDM) · JS/WASM extension system (Gopeed) · tray with live speed, Dock badge (Motrix) · portable mode (MDM/FDM) · auto-update channels stable/beta/nightly with signed updates (Rayburst) · themes incl. "Classic IDM-like layout" and "Modern" (MDM/AB DM) · 80+ languages with RTL (MDM) · screen-reader + full keyboard (MDM) · diagnostics ZIP (Rayburst).

### 3.2 Exclusive to us (the "beyond best" list)

1. **Predictive preconnect** on link hover (§1.6).
2. **Self-healing links** — when a URL expires (403/410/signature expired), the extension silently re-navigates the origin page, captures the fresh URL, and the download continues. IDM makes you click "Refresh download address" by hand.
3. **Power-cut-proof resume** with tail verification (§1.8).
4. **Multi-source swarm incl. LAN peers** (§1.9) — the office/hostel downloads a 4 GB file once from the internet, then everyone else gets it at LAN speed.
5. **BDIX / local-FTP mode** — browse h5ai/Apache/nginx indexes and Emby/Jellyfin libraries inside the app, queue whole folders, and when the same file exists on several local servers, pull from all of them while ranking servers by measured speed. (Bangladesh-first; nobody abroad will build this.)
6. **Polite mode** — latency-aware limiter so downloads never hurt calls/games (§1).
7. **Content verification badge** — SHA-256/-512 from Metalink, `.sha256` sidecars, or hash text near the link; GPG/Sigstore signature check when available; optional hash-only VirusTotal lookup (opt-in, never uploads the file).
8. **Learned host profiles** (§1.3) with optional, anonymous, opt-in crowd sharing of "this host allows N connections".
9. **Send to device** — from phone to PC, PC to NAS, browser to any paired device. Pairing by QR; transport over LAN or the user's own WireGuard/Tailscale; optional self-hostable relay for outside-LAN.
10. **WASM plugin sandbox** — hoster/site/post-processor plugins run in `wasmtime` with capability permissions (which hosts, which folders). Safer than JDownloader's Java plugins; in-app signed plugin catalog.
11. **Native Bangla + first-class accessibility** from v1, not machine-translated.
12. **Reproducible builds + SBOM + signed releases** — users can verify the binary matches the source.

---

## 4. Surfaces, honestly scoped

### 4.1 Desktop (Windows, macOS, Linux)
Full feature set. Tauri 2 app over the daemon, started as a sidecar ([ADR 0007](docs/adr/0007-phase2-plan.md)). Packages: MSIX + NSIS + portable ZIP + winget + scoop + choco; `.dmg` (Universal) + Homebrew cask; `.deb`, `.rpm`, AppImage, **Flatpak (Flathub)**, Snap, AUR, Nix — "any Linux distro" is satisfied by AppImage + Flatpak at minimum, native packages for the big families. Windows signing via SignPath Foundation (free for OSS) or an EV cert; macOS Developer ID + notarization ($99/yr) — unsigned builds are a dead end for mainstream users.

### 4.2 Browser extension — Pro mode vs Lite mode
Browsers: Chrome, Edge, Brave, Opera, Vivaldi (one Chromium build), Firefox, Safari (Xcode-converted, reduced).

**Pro mode** (desktop app installed): native messaging → full engine. Everything in §3.

**Lite mode** (no install — the user's point 4). The extension carries the core compiled to **WASM**; `fetch()` is the transport; the File System Access API streams to disk. This is real multi-connection acceleration inside the browser — but the browser's sandbox sets hard limits. What the user gets and loses:

| Capability | Lite (extension only) | Pro (with app) |
|---|---|---|
| Catch browser downloads, cookies/referer | ✅ | ✅ |
| Multi-connection acceleration | ✅ but **capped ≈6 connections per host on HTTP/1.1** (Chromium's per-host limit; H2 servers unaffected) | ✅ unlimited, learned |
| HTTP/3 with our congestion control, socket tuning, io_uring | ❌ browser owns the network stack | ✅ |
| Stream to disk | ✅ Chrome/Edge/Brave/Opera (File System Access API, user picks a folder once) · ⚠️ Firefox/Safari: buffer in OPFS then hand to browser download (double write, ~2 GB practical cap) | ✅ |
| Resume after browser restart | ⚠️ state in IndexedDB/OPFS; may need to re-pick the destination folder | ✅ |
| Runs with browser closed; survives browser sleep | ❌ (MV3 service worker lifetime; we keep an offscreen document alive while downloading) | ✅ |
| Queues, scheduler, speed limit, categories | ✅ while browser open; no wake/shutdown | ✅ |
| Video sniffer (direct media URLs, HLS via WASM muxer) | ✅ experimental, CPU-heavy | ✅ |
| yt-dlp sites (YouTube etc.) | ❌ not possible, and store policy forbids it anyway | ✅ (GitHub builds) |
| BitTorrent / ed2k / FTP / SFTP | ❌ (WebTorrent-over-WebRTC only, experimental) | ✅ |
| Archive extraction | ⚠️ zip only via WASM | ✅ zip/7z/rar |
| Catch downloads from *other* browsers | ❌ | ✅ |
| Tray, OS notifications, protocol handlers (`magnet:` works via registerProtocolHandler) | partial | ✅ |
| Remote-control another PROJECT daemon | ✅ (the extension is also a client) | ✅ |
| Headless/NAS, MCP, CLI, VPN bind | ❌ | ✅ |

Lite mode is therefore a **genuinely useful accelerator for HTTP(S) files** and a funnel to the app, not feature parity. The extension detects the app and switches modes automatically; a one-line banner explains what Pro adds.

### 4.3 Android
Full core via Tauri 2 mobile (or Kotlin + UniFFI). Foreground service with progress notification, WorkManager scheduling, Storage Access Framework / MediaStore, multi-connection, torrent (allowed on Play), share-sheet + clipboard + `magnet:`/file-type intents, Firefox-for-Android extension talks to the app over localhost. **Two builds:** Play Store build *without* YouTube/yt-dlp (Play policy), and GitHub/F-Droid build with everything.

### 4.4 iOS
Scoped honestly: a **companion + HTTP downloader**, not parity. Background downloads via `URLSession` background sessions (Apple schedules them; multi-Range tasks are possible but throttled in background; full engine runs in foreground), save to Files/Photos, share extension to "send to my PC", full remote control of desktop/daemon. **No torrent, no YouTube** — App Store rejects both. A sideloaded/AltStore build with torrent can exist later; it is not the main line.

### 4.5 Daemon, CLI, web remote, API
`projectd` runs as systemd/launchd/Windows service or Docker (Unraid/Synology/QNAP templates). Web remote served by the daemon (same UI package). CLI for scripts (plus IDM-switch compatibility). Local API: WebSocket JSON-RPC + REST, token-bound, origin-checked. MCP server. SDKs: TS, Python, Rust.

---

## 5. Security & privacy (non-negotiable from v1)

- Local API bound to localhost with a per-install token; extension IDs allow-listed in the native-messaging manifest; origin checks on WebSocket.
- Web pages can never choose a local path. Filenames sanitized (RFC 6266/5987, non-UTF-8, path traversal). Zip-slip protection in extraction.
- Signed updates (minisign) with a public key pinned in the binary; reproducible builds; SBOM per release; GitHub Actions builds every artifact.
- No telemetry by default. Opt-in crash reports (self-hosted Sentry/GlitchTip). Hash-only external lookups, only if the user turns them on.
- Downloads written with mark-of-the-web (Windows Zone.Identifier, macOS quarantine) so OS protections still apply.
- Threat model doc in the repo; security.txt; coordinated disclosure policy.

---

## 6. Legal & store-policy constraints (know before building)

- **Trademark:** the name, icon and marketing must not use "IDM"/"Internet Download Manager" except in truthful comparison. Clean-room statement in the README like MDM's.
- **Chrome Web Store / Firefox AMO / Play / App Store** forbid facilitating unauthorized downloads of copyrighted media. Therefore: YouTube/yt-dlp capability lives only in desktop builds and GitHub/F-Droid Android builds; the store extension and Play build omit it. The extension stays a general downloader.
- **App Store:** no torrent, no YouTube.
- **yt-dlp / ffmpeg / 7-Zip** run as separate processes, downloaded on first use or bundled per their licenses (yt-dlp Unlicense, ffmpeg LGPL build, 7-Zip LGPL + unRAR restriction). Keep the core free of GPL-incompatible linking surprises.
- **Export/crypto:** standard TLS only; nothing exotic.

---

## 7. Platform & distribution matrix

| Surface | Channels |
|---|---|
| Windows 10/11 x64 + ARM64 | GitHub Releases (MSIX, NSIS, portable), winget, scoop, choco, Microsoft Store (later) |
| macOS 12+ (Universal) | GitHub (.dmg), Homebrew cask, notarized |
| Linux x64 + ARM64 | AppImage, Flathub, Snap, .deb (apt repo), .rpm (dnf repo), AUR, Nix, Docker |
| Browser extension | Chrome Web Store (covers Edge/Brave/Opera/Vivaldi), Firefox AMO, Safari (App Store, later), unpacked ZIP |
| Android 8+ | Play (store build), F-Droid + GitHub APK (full build) |
| iOS 16+ | App Store, TestFlight |
| Server | Docker Hub/GHCR, Unraid/Synology templates |

---

## 8. Roadmap (assumes a focused team of 4–6, see §9)

Gates are hard: the next phase does not start until the gate passes.

### Phase 0 — Foundations (weeks 1–3)
Name + trademark search, license split (§10), monorepo, CI matrix (Win/mac/Linux/Android/iOS), design system + tokens, `labserver` with fault injection, `bench` harness, threat model, contribution guide, website skeleton. Register Apple Developer, Play, SignPath.
**Gate:** `bench` runs IDM, aria2, MDM, Gopeed, AB DM, Motrix against labserver on one machine and publishes a baseline report.

### Phase 1 — Engine (months 1–3)
Core: H1/H2/H3, TLS, Happy Eyeballs, host profiles, work-stealing scheduler, hedging, learned connection count, preallocation + io_uring/overlapped I/O, journal + tail verification, speed limits, queues, categories/rules, SQLite state. Daemon with WebSocket/REST API. CLI. Unit + property tests + fault-injection tests.
**Gate** (amended 2026-11-11 and 2026-11-13 by the owner's decisions, after the first Phase 1 benchmarks; the original read "wins every labserver scenario and three real-world CDN scenarios against all competitors with p < 0.01, lower CPU, and zero corruption across 1,000 simulated hard kills"). Measured on an idle machine, ≥ 7 runs per cell, Mann-Whitney U, with Whoosh's daemon **resident** (as a download manager runs; cold one-shot CLI runs are reported alongside but do not gate):
1. **Correct:** every run byte-exact in every labserver scenario; zero corrupt files; zero wrong file names.
2. **Crash-proof:** zero corrupt and zero duplicate files across 1,000 SIGKILLs of the real daemon mid-download (`whoosh-bench hardkill`).
3. **Never slower:** in no labserver scenario is any competitor significantly faster than Whoosh (p < 0.01). Where one connection is optimal (no ranges, small files, loopback single stream) or the server imposes the limit (connection caps, a global throttle), a statistical tie is the honest best. Time is measured **to durability** for every client (owner's decision, 2026-11-11): Whoosh forces a finished file to disk before it calls it done, so a power cut never damages it (its default `powerSafe` setting; load-shedding is common where it is used); other clients leave the bytes in the page cache, so the bench syncs their output file and directory after they finish and counts that time too. Time to the file appearing is reported alongside. **Small-file allowance** (owner's decision, 2026-11-13, after gate run 4): on files of at most 4 MiB, a competitor whose median time to durability is lower by at most 10 ms does not fail this rule. That is the fixed cost of the power-safe finish (the finished file, its name and a crash-safe record of the move forced to disk), which clients that leave the bytes in the page cache never pay; on 1 MiB files it shows as a 5–9 ms lead for wget on this laptop. The report lists every row the allowance covers, with its p-value and the gap in ms.
4. **Fastest where the engine matters:** in the per-connection-throttle, slow-drip, flaky, stall, high-latency and rate-limited scenarios, Whoosh's median time is the best of all clients or within 5 % of it.
5. **Lean:** median CPU per GiB at or below every multi-connection competitor's, per scenario.
6. **Real world:** three real CDN scenarios, interleaved with aria2c `-x16` and the best other client: no competitor significantly faster, CPU at or below theirs.

Public report.

### Phase 2 — Desktop + Extension Pro → **v1.0** (months 4–6)
Tauri desktop (Win/mac/Linux), tray, Classic + Modern layouts, categories/queues/scheduler UI, history. Extension (Chromium + Firefox) Pro mode: capture, cookie/referer forwarding, context menus, media sniffer, predictive preconnect, self-healing links. yt-dlp/ffmpeg integration (desktop build). Signed installers on the v1.0 channels (amended 2026-11-14, [ADR 0007](docs/adr/0007-phase2-plan.md): GitHub Releases, winget, scoop, Homebrew cask, Flathub, AUR; MSIX/Microsoft Store, Snap, Nix, Chocolatey and apt/dnf repositories follow in v1.x). Bangla + English + ~10 machine-seeded languages via Weblate. Accessibility pass.
**Gate:** 2 weeks public beta, crash-free rate > 99.5% (from opt-in crash reports), extension approved in both stores. Work streams and order: [ADR 0007](docs/adr/0007-phase2-plan.md).

### Phase 3 — Protocol & power features → **v1.5** (months 7–9)
BitTorrent (librqbit), ed2k, FTP/SFTP/WebDAV, HLS/DASH, Metalink + multi-source + LAN peers, archive extraction, batch/site grabber/link crawler, RSS, debrid, proxy/VPN bind/kill switch, polite mode, content verification, headless Docker, web remote, MCP, portable mode, BDIX/local-FTP mode.
**Gate:** feature-parity checklist vs §3.1 ≥ 90%; benchmark report #2.

### Phase 4 — Everywhere → **v2.0** (months 10–13)
Extension Lite mode (WASM core, File System Access). Android (Play + full builds). iOS companion. Send-to-device + pairing + optional relay. WASM plugin system + signed catalog. Reproducible builds + SBOM. Safari extension.
**Gate:** one download started on the phone, finished on the PC, verified by hash; Lite mode measurably faster than the browser's own downloader on a 1 GB H2 file.

### Phase 5 — Ecosystem (ongoing)
Plugin catalog growth, hoster plugins, translations to 80+ languages with native review, community programs, quarterly benchmark reports, enterprise/NAS integrations, optional hosted relay.

"Every feature of everyone" is a multi-year surface; the phases order them by user impact. v1.0 alone must already be the best HTTP downloader on the market — that is what the Phase 1 gate guarantees.

---

## 9. Team, tooling, cost

**Team (minimum viable for the timeline above)**
- 1 Rust engine lead (networking, I/O, scheduler) — the hire that decides the project
- 1 Rust generalist (daemon, torrent, media, extraction, FFI/WASM)
- 1–2 TypeScript/Svelte engineers (desktop UI, extension, web remote)
- 1 mobile engineer (Kotlin first; Swift later) — from Phase 4, part-time earlier for the spike
- 0.5 release/DevOps (CI, signing, packaging for 10+ channels)
- Part-time: designer, technical writer, community/i18n coordinator

**Tooling:** GitHub (monorepo, Actions, Releases, Discussions), Weblate (self-hosted) or Crowdin for OSS, self-hosted Sentry/GlitchTip, Plausible for the website, Discord/Telegram community, SignPath, Apple Developer, Play Console.

**Hard costs (yearly):** Apple $99 · Play $25 (once) · domain/CDN ~$100 · crash/analytics hosting ~$300 · optional EV cert ~$400 · relay server (if hosted) ~$600+. Everything else is engineering time. GitHub Actions is free for public repos (macOS runners count against limits — budget self-hosted Mac runner if needed).

**Sustainability options (open source stays free):** GitHub Sponsors / Open Collective, optional hosted relay subscription, enterprise/NAS support contracts, CoderZone branding and recruiting value.

---

## 10. Decisions needed now

1. **Name** — short, pronounceable in Bangla and English, `.app`/`.com`/`.dev` available, no trademark conflicts.
2. **License split** — recommended: core/daemon/CLI under **Apache-2.0** (lets others embed the engine → adoption and brand), apps and extension under **GPL-3.0** (prevents proprietary forks of the product). Alternative: everything GPL-3.0.
3. **Mobile stack** — Tauri 2 mobile first with a 6-week spike (recommended), or go straight to native Kotlin/Swift over UniFFI.
4. **Frontend** — Svelte 5 (recommended: small bundles matter in the extension) vs Vue 3.
5. **v1 scope** — confirm Phase 2 = desktop + extension Pro only; mobile and Lite follow.
6. **Team commitment** — the roadmap assumes 4–6 people full-time. Fewer people stretches the calendar, not the gates.

---

## 11. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Speed claims not reproducible → credibility loss | Public harness, raw data, third parties can rerun; never publish a number the harness didn't produce |
| Antivirus false positives / SmartScreen | Sign everything from day one (SignPath/EV, notarization); submit to AV vendors' allow-lists |
| Store rejections (YouTube, torrent) | Split builds (§6); extension stays general-purpose |
| Tauri mobile immaturity | Time-boxed spike; UniFFI fallback keeps the core intact |
| MV3 service worker killed mid-download | Offscreen document keepalive; state persisted per segment; resume on wake |
| yt-dlp breakage cadence | Self-updating yt-dlp with pinned fallback; it runs out-of-process |
| Scope explosion ("every feature") | Phase gates; feature parity tracked as a checklist, not promises |
| Burnout / bus factor | Written architecture docs, ADRs, contributor onboarding, at least two people per crate |
| A competitor (MDM, Rayburst) ships the same ideas first | Our moat is the engine + benchmark culture + BDIX/LAN/device ecosystem, not any single feature |

---

## 12. Success metrics

- **Engine:** fastest in 100% of labserver scenarios and ≥ 90% of real-world CDN scenarios vs all listed competitors; 0 corrupted resumes in 1,000 hard-kill tests; lowest CPU/MB.
- **Product:** crash-free sessions > 99.5%; extension rating ≥ 4.6; time-to-first-byte after click < 100 ms with preconnect.
- **Ecosystem:** 10k GitHub stars within 6 months of v1.0; 20+ native-reviewed languages; 50+ external contributors; Flathub + winget + Homebrew + Play + App Store presence by v2.0.
- **Bangladesh:** BDIX mode used by > 30% of BD users; Bangla UI reviewed by native speakers at 100%.
