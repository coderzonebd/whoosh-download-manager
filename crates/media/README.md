# whoosh-media

Video and audio sites for desktop and GitHub builds: yt-dlp finds the media on a page, Whoosh Download Manager's engine downloads it, ffmpeg joins video and audio. Store builds never include it (MASTER-PLAN §6). License: Apache-2.0. Protocol: [PROTOCOL.md, "Media"](../../docs/api/PROTOCOL.md#media-video-and-audio-sites).

```rust
let engine: Arc<dyn Service> = whoosh_engine::Engine::start(deps, config).await?;
let service = whoosh_media::MediaService::start(engine, MediaConfig::new(data_dir.join("media"))).await?;
// serve `service`: everything the engine does, plus media.* and the `media` capability
```

`whoosh-daemon` does exactly this when built with its `media` feature (the default) and not started with `--no-media`.

## The tools

yt-dlp and ffmpeg are separate programs. They are never linked, and run with an argument array (no shell). The desktop installers ship them (see [Shipped with the desktop app](#shipped-with-the-desktop-app)); otherwise `Tools` finds or fetches them on first use. Either way they come from:

| | Windows | macOS | Linux |
|---|---|---|---|
| yt-dlp | `yt-dlp.exe` / `yt-dlp_arm64.exe` | `yt-dlp_macos` | system `yt-dlp`, else `yt-dlp_linux` / `_aarch64` / `musllinux` |
| from | [yt-dlp releases](https://github.com/yt-dlp/yt-dlp/releases), checked against `SHA2-256SUMS` of the same release | | |
| ffmpeg | BtbN `win64` / `winarm64` **LGPL** static zip | system (Homebrew) `ffmpeg`, else Martin Riedl's release build | system `ffmpeg`, else BtbN `linux64` / `linuxarm64` **LGPL** static `.tar.xz` |
| from | [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds/releases) `latest`, newest `nX.Y` release branch, checked against `checksums.sha256` | [ffmpeg.martin-riedl.de](https://ffmpeg.martin-riedl.de) `redirect/latest/macos/<arch>/release/ffmpeg.zip`, checked against the `.sha256` next to it | as Windows |

Why these: yt-dlp's own builds are the only official ones. BtbN's builds are linked from ffmpeg.org's download page (Windows), cover Windows and Linux on x64 and ARM64, publish LGPL variants and a checksum list with every build, and are the base of yt-dlp's own ffmpeg builds. Nobody publishes an LGPL ffmpeg build for macOS; Martin Riedl's is a long-running, scripted build with per-file checksums. It is GPL (it includes x264), which does not affect Whoosh Download Manager's own licence: ffmpeg is a separate program, never linked. The macOS installer ships it with its licence, source references and a written offer for the source ([packaging/media-tools](../../packaging/media-tools)). Without a managed copy, Homebrew's ffmpeg is used when installed. (Intel macOS release builds there end in January 2027; a Homebrew ffmpeg keeps working.)

Managed copies live in `<data dir>/media/bin`, their versions and checksums in `<data dir>/media/tools.json`. A managed copy wins over a system one. Each download goes to a temporary directory, is hashed while it streams, and is deleted on a checksum mismatch before anything is unpacked or made executable. Archives are unpacked with the system's `tar` (bsdtar on Windows 10+ and macOS reads zip). Updates: only on `media.updateTools`, or at most once a day when the user turned on `autoUpdate`.

### Shipped with the desktop app

So that video and audio sites work right after installation, every desktop installer (NSIS, MSI, portable zip, dmg, AppImage, deb, rpm; x64 and ARM64) carries both tools, fetched at release time by `whoosh-media-bundle` (`src/bin`, module `bundle`) from the same upstreams with the same checksum checks as above:

- `whoosh-media-bundle fetch --target <triple> --out <dir>` takes the newest builds for the target, checks each download against upstream's checksum, unpacks it and writes the programs (ffmpeg with ffprobe where the archive has it) and `manifest.json`: versions, upstream URLs and checksums, and the SHA-256 of every shipped file. For `universal-apple-darwin` it fetches the arm64 and x86_64 ffmpeg and joins them with `lipo` (`yt-dlp_macos` is universal already); different versions for the two fail the build. For Linux targets each program is shipped in an uncompressed `<name>.tar` instead: the AppImage bundler (linuxdeploy) rewrites every ELF program it finds in the image (rpath, stripping), but leaves a tar alone; the manifest records the SHA-256 of both the tar and the program in it. `rehash <dir>` records the checksums again after signing (macOS); `verify <dir>` checks a built bundle's copy (for a tar, the tar and the program in it).
- The release workflow puts the directory into the app's resources as `media-tools/` ([packaging/media-tools](../../packaging/media-tools) lists where that is in each bundle). The desktop app starts the daemon with `--media-tools-dir <resources>/media-tools` when it has a manifest.
- At start, `Tools` shows each tool the bundle has as ready (source `managed`, the bundle's version) and copies it into `bin` in the background, each file (on Linux first unpacked from its tar with the system's `tar` into a work directory under `bin`) checked against the manifest's SHA-256 while it is copied (a plain byte copy: no quarantine attribute comes along). They are never run from the bundle: `get` returns nothing until the copy is in place and `ensure` copies first. From then on it is an ordinary managed copy, updated from upstream by `media.updateTools` or the daily check. An app update that ships a newer version replaces an older managed copy; an older one never replaces a newer one.
- A file that does not match the manifest is not used; the tool is downloaded on first use as without a bundle. The command line, the CLI archives and source builds (AUR) ship no tools and keep the first-use download.

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

`tests/media.rs` has its own `main`: run with `WHOOSH_FAKE_TOOL=yt-dlp|ffmpeg` it *is* a fake yt-dlp or ffmpeg that prints canned JSON from `tests/fixtures/info.json` (with the labserver's address filled in), writes subtitles, simulates a yt-dlp download, or concatenates its inputs. The tests then run the real engine, whoosh-net and whoosh-store against the labserver: the merge path (the audio part only passes the labserver's Referer check if yt-dlp's `http_headers` reach the engine), audio only without ffmpeg, the yt-dlp fallback, bad input and cookies, removal, the installer's checksum check with a fake upstream, including a mismatch, and the bundled tools: shown as ready and copied without the network, a manifest mismatch falling back to the download, and newer bundled versions replacing older copies (but not the reverse). Nothing touches the network.
