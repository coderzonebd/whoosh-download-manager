# AUR package `vayu` (template)

`PKGBUILD` builds from the source archive that `release.yml` attaches to
every release (`vayu-<version>-source.tar.gz`, a `git archive` of the tag),
following the Arch [Rust](https://wiki.archlinux.org/title/Rust_package_guidelines)
and [Node.js](https://wiki.archlinux.org/title/Node.js_package_guidelines)
package guidelines: dependencies are fetched in `prepare()`, `build()` runs
offline (`--frozen`), Arch's stable Rust is used (`RUSTUP_TOOLCHAIN=stable`)
instead of the toolchain pinned in `rust-toolchain.toml`.

It installs `vayu-desktop`, `vayu-nmhost`, the desktop entry, AppStream
metainfo and icons from [`../linux`](../linux), and system-wide native
messaging manifests for Firefox, Chromium and Google Chrome.

## Per release

```bash
# in a clone of ssh://aur@aur.archlinux.org/vayu.git
cp /path/to/repo/packaging/aur/PKGBUILD .
updpkgsums                     # or paste the hash from SHA256SUMS
makepkg -sci                   # build, check, install in a clean chroot ideally (extra-x86_64-build)
namcap PKGBUILD ./*.pkg.tar.zst
makepkg --printsrcinfo > .SRCINFO
git commit -am "Update to <version>" && git push
```

A binary package (`vayu-bin`, repackaging the release `.deb`) can follow if
build times on user machines become a complaint. Pushing from CI would need an
`AUR_SSH_KEY` secret (not used yet).

## Placeholders until the name and the repository are settled

- `pkgname`, maintainer line, `url`, the `com.coderzonebd.vayu` file names,
  `pkgver` and `sha256sums=('SKIP')` (must be a real hash on the AUR).
- `depends` is hand-written from what Tauri 2 links on Linux; run `namcap`
  on a real build to confirm it.
- The native messaging manifests allow only the Firefox add-on id until the
  Chrome Web Store id exists (`--extension-id`).
- Not built on an Arch machine yet: no `makepkg` run has been done.
