# Connecting the browsers

How the installers link the browser extension to the app ([ADR 0007, amendment 1, *Connecting the browsers*](../../docs/adr/0007-phase2-plan.md#connecting-the-browsers-wave-4-release-engineering)).
Two things are needed in each browser:

1. **The native messaging host** (`whoosh-nmhost`) registered for the browser, with a manifest that allows our extension's IDs. Without it the extension cannot reach the app.
2. **The extension** itself, from the browser's store. Where the browser allows it, the installer asks the browser to *offer* it ("external extensions"); the browser then asks the user once to enable it. No enterprise policies are used.

## The one config file: `extension-ids.json`

| Field | What | Today |
|---|---|---|
| `firefox.id` | `browser_specific_settings.gecko.id`, fixed by us | `whoosh@coderzonebd.com` |
| `firefox.amoSlug` | the listing's slug on addons.mozilla.org | `null` (not listed yet) |
| `edge.id` | the ID Microsoft Edge Add-ons assigns | `null` |
| `chromeWebStore.id` | the ID the Chrome Web Store assigns | `null` (the US$5 account waits for the owner) |
| `chromeWebStore.externalInstall` | **the switch**: offer the extension to Chrome, Chromium, Brave and Vivaldi from the installers. Refused while `id` is `null` | `false` |
| `github.chromiumId` | the ID of the Chromium zip on GitHub Releases (and of development builds), fixed by the public `key` in the extension's manifest | `clhjgofaikdmabdnebgcmdffdjenagfl` |

It is read by:
- `whoosh-nmhost` (compiled in, `crates/nmhost/src/ids.rs`): the IDs every registration allows, and the offers `--offer-extension` writes;
- the desktop app (through `whoosh-nmhost`'s library): the "Connect your browser" page and its store links;
- [`generate.mjs`](generate.mjs): the files the MSI, `.deb` and `.rpm` install.

### When a store assigns an ID

1. Put it in `extension-ids.json` (for the Chrome Web Store, also set `externalInstall` to `true` once the listing is public or unlisted).
2. Run `node packaging/browsers/generate.mjs` and commit what it changed. `pnpm -r build` and `pnpm -r test` fail while the generated files are out of date.
3. `cargo test -p whoosh-nmhost` checks the IDs (32 letters `a`–`p`).

The next release then allows the new ID in every host manifest, and offers the extension from that store.

## What each installer does

| Installer | Host | Store offers | Uninstall |
|---|---|---|---|
| NSIS (Windows, per user) | `whoosh-nmhost --register --browser all --offer-extension` from [`hooks.nsh`](../../apps/desktop/src-tauri/installer/windows/hooks.nsh): manifests in `%LOCALAPPDATA%\whoosh\nmhost\`, `HKCU\Software\<vendor>\NativeMessagingHosts\com.coderzonebd.whoosh` | `HKCU\Software\Microsoft\Edge\Extensions\<edge id>`; `HKCU\Software\Google\Chrome\Extensions\<cws id>` with the switch | `whoosh-nmhost --unregister` removes both |
| MSI (Windows, per machine) | manifests next to the app, `HKLM\Software\<vendor>\NativeMessagingHosts\com.coderzonebd.whoosh` ([`browsers.wxs`](../../apps/desktop/src-tauri/installer/windows/browsers.wxs)) | `HKLM\Software\Wow6432Node\Microsoft\Edge\Extensions\<edge id>`; the Chrome key with the switch | Windows Installer removes them |
| `.deb`, `.rpm` | `/etc/opt/chrome/`, `/etc/chromium/`, `/etc/opt/edge/native-messaging-hosts/` and `/usr/lib/mozilla/native-messaging-hosts/` | `/usr/share/microsoft-edge/extensions/<edge id>.json`; `/usr/share/google-chrome/extensions/` and `/usr/share/chromium/extensions/<cws id>.json` with the switch | the package manager removes them |
| AUR | the same files as the `.deb` (from `installer/linux/files.txt`) | the same | pacman |
| `.dmg` (macOS), AppImage | the app registers the host for the user at every start where it is missing or stale (from an AppImage it first copies `whoosh-nmhost` to `<data dir>/bin/`) | macOS: the first run writes `~/Library/Application Support/<browser>/External Extensions/<id>.json` | not automatic: run `whoosh-nmhost --unregister` (inside `Whoosh.app/Contents/MacOS/`) before deleting the app |

On every platform the desktop app's **Connect your browser** page (first run, and Settings → Browsers) shows, per browser in use, whether the host is registered and the extension installed, and offers the next step: *Link to Whoosh* (registers the host), or the store page. Until a store lists the extension, Chromium-based browsers get the release page, where the GitHub Chromium zip can be loaded unpacked; it keeps the public key, so its ID is `github.chromiumId` and the host accepts it.

Firefox no longer installs extensions dropped into folders: the page opens its AMO listing (once `amoSlug` is set), and the user clicks *Add*.

## Not verified yet

- Whether Brave and Vivaldi read Chrome's external-extension key on Windows, and their system-wide host locations on Linux (they are given Chrome's).
- `/usr/share/microsoft-edge/extensions/` on Linux (follows Chromium's layout for Edge's product directory).
- Microsoft documents the Edge external-extension key under `HKLM`; whether `HKCU` works for per-user installs (Chromium reads both).
- Every path above was written from the browser vendors' documentation; the NSIS hooks and the WiX fragment run for the first time in the release workflow's dry run.
