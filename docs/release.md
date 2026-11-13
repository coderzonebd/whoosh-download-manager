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
| `Whoosh_<v>_x64-setup.exe` (NSIS, per-user) | windows-2025 | SignPath (exe inside and installer) |
| `Whoosh_<v>_x64_en-US.msi` (WiX, per-machine; not for pre-releases) | windows-2025 | SignPath |
| `Whoosh_<v>_x64_portable.zip` (the exe alone) | windows-2025 | SignPath (the exe) |
| `Whoosh_<v>_universal.dmg` (Apple silicon + Intel) | macos-15 | Developer ID + notarization, stapled |
| `Whoosh_<v>_universal.app.tar.gz` (updater archive) | macos-15 | only with the updater key and public key (below) |
| `Whoosh_<v>_amd64.AppImage`, `Whoosh_<v>_amd64.deb`, `Whoosh-<v>-1.x86_64.rpm` | ubuntu-22.04 (glibc 2.35) | — (see *Not automated yet*) |
| `whoosh-nmhost-<v>-<platform>.{tar.gz,zip}` | linux-x64, linux-arm64, macos-universal, windows-x64 | SignPath / Developer ID + notarization |
| `whoosh-labserver-<v>-<platform>.{tar.gz,zip}` | same | same |
| `whoosh-chromium-<v>-store.zip`, `whoosh-firefox-<v>-store.zip` | ubuntu-24.04 | stores sign on upload |
| `*.cargo.cdx.json` (one per crate), `whoosh-desktop-<v>.npm.cdx.json`, `whoosh-extension-<v>.npm.cdx.json` | ubuntu-24.04 | covered by the attestation |
| `whoosh-<v>-source.tar.gz` (`git archive` of the tag) | ubuntu-24.04 | covered by the attestation |
| `*.sig`, `latest.json` (Tauri updater) | — | only with the updater key and public key (below) |
| `SHA256SUMS` | ubuntu-24.04 | covered by the attestation |

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
6. Afterwards: upload the store zips, update the package-manager manifests
   (`packaging/*/README.md` says how for each).

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
| Windows exe, NSIS, MSI, `whoosh-nmhost.exe`, `whoosh-labserver.exe` | [SignPath](https://signpath.io) (free for OSS through the [SignPath Foundation](https://signpath.org)); the unsigned files are uploaded as a short-lived artifact, SignPath signs them and the signed files replace them | `desktop` (Windows): build `--no-bundle` → sign exe → `tauri bundle --no-binary-patching` → sign installers; `binaries` (Windows) |
| macOS `.app` | Tauri CLI with `APPLE_*` variables: Developer ID, hardened runtime, notarization, stapling | `desktop` (macOS) |
| macOS `.dmg` | `codesign` + `notarytool` + `stapler` | `.github/scripts/macos-sign.sh`, `macos-notarize.sh` |
| macOS `whoosh-nmhost`, `whoosh-labserver` | `codesign` (hardened runtime), zipped and notarized (bare binaries cannot be stapled; Gatekeeper checks online) | `binaries` (macOS) |
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
| `TAURI_SIGNING_PRIVATE_KEY` | secret | updater signing key (minisign) | `pnpm --filter @whoosh/desktop tauri signer generate -w whoosh-updater.key`; store the file's contents; keep an offline backup |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | secret | its password (may be empty) | chosen at generation |

Notarization uses the API key when all three `APPLE_API_*` secrets are set,
otherwise `APPLE_ID` + `APPLE_APP_SPECIFIC_PASSWORD` + `APPLE_TEAM_ID`; with
neither, macOS artifacts are signed but not notarized (warning in the log).

`GITHUB_TOKEN` covers the draft release and the attestation; no personal token
is needed. Not used yet, but will be for automated store/package submissions:
`CHROME_WEBSTORE_*` (client id/secret/refresh token), `AMO_JWT_ISSUER` /
`AMO_JWT_SECRET`, `WINGET_TOKEN`, `HOMEBREW_TAP_TOKEN`, `SNAPCRAFT_STORE_CREDENTIALS`,
`AUR_SSH_KEY`, a GPG key for apt/dnf repositories, `DOCKERHUB_TOKEN`.

### Updater public key

Updater artifacts are produced only when **both** the private key secret and
`plugins.updater.pubkey` in `tauri.conf.json` exist: the Tauri CLI refuses
`createUpdaterArtifacts` without an updater section (seen in a local build),
and signatures are pointless before the app can check them. So the key does
nothing until the app ships `tauri-plugin-updater` (not wired in yet). Then
the **public** key (`whoosh-updater.key.pub`) goes into
`plugins.updater.pubkey` in `tauri.conf.json`, with the endpoint
`https://github.com/<owner>/<repo>/releases/latest/download/latest.json`
(served only for the latest *published, non-pre-release* release). Losing the
private key means existing installs can never auto-update again: back it up
offline.

## Accounts to create

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
  `dev` the default branch and applies `.github/rulesets/*.json`: `dev` needs a
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

- Store uploads (Chrome Web Store, AMO, Microsoft Store) and package-manager
  submissions (winget, Homebrew, Scoop, Flathub, AUR, Snap, Nix, Chocolatey);
  templates and per-channel steps are in `packaging/`.
- Linux package signing and apt/dnf repositories; Snap and Nix packaging.
- Desktop builds for Linux ARM64 and Windows ARM64 (the CLI binaries already
  include `linux-arm64`); MSIX.
- Publishing the Docker image (it is only built in CI).
- The in-app updater itself (`tauri-plugin-updater`, public key, endpoint).
- Registering `whoosh-nmhost` with browsers from the installers (`--register
  --no-dev-id` plus the store extension ids).
