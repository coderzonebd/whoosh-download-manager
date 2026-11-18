# Releasing

Every artifact is built by GitHub Actions from a tag; nothing is built on a
laptop and uploaded by hand (MASTER-PLAN §5). Releases are always created as
**drafts**: a maintainer reviews them and presses *Publish*.

- CI (every PR and every push to `dev` / `pub`): [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)
- Release (tags `v*`, or a manual dry run): [`.github/workflows/release.yml`](../.github/workflows/release.yml)
- Store and package-manager manifests: [`packaging/`](../packaging)

## What a release contains

| Asset | Built on | Signed when configured |
|---|---|---|
| `Whoosh-Download-Manager_<v>_x64-setup.exe`, `Whoosh-Download-Manager_<v>_arm64-setup.exe` (NSIS, per-user) | windows-2025, windows-11-arm | SignPath (exe inside and installer) |
| `Whoosh-Download-Manager_<v>_x64_en-US.msi` (WiX, per-machine; x64 only, not for pre-releases) | windows-2025 | SignPath |
| `Whoosh-Download-Manager_<v>_{x64,arm64}_portable.zip` (the exe with `wdm-daemon.exe`, `wdm-nmhost.exe` and `media-tools\` with yt-dlp and ffmpeg) | windows-2025, windows-11-arm | SignPath (all three) |
| `Whoosh-Download-Manager_<v>_universal.dmg` (Apple silicon + Intel) | macos-15 | Developer ID + notarization, stapled |
| `Whoosh-Download-Manager_<v>_universal.app.tar.gz` (updater archive) | macos-15 | updater key (below) |
| `Whoosh-Download-Manager_<v>_amd64.AppImage`, `Whoosh-Download-Manager_<v>_amd64.deb`, `Whoosh-Download-Manager-<v>-1.x86_64.rpm` | ubuntu-22.04 (glibc 2.35) | — (see *Not automated yet*) |
| `Whoosh-Download-Manager_<v>_aarch64.AppImage`, `Whoosh-Download-Manager_<v>_arm64.deb`, `Whoosh-Download-Manager-<v>-1.aarch64.rpm` | ubuntu-22.04-arm (glibc 2.35) | — |
| `wdm-<v>-<platform>.{tar.gz,zip}`: the command line `wdm` and `wdm-daemon` (Windows: also `wdm-autostart.exe`) | linux-x64, linux-arm64, macos-universal, windows-x64, windows-arm64 | SignPath / Developer ID + notarization |
| `wdm-nmhost-<v>-<platform>.{tar.gz,zip}` (native messaging host, for packagers) | same | same |
| `wdm-chromium-<v>-store.zip`, `wdm-firefox-<v>-store.zip` | ubuntu-24.04 | stores sign on upload |
| `wdm-chromium-<v>.zip` (GitHub build for Chrome, Brave, Vivaldi: load unpacked; keeps the public key, so the native host accepts it) | ubuntu-24.04 | — |
| `wdm-<v>-package-manifests.tar.gz` (winget, Scoop, Homebrew cask, Flathub, AUR, filled in with this release's URLs and hashes) | ubuntu-24.04 | covered by the attestation |
| `*.cargo.cdx.json` (one per crate), `wdm-desktop-<v>.npm.cdx.json`, `wdm-extension-<v>.npm.cdx.json` | ubuntu-24.04 | covered by the attestation |
| `wdm-<v>-source.tar.gz` (`git archive` of the tag) | ubuntu-24.04 | covered by the attestation |
| `*.sig`, `latest.json` (Tauri updater) | — | updater key (below) |
| `SHA256SUMS` | ubuntu-24.04 | covered by the attestation |

Every desktop installer and the portable zip also carry **yt-dlp and ffmpeg**
(with ffprobe on Windows and Linux) in the app's resources, `media-tools/`, so
video and audio sites work right after installation. The *Fetch yt-dlp and
ffmpeg for the installers* step runs `wdm-media-bundle` (crates/media): the
newest yt-dlp release and the newest ffmpeg build for the target from the same
upstreams, with the same checksum checks, as the first-use download (yt-dlp
releases; BtbN LGPL builds for Windows and Linux, x64 and ARM64; Martin
Riedl's GPL builds for macOS, arm64 and x86_64 joined with `lipo`). Nothing is
pinned: each release takes what is newest, and the versions, source URLs and
checksums are in `media-tools/manifest.json` and in the job summary. The
licence texts, notice and written source offer next to them come from
[packaging/media-tools](../packaging/media-tools). After bundling, *Check
yt-dlp and ffmpeg inside the bundles* verifies the copies in the `.app`, the
`.deb` and the AppImage against the manifest (Windows: the portable zip's).
On Linux the programs are packed in uncompressed tars, because the AppImage
bundler (linuxdeploy) rewrites every ELF program in the image; the check
covers the tar and the program in it.
They add roughly 100–200 MB to each installed app (less to the compressed
installers); the command-line archives do not include them.

`wdm-labserver` (the engine's fault-injecting test server) is not released;
it is built in CI and as the `packaging/docker` image only.

ARM64 desktop builds run on GitHub's free arm64 runners (`ubuntu-22.04-arm`,
`windows-11-arm`), which are available to public repositories. Windows ARM64
ships NSIS and the portable zip only: Tauri's WiX (MSI) bundler does not target
ARM64. macOS has been universal (Apple silicon + Intel) from the start.

Every file also gets a **build provenance attestation** (SLSA, signed through
Sigstore with the workflow's OIDC identity) via
`actions/attest-build-provenance`.

The extension zips are the store builds: `node scripts/build.mjs --store --zip`,
without the development `key` (the job fails if a key is left in).

## Cutting a release

1. Pick the version `X.Y.Z` (or `X.Y.Z-beta.N`; pre-releases skip the MSI
   because WiX only accepts numeric versions).
2. Set it in all six places, which the release job checks against the tag:
   - `Cargo.toml` → `[workspace.package] version`
   - `apps/desktop/src-tauri/tauri.conf.json` → `version`
   - `apps/desktop/package.json` → `version`
   - `apps/extension/package.json` → `version`
   - `apps/mobile/src-tauri/tauri.conf.json` → `version`
   - `apps/mobile/package.json` → `version`

   Run `cargo check` so `Cargo.lock` follows, and commit on `dev` (via a PR).
   Two versions are derived, not set: the browser extension's (numbers only; a pre-release
   becomes a version just below its release, `apps/extension/scripts/manifest.mjs`,
   `browserVersion`) and Android's versionCode (`(major*1e6 + minor*1e3 + patch)*100`, plus
   the pre-release number or 99 for the release; the *Android* job in `release.yml`).
   Then open the **release PR `dev → pub`** and merge it with a merge commit
   ([ADR 0006](adr/0006-dev-and-pub-branches.md)).
3. Tag the merge commit on `pub` and push the tag (the release workflow
   refuses a tag that is not on `pub`):
   ```bash
   git tag -s vX.Y.Z -m "vX.Y.Z"   # -a if you do not sign tags yet
   git push origin vX.Y.Z
   ```
4. Wait for **Release** to finish. It creates (or, on a re-run, refreshes) a
   draft release with every asset, `SHA256SUMS` and generated notes. It never
   touches a release that is already published.
5. Review the draft: download a few assets, check them as in
   [Verifying a download](#verifying-a-download), edit the notes, then
   **Publish**. Mark it "latest" only for stable versions.
6. Afterwards: upload the store zips, and submit the package-manager
   manifests from `wdm-<v>-package-manifests.tar.gz` (the *Package
   manifests* job filled in the version, URLs and SHA-256s with
   `.github/scripts/package-manifests.mjs`; `packaging/*/README.md` says how
   to submit each). Nothing is submitted automatically.

### Dry run

*Actions → Release → Run workflow* on any branch builds everything a tag
would build (every platform, ARM64, installers, sidecars, package manifests)
and keeps the files as run artifacts for 7 days. It creates no release and
uses **no environment**: the `release` environment, and with it every
signing secret, is used only by a run for a `v*` tag, so the environment can
stay restricted to tags and a dry run is always unsigned. The version comes
from `tauri.conf.json`.

## Signing

Each signing path switches itself on only when its secrets exist, so the
pipeline produces unsigned artifacts until then (and always on a dry run,
which never sees the secrets). The switches are evaluated
in the job's `env` (`SIGNPATH_ENABLED`, `APPLE_SIGNING_ENABLED`) and by the
*Decide on updater artifacts* step (`UPDATER_ARTIFACTS`), and shown in the
run log.

| What | How | Where in `release.yml` |
|---|---|---|
| Windows exe, NSIS, MSI, `wdm.exe`, `wdm-daemon.exe`, `wdm-autostart.exe`, `wdm-nmhost.exe` | [SignPath](https://signpath.io) (free for OSS through the [SignPath Foundation](https://signpath.org)); the unsigned files are uploaded as a short-lived artifact, SignPath signs them and the signed files replace them | `desktop` (Windows): build `--no-bundle` → sign the app exe and the sidecars (`wdm-daemon.exe`, `wdm-nmhost.exe`) in one request → `tauri bundle --no-binary-patching` → sign installers; `binaries` (Windows) |
| macOS `.app` | Tauri CLI with `APPLE_*` variables: Developer ID, hardened runtime, notarization, stapling | `desktop` (macOS) |
| macOS yt-dlp and ffmpeg inside the `.app` | `codesign` before bundling: Developer ID (hardened runtime; yt-dlp with `packaging/media-tools/yt-dlp.entitlements`) when configured, else ad hoc like the app; then `wdm-media-bundle rehash` | *Sign yt-dlp and ffmpeg (macOS)* |
| macOS `.dmg` | `codesign` + `notarytool` + `stapler` | `.github/scripts/macos-sign.sh`, `macos-notarize.sh` |
| macOS `wdm`, `wdm-daemon`, `wdm-nmhost` | `codesign` (hardened runtime), zipped and notarized (bare binaries cannot be stapled; Gatekeeper checks online) | `binaries` (macOS) |
| Updater artifacts | minisign key via `TAURI_SIGNING_PRIVATE_KEY`: `createUpdaterArtifacts` on macOS/Linux, `tauri signer sign` after SignPath on Windows; `latest.json` is generated from the `.sig` files | `desktop`, `release` |
| Everything | Sigstore build provenance attestation | `release` |

Unsigned macOS builds are ad-hoc signed (`bundle.macOS.signingIdentity: "-"`)
so they at least launch on Apple silicon after *right-click → Open*.

Windows note: `tauri bundle` normally patches the bundle type into the exe,
which would break its Authenticode signature, so the Windows job bundles with
`--no-binary-patching`. The cost: an installed copy cannot tell whether it came
from NSIS or MSI, so the updater uses the generic `windows-x86_64` entry,
which points at the NSIS installer. Fleet (MSI) installs should be updated
with MSI tooling, not the in-app updater.

## Secrets and variables

Store secrets in the **`release` environment** (*Settings → Environments*),
not as repository secrets: only the `desktop` and `binaries` jobs of
`release.yml` reference that environment, so CI and pull requests can never
read them. Restrict the environment's deployments to tags matching `v*`, and
optionally add required reviewers.

| Name | Kind | Purpose | How to get it |
|---|---|---|---|
| `SIGNPATH_API_TOKEN` | secret | SignPath CI user token | SignPath → *Users* → a CI user → API token; give it *Submitter* on the project |
| `SIGNPATH_ORGANIZATION_ID` | variable | SignPath organisation id | SignPath → *Settings* |
| `SIGNPATH_PROJECT_SLUG` | variable | SignPath project | the project you create for this repository |
| `SIGNPATH_SIGNING_POLICY_SLUG` | variable, optional | defaults to `release-signing`; use `test-signing` for trials | SignPath project → *Signing policies* |
| `APPLE_CERTIFICATE` | secret | *Developer ID Application* certificate + private key, `.p12`, base64 | Keychain Access → export `.p12` → `base64 -i cert.p12 \| pbcopy` |
| `APPLE_CERTIFICATE_PASSWORD` | secret | password of that `.p12` | chosen at export |
| `APPLE_SIGNING_IDENTITY` | secret | e.g. `Developer ID Application: CoderZone BD (TEAMID1234)` | `security find-identity -v -p codesigning` |
| `APPLE_API_KEY_ID` | secret | notarization (preferred): App Store Connect API key id | App Store Connect → *Users and Access* → *Integrations* → *Team Keys* (role *Developer*) |
| `APPLE_API_ISSUER` | secret | issuer UUID of that key | same page |
| `APPLE_API_KEY_P8` | secret | contents of `AuthKey_<id>.p8` (multi-line is fine) | downloaded once when the key is created |
| `APPLE_ID` | secret | notarization (alternative): Apple ID e-mail | — |
| `APPLE_APP_SPECIFIC_PASSWORD` | secret | app-specific password for that Apple ID | appleid.apple.com → *Sign-In and Security* |
| `APPLE_TEAM_ID` | secret | 10-character team id (needed with `APPLE_ID`) | developer.apple.com → *Membership* |
| `WHOOSH_CRASH_DSN` | secret | GlitchTip DSN for opt-in crash reports, compiled into the daemon and the desktop app (`crates/crash`, which reads it as `WDM_CRASH_DSN` at build time); without it a build cannot send reports. Passed to the `desktop` and `binaries` jobs on `v*` tag runs only, so dry runs build without it | GlitchTip → project → *Client Keys (DSN)* |
| `TAURI_SIGNING_PRIVATE_KEY` | secret | updater signing key (minisign); its public half is pinned in `tauri.conf.json` | the key file generated for this repository (see *Updater key*); store the file's contents; keep an offline backup |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | secret | its password | the current key has none: leave the secret unset or empty |

Notarization uses the API key when all three `APPLE_API_*` secrets are set,
otherwise `APPLE_ID` + `APPLE_APP_SPECIFIC_PASSWORD` + `APPLE_TEAM_ID`; with
neither, macOS artifacts are signed but not notarized (warning in the log).

`GITHUB_TOKEN` covers the draft release and the attestation; no personal token
is needed. Not used yet, but will be for automated store/package submissions:
`CHROME_WEBSTORE_*` (client id/secret/refresh token), `AMO_JWT_ISSUER` /
`AMO_JWT_SECRET`, `WINGET_TOKEN`, `SNAPCRAFT_STORE_CREDENTIALS`,
`AUR_SSH_KEY`, a GPG key for apt/dnf repositories, `DOCKERHUB_TOKEN`.

### Updater key

The app ships `tauri-plugin-updater` (`apps/desktop/src-tauri/src/updates.rs`)
with the **public** key pinned in `tauri.conf.json` (`plugins.updater.pubkey`)
and the endpoint
`https://raw.githubusercontent.com/coderzonebd/whoosh-download-manager/updater/latest.json`.
Settings → Updates has *Check for a new version* (download, install, restart)
and a daily check, on by default, that only shows a notice. An update whose
signature does not match the pinned key is refused, whether or not the
installer was code-signed.

The key pair was generated for this repository with
`tauri signer generate` (no password). Only the public key is in the
repository; the private key was handed to the owner outside it. The release
workflow produces `*.sig` files and `latest.json` as soon as the
`TAURI_SIGNING_PRIVATE_KEY` secret exists (*Decide on updater artifacts*);
without it, releases build exactly as before and the app finds no update.

- **Losing the private key** means existing installs can never auto-update
  again (a new key needs a new public key in the app, which only a manual
  install delivers). Back it up offline.
- **The feed.** GitHub's `releases/latest` skips pre-releases, so the app
  does not read it. When a release is **published** (not while it is a
  draft: the assets would not download yet),
  [`updater-feed.yml`](../.github/workflows/updater-feed.yml) copies that
  release's `latest.json` to the `updater` branch: `latest.json` for every
  release, pre-releases included, and `stable.json` for full releases only.
  A file only ever moves to a newer version, so publishing a backport does
  not roll users back. It needs no secret (`GITHUB_TOKEN` may push to
  `updater`; only `dev` and `pub` are protected) and commits in the name of
  whoever published the release. To re-run it: *Actions → Updater feed →
  Run workflow* with the tag. Signatures are still checked against the
  pinned key, so whoever can write the branch still cannot ship an update.
- **Keeping stable users off betas** (after v1.0.0): point the app at
  `stable.json` instead (`plugins.updater.endpoints` in `tauri.conf.json`),
  or add a "Get beta versions" setting and choose the endpoint at run time
  (`app.updater_builder().endpoints(...)` in `updates.rs`). Users who are on
  a beta keep getting `latest.json` until they switch. Until then every
  install follows `latest.json`, so publishing a pre-release after v1.0.0
  offers it to everyone.
- **Package managers:** installs from winget (NSIS) update in place like any
  NSIS install. Scoop, Homebrew, Flathub and AUR installs should update
  through their package manager; the in-app check still offers the update,
  and installing it there replaces the package manager's copy (Windows,
  macOS) or fails (Flatpak). Switching the check off is in Settings.
- `.deb` and `.rpm` installs are updated by the plugin when the release has
  signed `.deb`/`.rpm` entries in `latest.json` (it has them; Tauri's
  support for these formats is newer than for the AppImage and has not been
  tried here).

## What the owner sets up for a free release

Everything below is free. Until each item exists, the matching path in
`release.yml` stays off by itself and the release is built without it.

1. **Updater key** (needed for in-app updates). Store the private key file
   handed over with this change as the secret `TAURI_SIGNING_PRIVATE_KEY`
   in the `release` environment (*Settings → Environments → release →
   Add secret*; paste the whole file content, one line of base64). No
   password secret is needed. Keep an offline backup of the file, then
   delete the local copy.
2. **SignPath Foundation** (Windows Authenticode). Apply at
   <https://signpath.org/apply> with the public repository. Once accepted:
   create a project for this repository, paste
   `packaging/signpath/artifact-configuration.xml` as its artifact
   configuration, create a CI user with *Submitter* rights, then set
   `SIGNPATH_API_TOKEN` (secret) and `SIGNPATH_ORGANIZATION_ID`,
   `SIGNPATH_PROJECT_SLUG` (variables; optionally
   `SIGNPATH_SIGNING_POLICY_SLUG`) in the `release` environment. SignPath
   asks a maintainer to approve each release-signing request.
3. **Firefox Add-ons (AMO)**. Create an account at
   <https://addons.mozilla.org/developers/>, submit
   `wdm-firefox-<v>-store.zip` from a release (AMO also asks for the
   source: `wdm-<v>-source.tar.gz` and `apps/extension/store/amo.md`).
   The add-on ID is already fixed (`whoosh@coderzonebd.com`). When it is
   listed, put its slug in `packaging/browsers/extension-ids.json`
   (`firefox.amoSlug`) so the app links to it.
4. **Microsoft Edge Add-ons**. Register in Microsoft Partner Center
   (<https://partner.microsoft.com/dashboard/microsoftedge/>; free), submit
   `wdm-chromium-<v>-store.zip`. Once it has an ID, put it in
   `packaging/browsers/extension-ids.json` (`edge.id`), run
   `node packaging/browsers/generate.mjs`, and merge that change: from the
   next release the native host accepts the Edge build and the installers
   offer it to Edge.
5. **Later, paid:** the Chrome Web Store (one-time US$5): its ID goes into
   `chromeWebStore.id`, and `chromeWebStore.externalInstall: true` turns on
   the offer to Chrome, Brave and Vivaldi. Apple Developer (US$99/year) for
   notarization: the `APPLE_*` secrets below.

How the IDs reach the installers and the app:
[`packaging/browsers/README.md`](../packaging/browsers/README.md).

### Crash reports

`WHOOSH_CRASH_DSN` (secret, in the `release` environment) is the hosted
GlitchTip project's DSN. `release.yml` passes it to the builds as
`WDM_CRASH_DSN`, which is read when the app and the daemon are
**compiled** (`option_env!` in `crates/crash`), so it must be in the
environment of the build steps that compile `wdm-daemon` and the desktop
app. Builds with it send crash reports **by default**; users can turn them off
in *Settings → Privacy*, and the app tells them once on first run. Builds
without it (pull requests, local builds, dry runs without the secret) contain
no active reporter, hide the switch and send nothing. What a report holds:
[docs/privacy.md](privacy.md).

## Android

The *Android* job builds the app (`apps/mobile`) for 64-bit and 32-bit ARM phones and x86_64
(emulators, Chromebooks): `Whoosh-Download-Manager_<v>_android.apk` for the GitHub release and
`Whoosh-Download-Manager_<v>_android.aab` for Google Play, both in `SHA256SUMS`.

Every update of an Android app must be signed with the same key as the first version a user
installed, so the key is made once and kept for good:

1. Make the upload keystore (on your own computer; keep two offline backups):
   ```bash
   keytool -genkeypair -v -keystore wdm-upload.jks -alias wdm -keyalg RSA -keysize 4096 -validity 10000
   ```
2. In the `release` environment, add the secrets `ANDROID_KEYSTORE_BASE64`
   (`base64 -w0 wdm-upload.jks`), `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` (`wdm`) and
   `ANDROID_KEY_PASSWORD`.
3. On Google Play, enrol in Play App Signing with this key as the upload key.

Without the keystore a dry run signs with a throwaway debug key (installs for testing), and a
tagged release ships no Android app rather than one users could never update.

## Package channels

Installing takes one command on every system (README, *Install*), and the app keeps itself up
to date afterwards; the package manager it came from sees each new version, so nothing fights:

| Channel | Install | Who updates | How the package manager knows |
|---|---|---|---|
| winget | `winget install --id CoderZoneBD.WDM -e` | the app (signed in-app update, NSIS/MSI) | the installer writes the new version to Windows' list of programs, which winget reads |
| Homebrew cask | `brew install --cask coderzonebd/tap/whoosh-download-manager` | the app | the cask says `auto_updates true`, so `brew upgrade` leaves it to the app |
| apt / dnf / zypper (Cloudsmith repository) | `packaging/install/install`, or the repository by hand | the app, through `dpkg -i` / `rpm -U` (asks for the password once, with pkexec); `apt upgrade` / `dnf upgrade` work too | dpkg's and rpm's own database |
| AppImage, `.dmg`, NSIS from the releases page | `packaging/install/install` | the app | — |
| AUR (`wdm`, built from source) | `yay -S wdm` | pacman / the AUR helper; the app does not update itself | it is pacman's package |
| Flathub | `flatpak install flathub com.coderzonebd.whoosh` | the software centre (`flatpak update`); not the app (sandboxed) | it is Flatpak's |
| Scoop | not published: a self-updating app and Scoop's versioned folders would disagree; the manifest stays in `packaging/scoop` for later | — | — |

The app's own updates: `apps/desktop/src-tauri/src/updates.rs`. With *Keep Whoosh Download
Manager up to date by itself* on (the default), it checks once a day, downloads a new version
quietly, and installs it when nothing is downloading and the window is closed, or when the user
quits; downloads resume after the restart. A `.deb` or `.rpm` asks first (a toast with
*Install*), since installing needs the administrator's password. Where a package manager owns
the copy (Flatpak, Scoop, an AUR build), Settings says so and shows its update command.

When a release is published, `.github/workflows/publish-channels.yml` hands it on. Each job runs
only when its secret exists in the `release` environment:

| Secret / variable | Channel | What to create |
|---|---|---|
| `CLOUDSMITH_API_KEY` (secret), `CLOUDSMITH_REPO` (variable, `owner/repository`), optionally `CLOUDSMITH_BETA_REPO` | apt and rpm repository | the open-source repository `coderzonebd/whoosh-download-manager` (created with *Repositories → New repository*, *Broadcast* on, then *Open source*, licence GPL-3.0-or-later, project URL this repository; a plain *Public* repository is only a trial. [Policy](https://docs.cloudsmith.com/resources/open-source-hosting-policy): the README credits Cloudsmith as the host); the API key of a user with write access. `packaging/install/install` points at it (`CLOUDSMITH`, `REPO_KEY`): change both if the repository or its GPG key changes. A deleted repository's name stays taken for a while |
| `WINGET_TOKEN` (secret) | winget | a GitHub token (classic, `public_repo`) of an account that forked `microsoft/winget-pkgs`. The first version is submitted by hand from the release's package manifests (`packaging/winget/README.md`); Komac updates it from then on |
| `HOMEBREW_TAP_DEPLOY_KEY` (secret), `HOMEBREW_TAP` (variable, default `coderzonebd/homebrew-tap`) | Homebrew | the public `coderzonebd/homebrew-tap` repository and an SSH deploy key with write access on it (its private half is the secret); no personal token |
| `AUR_SSH_PRIVATE_KEY` (secret) | AUR | an AUR account with that SSH key; the first push creates the `wdm` package |

Pre-releases go only to `CLOUDSMITH_BETA_REPO` (when set); winget, Homebrew and the AUR take full
releases. Flathub is a pull request to `flathub/flathub` by hand (`packaging/flatpak/README.md`).

## Accounts to create

For v1.0 the owner chose distribution at no cost: no Apple Developer Program and no Chrome Web Store fee for now ([ADR 0007, Amendment 1](adr/0007-phase2-plan.md#amendment-1-the-road-to-v10-at-no-cost-2026-11-14)). The paid rows below stay for later.

| Account | For | Cost / lead time |
|---|---|---|
| GitHub organisation + public repository | everything; artifact attestations on private repositories need GitHub Enterprise Cloud, and the free `ubuntu-22.04-arm` runner is meant for public repositories | free |
| [SignPath Foundation](https://signpath.org/apply) | Windows Authenticode signing | free for OSS; the application is reviewed, and each release-signing request is approved by a maintainer |
| [Apple Developer Program](https://developer.apple.com/programs/) (organisation; D-U-N-S number needed) | Developer ID certificate, notarization | US$99/year; D-U-N-S and enrolment can take weeks |
| Chrome Web Store developer account | store extension | one-time US$5 |
| addons.mozilla.org account | Firefox extension (AMO also wants the source archive for review) | free |
| Microsoft Partner Center (later) | Microsoft Store / MSIX | free for individuals, fee for companies |
| Cloudsmith (open source), Homebrew tap repository, AUR, winget-pkgs fork, Flathub | package channels (*Package channels* above) | free |

## GitHub settings to switch on after the first push

- *Settings → Code security*: **Private vulnerability reporting** (SECURITY.md
  relies on it), Dependabot alerts and security updates, secret scanning with
  push protection.
- *Settings → Actions → General*: workflow permissions **read-only**;
  optionally *Require actions to be pinned to a full-length commit SHA*
  (every action here already is).
- *Settings → Environments*: create `release` as described above.
- *Settings → Rules*: run `scripts/github-setup.sh OWNER/REPO` once. It makes
  `pub` the default branch (visitors see the latest release) and applies `.github/rulesets/*.json`: `dev` needs a
  reviewed PR with the **CI OK** check; `pub` takes release PRs from `dev` only
  (**CI OK** + **PR source is dev**), merge commits only; tags `v*` cannot be
  deleted or moved. No force pushes or deletion on either branch.
- `.github/CODEOWNERS` is a commented placeholder; fill in real teams, then
  optionally require code-owner review.

## Verifying a download

```bash
# 1. Checksums (from the folder you downloaded into)
sha256sum --check --ignore-missing SHA256SUMS

# 2. Build provenance: built by this repository's release workflow from this tag
gh attestation verify Whoosh-Download-Manager_0.1.0_amd64.AppImage --repo <owner>/<repo>
```

- Windows: file *Properties → Digital Signatures*, or
  `Get-AuthenticodeSignature .\Whoosh-Download-Manager_0.1.0_x64-setup.exe` (PowerShell).
- macOS: `spctl --assess --type open --context context:primary-signature -v Whoosh-Download-Manager_0.1.0_universal.dmg`
  and, after installing, `spctl --assess -v "/Applications/Whoosh Download Manager.app"`
  (both should say *Notarized Developer ID*).
- Updater signatures (`*.sig`) and the updater public key are base64-encoded
  minisign files: `base64 -d file.sig > file.minisig`, `base64 -d key.pub > key.minisign.pub`,
  then `minisign -Vm file -x file.minisig -p key.minisign.pub`.

## Reproducibility

Done: pinned Rust toolchain (`rust-toolchain.toml`), `--locked` /
`--frozen-lockfile`, pinned action SHAs, `SOURCE_DATE_EPOCH` = commit time,
`--remap-path-prefix` for the workspace and Cargo home, deterministic
extension zips and CLI tarballs, no build caches in release jobs.

Not yet verified: byte-for-byte rebuilds. Known sources of difference are the
installer formats (NSIS, WiX, dmg, AppImage, rpm embed timestamps or random
ids) and code signatures. Rebuilding the CLI binaries and extension zips from
the source archive and comparing hashes is the first target (MASTER-PLAN §8,
Phase 4).

## Not automated yet

- Store uploads (Chrome Web Store, AMO, Edge Add-ons, Microsoft Store) and
  package-manager submissions (winget, Homebrew, Scoop, Flathub, AUR, Snap,
  Nix, Chocolatey). The manifests are filled in per release
  (`wdm-<v>-package-manifests.tar.gz`); submitting them is by hand.
- Linux package signing and apt/dnf repositories; Snap and Nix packaging;
  MSIX; an MSI for Windows ARM64.
- Publishing the Docker image (it is only built in CI).
- Notarizing the `.app` with the bundled yt-dlp and ffmpeg signed by our
  Developer ID has not been tried yet (no certificate so far); the yt-dlp
  entitlements in `packaging/media-tools/yt-dlp.entitlements` are untested.
- Martin Riedl stops publishing Intel macOS release builds of ffmpeg in
  January 2027; from then the universal macOS build cannot fetch the x86_64
  ffmpeg and the *Fetch yt-dlp and ffmpeg* step fails. Decide before then
  (another x86_64 source, or an arm64-only ffmpeg with the first-use download
  on Intel Macs).
- A "Get beta versions" setting (see *Updater key*); today everyone follows
  `latest.json`.
