# Packaging

Manifests and templates for the distribution channels in MASTER-PLAN §4.1 and
§7. The installers themselves (NSIS, MSI, dmg, AppImage, deb, rpm) are built
by [`release.yml`](../.github/workflows/release.yml); everything here consumes
those release assets. How a release is cut and signed: [docs/release.md](../docs/release.md).

| Directory | Channel | State |
|---|---|---|
| [`flatpak/`](flatpak) | Flathub | manifest repackaging the release `.deb`; Flathub will want a from-source build |
| [`aur/`](aur) | Arch User Repository (`whoosh`) | from-source `PKGBUILD` |
| [`winget/`](winget) | `microsoft/winget-pkgs` | multi-file manifest template |
| [`homebrew/`](homebrew) | own tap, later `homebrew/cask` | cask template |
| [`scoop/`](scoop) | own bucket, later Extras | manifest with `checkver`/`autoupdate` |
| [`docker/`](docker) | GHCR / Docker Hub | multi-stage, non-root; builds `whoosh-labserver` until the daemon exists |
| [`linux/`](linux) | shared | `.desktop` entry and AppStream metainfo (both validate) |
| [`browsers/`](browsers) | every installer | the extension's store IDs (`extension-ids.json`) and the generator for the installers' browser integration |
| [`signpath/`](signpath) | Windows signing | SignPath artifact configuration used by `release.yml` |
| [`media-tools/`](media-tools) | every desktop installer | licence texts and notice shipped next to the bundled yt-dlp and ffmpeg |

## Filled in per release

`release.yml` (job *Package manifests*) runs
[`.github/scripts/package-manifests.mjs`](../.github/scripts/package-manifests.mjs)
on the release assets and attaches `whoosh-<version>-package-manifests.tar.gz`
to the draft release: the winget, Scoop, Homebrew, Flatpak and AUR files from
here with the real version, URLs and SHA-256s (and an AUR `.SRCINFO`).
Nothing is submitted; each channel's README says how to submit by hand.

## Placeholders everywhere

The product name is **Whoosh Download Manager** (**WDM** where space is
short; never "Whoosh" alone): `productName` in `tauri.conf.json`, the
desktop entry, the metainfo, the cask, Scoop and winget names. Release asset
names use hyphens (`Whoosh-Download-Manager_<version>_…`). Until the GitHub
organisation and signing are settled, these are placeholders in every file
here:

- the command and package name `whoosh`, the app id **`com.coderzonebd.whoosh`** (also in
  `tauri.conf.json` and the native messaging host name), winget id
  `CoderZoneBD.Whoosh`;
- the repository URL **`github.com/coderzonebd/whoosh-download-manager`**;
- versions (`0.0.1`) and zeroed hashes in the templates here (the release's
  generated copies have the real ones).

Renaming later means: `tauri.conf.json` (`productName`, `identifier`), the
file names in `linux/` and `flatpak/`, every manifest here, and the
`HOST_NAME` the extension and `whoosh-nmhost` agree on.

Not here yet: Snap, Nix, Chocolatey, MSIX, apt/dnf repositories, and
Unraid/Synology templates for the daemon image.
