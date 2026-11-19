# Privacy

What the Whoosh Download Manager desktop app, its background service (`wdm-daemon`), the command line (`wdm`) and the browser extension send anywhere. The short version: **apart from anonymous crash reports and an anonymous count of update checks, both of which you can turn off, nothing is sent anywhere.** The extension has its own policy in [`apps/extension/PRIVACY.md`](../apps/extension/PRIVACY.md); it collects no data.

## The app

Whoosh Download Manager downloads what you ask it to and keeps its settings, download list and history on your computer, in its data directory (Linux `~/.local/share/wdm`, macOS `~/Library/Application Support/wdm`, Windows `%LOCALAPPDATA%\wdm`). It does not send usage data, analytics or identifiers anywhere. It talks to the network only to download what you asked for, and to fetch yt-dlp and ffmpeg from their official releases where the installer did not include them (the desktop installers do; the command line fetches them the first time you download from a video site) or when you ask for an update (see [`crates/media/README.md`](../crates/media/README.md)).

## Update checks

The desktop app checks for a new version once a day (Settings → Updates; you can turn the daily check off). The check asks our update feed, a small Cloudflare Worker ([`infra/update-feed`](../infra/update-feed)), for the newest version, and tells it three things: the operating system (`windows`, `darwin` or `linux`), the processor type (such as `x86_64`) and the WDM version.

The feed adds one to a counter for that day, operating system, processor type and version, and keeps nothing else: no IP address, no ID, no time of day, and request logging is off. Because each copy checks about once a day, the daily total tells us roughly how many copies are in use; it cannot tell copies or people apart. The counts are public: the feed's `/stats` address and the `stats` branch of the repository. Copies installed through a package manager (winget, Homebrew, apt and the like) are updated by it and do not check.

Cloudflare, which runs the feed, sees the connection like any web host does, under its own privacy policy. If the feed is unreachable the app falls back to reading the same file from GitHub, which is not counted.

## The local API

The background service listens only on your own computer (`127.0.0.1`) and accepts only clients that know a secret token kept in a file only your user account can read ([PROTOCOL.md §1](api/PROTOCOL.md)). Web pages cannot talk to it: it refuses browser origins other than the app's own window.

## Crash reports

**On by default, and you can turn them off.** They help fix bugs and let the public beta measure the crash-free rate that the v1.0 gate asks for (ADR 0007, Amendment 1; the owner changed them from opt-in to on by default on 2026-11-14).

- **You are told once.** On first run the app shows a short notice: "Whoosh Download Manager sends anonymous crash reports to help fix bugs. No file names or links. Turn off in Settings." It links to the switch and can be dismissed; it never blocks anything.
- **Turning them off (or on again):** Settings → Privacy → *Send crash reports* in the desktop app. The choice is saved as `crash-reports.json` in the data directory, so the app and the background service follow the same switch, and a choice you made is kept across updates. Turning it off stops reports at once (every report checks the switch before it is sent); turning it back on applies to the app at once and to the background service the next time it starts.
- **Where they go:** our project on [GlitchTip](https://glitchtip.com/)'s hosted service (free plan), which receives reports in Sentry's format. GlitchTip keeps them under its own privacy policy.
- **When:** only when Whoosh Download Manager crashes: a panic in the app or the background service, or an uncaught error in the app's window. Nothing is sent while WDM works normally. There is no performance tracing, no session tracking and no screen recording.
- **What a report contains:** the error message, where in Whoosh Download Manager's code it happened (function names and source lines of WDM and the libraries it uses), the WDM version, which part crashed (app, window or background service), and the operating system's name, version and processor type.
- **What is removed before it leaves your computer**, by a scrubber that runs on every report ([`crates/crash/src/scrub.rs`](../crates/crash/src/scrub.rs), with tests built from realistic reports):
  - links (any `scheme://…`, `magnet:`, host names), query strings, cookies, request and response headers, tokens and passwords;
  - file names and paths (Unix, Windows and network paths, and quoted names in error messages);
  - your user name and the computer's name, e-mail addresses and IP addresses other than `127.0.0.1`;
  - in stack traces: absolute paths (only `crate/src/file.rs` and the line remain), local variables and source lines;
  - free-form parts of a report (extra data, request, user, device name), which are dropped entirely. The crash-report library is configured not to collect personal data (`send_default_pii` off, no server name).
- **Builds without a crash-report address send nothing.** The address (DSN) is set only when release builds are made (`WDM_CRASH_DSN`, a build secret). A build from source without it contains the switch's code but never starts the reporter, and hides the switch and the notice.

The rules err on the side of removing too much: a report that says "could not open \<file>" is still useful; one that names the file would be a bug. If you find a report detail that should not be there, report it privately as described in [`SECURITY.md`](../SECURITY.md).

## The website

The website sets no cookies and loads no analytics or third-party scripts. It is hosted on GitHub Pages, which keeps server logs under GitHub's own privacy statement.

## Contact

Questions: open an issue on GitHub, or report security problems privately as described in [`SECURITY.md`](../SECURITY.md).
