# whoosh-media

Video and audio sites for desktop and GitHub builds: yt-dlp finds the media on a page, Whoosh's engine downloads it, ffmpeg joins video and audio. Store builds never include it (MASTER-PLAN §6). License: Apache-2.0. Protocol: [PROTOCOL.md, "Media"](../../docs/api/PROTOCOL.md#media-video-and-audio-sites).

```rust
let engine: Arc<dyn Service> = whoosh_engine::Engine::start(deps, config).await?;
let service = whoosh_media::MediaService::start(engine, MediaConfig::new(data_dir.join("media"))).await?;
// serve `service`: everything the engine does, plus media.* and the `media` capability
```

`whoosh-daemon` does exactly this when built with its `media` feature (the default) and not started with `--no-media`.

## The tools

yt-dlp and ffmpeg are separate programs. They are never linked, never bundled, and run with an argument array (no shell). `Tools` finds or fetches them:

| | Windows | macOS | Linux |
|---|---|---|---|
| yt-dlp | `yt-dlp.exe` / `yt-dlp_arm64.exe` | `yt-dlp_macos` | system `yt-dlp`, else `yt-dlp_linux` / `_aarch64` / `musllinux` |
| from | [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases), checked against `SHA2-256SUMS` of the same release | | |
| ffmpeg | BtbN `win64` / `winarm64` **LGPL** static zip | system (Homebrew) `ffmpeg`, else Martin Riedl's release build | system `ffmpeg`, else BtbN `linux64` / `linuxarm64` **LGPL** static `.tar.xz` |
| from | [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds/releases) `latest`, newest `nX.Y` release branch, checked against `checksums.sha256` | [ffmpeg.martin-riedl.de](https://ffmpeg.martin-riedl.de) `redirect/latest/macos/<arch>/release/ffmpeg.zip`, checked against the `.sha256` next to it | as Windows |

Why these: yt-dlp's own builds are the only official ones. BtbN's builds are linked from ffmpeg.org's download page (Windows), cover Windows and Linux on x64 and ARM64, publish LGPL variants and a checksum list with every build, and are the base of yt-dlp's own ffmpeg builds. Nobody publishes an LGPL ffmpeg build for macOS; Martin Riedl's is a long-running, scripted build with per-file checksums. It is GPL (it includes x264), which does not affect Whoosh: ffmpeg runs as a separate program the user's machine downloads from its author, Whoosh never links or ships it. Homebrew's ffmpeg is preferred when installed. (Intel macOS release builds there end in January 2027; a Homebrew ffmpeg keeps working.)

Managed copies live in `<data dir>/media/bin`, their versions and checksums in `<data dir>/media/tools.json`. Each download goes to a temporary directory, is hashed while it streams, and is deleted on a checksum mismatch before anything is unpacked or made executable. Archives are unpacked with the system's `tar` (bsdtar on Windows 10+ and macOS reads zip). Updates: only on `media.updateTools`, or at most once a day when the user turned on `autoUpdate`.

## Media downloads

`MediaService` wraps any `Service` (the engine). A media download (`md_…`) is one entry for clients:

1. **Prepare**: fetch yt-dlp if needed, `yt-dlp -J <page>` (reused from a `media.probe` within 15 minutes), then `yt-dlp --load-info-json … -f <selector> -J` to let yt-dlp pick the formats (`bv*+ba/b`, `bv*[height<=H]+ba/b[height<=H]`, `ba/b`, or exact ids).
2. **Direct formats** (plain HTTP(S), no fragments): each becomes a hidden engine download (`<name>.f<format>.<ext>` next to the target) with the headers and cookies yt-dlp reports for that URL. They get the engine's connections, journal, queue, scheduler and speed limit. The service hides them from `download.list`, `download.get` and every event, and reports their sum as the media download.
3. **Finish**: when every part is complete, ffmpeg merges (`-c copy`, `+faststart` for mp4), takes the audio out (`-vn -c:a copy`) or encodes MP3, into a temporary name that is renamed at the end; a single part that needs nothing is just renamed. Then the parts are removed.
4. **Expired links**: a part in `needs_refresh` makes the service ask yt-dlp for fresh links to the same format ids and hand them to the engine (`download.refreshUrl`), up to three times per run; `download.refreshUrl` on the media download does it on demand.
5. **Streaming formats** (HLS `m3u8`, DASH fragments, …): yt-dlp downloads them itself, with a progress template parsed from its output, at most two at a time, only while the download's queue runs. Pause stops yt-dlp; resume starts it again on its partial files.

Subtitles are written next to the file by `yt-dlp --skip-download --write-subs --write-auto-subs`. Jobs are saved in `<data dir>/media/jobs.json` (never cookies: those stay in memory), so a restart picks up where it was, including an interrupted merge.

**Safety.** No shell; `--ignore-config` (a user's yt-dlp config could add `--exec` or browser cookies); the page URL after `--`; format ids and subtitle languages are checked against tight character sets before they reach `-f` / `--sub-langs`; `%` in names is escaped in output templates; file names go through `whoosh_net::sanitize_filename` with path separators turned into dashes. A cookie reaches yt-dlp only when the client passed one for the page (the extension), as an owner-only temporary cookie file scoped to the page's host and deleted after the run; `--cookies-from-browser` is never used. No DRM circumvention, no site-specific workarounds.

## Tests

```bash
scripts/cargo-slot.sh cargo test -p whoosh-media
```

`tests/media.rs` has its own `main`: run with `WHOOSH_FAKE_TOOL=yt-dlp|ffmpeg` it *is* a fake yt-dlp or ffmpeg that prints canned JSON from `tests/fixtures/info.json` (with the labserver's address filled in), writes subtitles, simulates a yt-dlp download, or concatenates its inputs. The tests then run the real engine, whoosh-net and whoosh-store against the labserver: the merge path (the audio part only passes the labserver's Referer check if yt-dlp's `http_headers` reach the engine), audio only without ffmpeg, the yt-dlp fallback, bad input and cookies, removal, and the installer's checksum check with a fake upstream, including a mismatch. Nothing touches the network.
