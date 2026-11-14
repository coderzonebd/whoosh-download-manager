# Privacy

What the Whoosh desktop app, its background service (`whoosh-daemon`), the command line (`whoosh`) and the browser extension send anywhere. The short version, as on the website's privacy page: **nothing is sent anywhere unless you turn on crash reports.** The extension has its own policy in [`apps/extension/PRIVACY.md`](../apps/extension/PRIVACY.md); it collects no data.

## The app

Whoosh downloads what you ask it to and keeps its settings, download list and history on your computer, in its data directory (Linux `~/.local/share/whoosh`, macOS `~/Library/Application Support/whoosh`, Windows `%LOCALAPPDATA%\whoosh`). It does not send usage data, analytics or identifiers anywhere. It talks to the network only to download what you asked for, and, on desktop builds, to fetch yt-dlp and ffmpeg from their official releases the first time you download from a video site (see [`crates/media/README.md`](../crates/media/README.md)).

## The local API

The background service listens only on your own computer (`127.0.0.1`) and accepts only clients that know a secret token kept in a file only your user account can read ([PROTOCOL.md §1](api/PROTOCOL.md)). Web pages cannot talk to it: it refuses browser origins other than the app's own window.

## Crash reports

**Optional and off by default.** They exist so the public beta can measure the crash-free rate that the v1.0 gate asks for (ADR 0007, Amendment 1).

- **Turning them on or off:** Settings → Privacy → *Send crash reports* in the desktop app. The choice is saved as `crash-reports.json` in the data directory, so the app and the background service follow the same switch. Turning it off stops reports at once (every report checks the switch before it is sent); turning it on applies to the app at once and to the background service the next time it starts.
- **Where they go:** our project on [GlitchTip](https://glitchtip.com/)'s hosted service (free plan), which receives reports in Sentry's format. GlitchTip keeps them under its own privacy policy.
- **When:** only when Whoosh crashes: a panic in the app or the background service, or an uncaught error in the app's window. Nothing is sent while Whoosh works normally. There is no performance tracing, no session tracking and no screen recording.
- **What a report contains:** the error message, where in Whoosh's code it happened (function names and source lines of Whoosh and the libraries it uses), the Whoosh version, which part crashed (app, window or background service), and the operating system's name, version and processor type.
- **What is removed before it leaves your computer**, by a scrubber that runs on every report ([`crates/crash/src/scrub.rs`](../crates/crash/src/scrub.rs), with tests built from realistic reports):
  - links (any `scheme://…`, `magnet:`, host names), query strings, cookies, request and response headers, tokens and passwords;
  - file names and paths (Unix, Windows and network paths, and quoted names in error messages);
  - your user name and the computer's name, e-mail addresses and IP addresses other than `127.0.0.1`;
  - in stack traces: absolute paths (only `crate/src/file.rs` and the line remain), local variables and source lines;
  - free-form parts of a report (extra data, request, user, device name), which are dropped entirely. The crash-report library is configured not to collect personal data (`send_default_pii` off, no server name).
- **Builds without a crash-report address send nothing.** The address (DSN) is set only when release builds are made (`WHOOSH_CRASH_DSN`, a build secret). A build from source without it contains the switch's code but never starts the reporter and hides the switch.

The rules err on the side of removing too much: a report that says "could not open \<file>" is still useful; one that names the file would be a bug. If you find a report detail that should not be there, report it privately as described in [`SECURITY.md`](../SECURITY.md).

## The website

The website sets no cookies and loads no analytics or third-party scripts. It is hosted on GitHub Pages, which keeps server logs under GitHub's own privacy statement.

## Contact

Questions: open an issue on GitHub, or report security problems privately as described in [`SECURITY.md`](../SECURITY.md).
