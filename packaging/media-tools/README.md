# Media tools in the installers

> **Not used by the release installers any more** (they would be ten times larger): the app
> fetches yt-dlp, ffmpeg and Deno in the background after its first start (`Tools::prefetch`).
> What follows still applies to `wdm-media-bundle` and `--media-tools-dir`, for distribution
> packages and offline setups.

The desktop installers ship yt-dlp, ffmpeg, Deno and QuickJS (`qjs`), the
JavaScript runtimes yt-dlp uses for YouTube when the computer has none (Deno
first, QuickJS as the fallback), so video and audio
sites work right after installation. The release workflow fetches them for each target
with `wdm-media-bundle` (crates/media, `bundle` module: the same upstreams
and checksum checks as the first-use download) into
`apps/desktop/src-tauri/media-tools/`, and `tauri.media.conf.json` puts that
directory into the app's resources.

This directory holds what is copied next to the programs:

| File | In the bundle as | For |
|---|---|---|
| `NOTICE.txt` | `NOTICE.txt` | what is shipped, where from, licences, source and written offer |
| `LGPL-2.1.txt` | `LGPL-2.1.txt` | ffmpeg on Windows and Linux (BtbN LGPL builds) |
| `Unlicense.txt` | `Unlicense.txt` | yt-dlp ([its LICENSE](https://github.com/yt-dlp/yt-dlp/blob/master/LICENSE)) |
| `QuickJS-MIT.txt` | `QuickJS-MIT.txt` | QuickJS ([its LICENSE](https://github.com/quickjs-ng/quickjs/blob/master/LICENSE)) |
| `Deno-MIT.txt` | `Deno-MIT.txt` | Deno ([its LICENSE](https://github.com/denoland/deno/blob/main/LICENSE.md)) |
| `deno.entitlements` | (not copied) | Deno's hardened-runtime entitlements for Developer ID signing on macOS (V8 needs JIT memory) |
| `../../LICENSE-GPL` | `GPL-3.0.txt` | ffmpeg on macOS (Martin Riedl's GPL build) |

`wdm-media-bundle` adds `manifest.json` (versions, upstream files and
checksums, the SHA-256 of every shipped file) and, for BtbN builds,
`LICENSES/ffmpeg-build-LICENSE.txt` from the archive.

On Linux the programs are shipped as uncompressed tars (`yt-dlp.tar`,
`ffmpeg.tar`, `ffprobe.tar`, `qjs.tar`, `deno.tar`): the AppImage bundler (linuxdeploy) rewrites
every ELF program in the image, a tar it leaves alone. The manifest has the
SHA-256 of each tar and of the program inside it; the daemon unpacks them
with the system's `tar`. macOS and Windows ship the programs as they are.

Where the directory ends up:

| Bundle | Path |
|---|---|
| NSIS, MSI, portable zip | `<install dir>\media-tools\` |
| macOS `.app` (dmg) | `Whoosh Download Manager.app/Contents/Resources/media-tools/` |
| deb, rpm | `/usr/lib/Whoosh Download Manager/media-tools/` |
| AppImage | `usr/lib/Whoosh Download Manager/media-tools/` inside the image |

The app passes it to the daemon (`--media-tools-dir`), which copies the
programs into `<data dir>/media/bin` and updates them there.
