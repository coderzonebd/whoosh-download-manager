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
| [`signpath/`](signpath) | Windows signing | SignPath artifact configuration used by `release.yml` |

## Placeholders everywhere

Until the product name, the GitHub organisation and signing are settled,
these are placeholders in every file here:

- the name **Whoosh** / `whoosh`, the app id **`com.coderzonebd.whoosh`** (also in
  `tauri.conf.json` and the native messaging host name), winget id
  `CoderZoneBD.Whoosh`;
- the repository URL **`github.com/coderzonebd/whoosh`**;
- versions (`0.0.1`) and zeroed hashes.

Renaming later means: `tauri.conf.json` (`productName`, `identifier`), the
file names in `linux/` and `flatpak/`, every manifest here, and the
`HOST_NAME` the extension and `whoosh-nmhost` agree on.

Not here yet: Snap, Nix, Chocolatey, MSIX, apt/dnf repositories, and
Unraid/Synology templates for the daemon image.
