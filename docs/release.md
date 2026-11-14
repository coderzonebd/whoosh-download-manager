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
| `Whoosh_<v>_x64-setup.exe`, `Whoosh_<v>_arm64-setup.exe` (NSIS, per-user) | windows-2025, windows-11-arm | SignPath (exe inside and installer) |
| `Whoosh_<v>_x64_en-US.msi` (WiX, per-machine; x64 only, not for pre-releases) | windows-2025 | SignPath |
| `Whoosh_<v>_{x64,arm64}_portable.zip` (the exe with `whoosh-daemon.exe` and `whoosh-nmhost.exe`) | windows-2025, windows-11-arm | SignPath (the app exe; see *Not automated yet* for the sidecars) |
| `Whoosh_<v>_universal.dmg` (Apple silicon + Intel) | macos-15 | Developer ID + notarization, stapled |
| `Whoosh_<v>_universal.app.tar.gz` (updater archive) | macos-15 | updater key (below) |
| `Whoosh_<v>_amd64.AppImage`, `Whoosh_<v>_amd64.deb`, `Whoosh-<v>-1.x86_64.rpm` | ubuntu-22.04 (glibc 2.35) | — (see *Not automated yet*) |
| `Whoosh_<v>_aarch64.AppImage`, `Whoosh_<v>_arm64.deb`, `Whoosh-<v>-1.aarch64.rpm` | ubuntu-22.04-arm (glibc 2.35) | — |
| `whoosh-<v>-<platform>.{tar.gz,zip}`: the command line `whoosh` and `whoosh-daemon` (Windows: also `whoosh-autostart.exe`) | linux-x64, linux-arm64, macos-universal, windows-x64, windows-arm64 | SignPath / Developer ID + notarization |
| `whoosh-nmhost-<v>-<platform>.{tar.gz,zip}` (native messaging host, for packagers) | same | same |
| `whoosh-chromium-<v>-store.zip`, `whoosh-firefox-<v>-store.zip` | ubuntu-24.04 | stores sign on upload |
| `whoosh-chromium-<v>.zip` (GitHub build for Chrome, Brave, Vivaldi: load unpacked; keeps the public key, so the native host accepts it) | ubuntu-24.04 | — |
| `whoosh-<v>-package-manifests.tar.gz` (winget, Scoop, Homebrew cask, Flathub, AUR, filled in with this release's URLs and hashes) | ubuntu-24.04 | covered by the attestation |
| `*.cargo.cdx.json` (one per crate), `whoosh-desktop-<v>.npm.cdx.json`, `whoosh-extension-<v>.npm.cdx.json` | ubuntu-24.04 | covered by the attestation |
| `whoosh-<v>-source.tar.gz` (`git archive` of the tag) | ubuntu-24.04 | covered by the attestation |
| `*.sig`, `latest.json` (Tauri updater) | — | updater key (below) |
| `SHA256SUMS` | ubuntu-24.04 | covered by the attestation |

`whoosh-labserver` (the engine's fault-injecting test server) is not released;
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
2. Set it in all four places, which the release job checks against the tag:
   - `Cargo.toml` → `[workspace.package] version`
   - `apps/desktop/src-tauri/tauri.conf.json` → `version`
   - `apps/desktop/package.json` → `version`
   - `apps/extension/package.json` → `version`

   Run `cargo check` so `Cargo.lock` follows, and commit on `dev` (via a PR).
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
   manifests from `whoosh-<v>-package-manifests.tar.gz` (the *Package
   manifests* job filled in the version, URLs and SHA-256s with
   `.github/scripts/package-manifests.mjs`; `packaging/*/README.md` says how
   to submit each). Nothing is submitted automatically.

### Dry run

*Actions → Release → Run workflow* on any branch builds and signs everything
exactly like a tag would, keeps the files as run artifacts for 7 days and
creates no release. The version comes from `tauri.conf.json`. If the
`release` environment restricts deployments to tags (recommended, below),
a dry run from a branch is rejected at the signing jobs; allow the branch
temporarily or test unsigned in a fork.

## Signing

Each signing path switches itself on only when its secrets exist, so the
pipeline produces unsigned artifacts until then. The switches are evaluated
in the job's `env` (`SIGNPATH_ENABLED`, `APPLE_SIGNING_ENABLED`) and by the
*Decide on updater artifacts* step (`UPDATER_ARTIFACTS`), and shown in the
run log.

| What | How | Where in `release.yml` |
|---|---|---|
| Windows exe, NSIS, MSI, `whoosh.exe`, `whoosh-daemon.exe`, `whoosh-autostart.exe`, `whoosh-nmhost.exe` | [SignPath](https://signpath.io) (free for OSS through the [SignPath Foundation](https://signpath.org)); the unsigned files are uploaded as a short-lived artifact, SignPath signs them and the signed files replace them | `desktop` (Windows): build `--no-bundle` → sign exe → `tauri bundle --no-binary-patching` → sign installers; `binaries` (Windows) |
| macOS `.app` | Tauri CLI with `APPLE_*` variables: Developer ID, hardened runtime, notarization, stapling | `desktop` (macOS) |
| macOS `.dmg` | `codesign` + `notarytool` + `stapler` | `.github/scripts/macos-sign.sh`, `macos-notarize.sh` |
| macOS `whoosh`, `whoosh-daemon`, `whoosh-nmhost` | `codesign` (hardened runtime), zipped and notarized (bare binaries cannot be stapled; Gatekeeper checks online) | `binaries` (macOS) |
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
| `TAURI_SIGNING_PRIVATE_KEY` | secret | updater signing key (minisign); its public half is pinned in `tauri.conf.json` | the key file generated for this repository (see *Updater key*); store the file's contents; keep an offline backup |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | secret | its password | the current key has none: leave the secret unset or empty |

Notarization uses the API key when all three `APPLE_API_*` secrets are set,
otherwise `APPLE_ID` + `APPLE_APP_SPECIFIC_PASSWORD` + `APPLE_TEAM_ID`; with
neither, macOS artifacts are signed but not notarized (warning in the log).

`GITHUB_TOKEN` covers the draft release and the attestation; no personal token
is needed. Not used yet, but will be for automated store/package submissions:
`CHROME_WEBSTORE_*` (client id/secret/refresh token), `AMO_JWT_ISSUER` /
`AMO_JWT_SECRET`, `WINGET_TOKEN`, `HOMEBREW_TAP_TOKEN`, `SNAPCRAFT_STORE_CREDENTIALS`,
`AUR_SSH_KEY`, a GPG key for apt/dnf repositories, `DOCKERHUB_TOKEN`.

### Updater key

The app ships `tauri-plugin-updater` (`apps/desktop/src-tauri/src/updates.rs`)
with the **public** key pinned in `tauri.conf.json` (`plugins.updater.pubkey`)
and the endpoint
`https://github.com/coderzonebd/whoosh-download-manager/releases/latest/download/latest.json`.
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
- **Pre-releases:** `releases/latest` only serves the latest *published,
  non-pre-release* release, so beta builds do not see newer betas through
  the updater; beta testers install each beta by hand. A beta channel would
  need a second endpoint (for example a `latest.json` on a fixed
  `updater-beta` release) and is not built.
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
   `whoosh-firefox-<v>-store.zip` from a release (AMO also asks for the
   source: `whoosh-<v>-source.tar.gz` and `apps/extension/store/amo.md`).
   The add-on ID is already fixed (`whoosh@coderzonebd.com`). When it is
   listed, put its slug in `packaging/browsers/extension-ids.json`
   (`firefox.amoSlug`) so the app links to it.
4. **Microsoft Edge Add-ons**. Register in Microsoft Partner Center
   (<https://partner.microsoft.com/dashboard/microsoftedge/>; free), submit
   `whoosh-chromium-<v>-store.zip`. Once it has an ID, put it in
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
| Flathub, Snapcraft, AUR, Homebrew tap, Scoop bucket | Linux and package-manager channels (`packaging/`) | free |

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
gh attestation verify Whoosh_0.1.0_amd64.AppImage --repo <owner>/<repo>
```

- Windows: file *Properties → Digital Signatures*, or
  `Get-AuthenticodeSignature .\Whoosh_0.1.0_x64-setup.exe` (PowerShell).
- macOS: `spctl --assess --type open --context context:primary-signature -v Whoosh_0.1.0_universal.dmg`
  and, after installing, `spctl --assess -v /Applications/Whoosh.app`
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
  (`whoosh-<v>-package-manifests.tar.gz`); submitting them is by hand.
- Linux package signing and apt/dnf repositories; Snap and Nix packaging;
  MSIX; an MSI for Windows ARM64.
- Signing the sidecars inside the installers: `whoosh-daemon.exe` and
  `whoosh-nmhost.exe` are bundled as built (the signed copies go into the CLI
  and host archives only). SignPath can sign them before bundling the same
  way as the app exe; not wired yet.
- Publishing the Docker image (it is only built in CI).
- A beta update channel (see *Updater key*).
