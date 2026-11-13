# Flatpak (template)

`com.coderzonebd.whoosh.yml` repackages the `.deb` that `release.yml` attaches
to each release, on the GNOME runtime (which provides WebKitGTK and GTK 3),
plus libayatana-appindicator for the tray icon. Desktop entry and AppStream
metainfo come from [`../linux`](../linux) (both pass `desktop-file-validate`
and `appstreamcli validate`).

## Try it locally

```bash
flatpak install --user flathub org.gnome.Platform//50 org.gnome.Sdk//50 org.flatpak.Builder
cd packaging/flatpak
git clone --depth 1 https://github.com/flathub/shared-modules.git
# Point the .deb source at a local build instead of a release while testing:
#   - type: file
#     path: ../../target/x86_64-unknown-linux-gnu/release/bundle/deb/Whoosh_0.0.1_amd64.deb
#     dest-filename: whoosh.deb
flatpak run org.flatpak.Builder --user --install --force-clean build-dir com.coderzonebd.whoosh.yml
flatpak run com.coderzonebd.whoosh
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest com.coderzonebd.whoosh.yml
```

## Before submitting to Flathub

- **Build from source.** Flathub expects open-source apps to be built from
  source in the sandbox, offline: generate `cargo-sources.json` with
  `flatpak-cargo-generator.py Cargo.lock` and the pnpm/npm sources with
  `flatpak-node-generator` (pnpm support there is limited; a fallback is to
  vendor the built frontend as a release asset), then replace the `.deb`
  module with a `cargo build --release --offline -p whoosh-desktop` module.
- **Runtime:** `org.gnome.Platform//50` was chosen without checking that it
  still ships `libwebkit2gtk-4.1.so` (GTK 3). Check with
  `flatpak run --command=sh org.gnome.Platform//50 -c 'ls /usr/lib/*/libwebkit2gtk-4.1*'`
  and move to the newest runtime that has it.
- **Metainfo:** Flathub's linter wants screenshots, a release list that
  matches tags, and branding colours; the name and URLs are placeholders.
- **Native messaging:** a sandboxed app cannot be started by a browser's
  native messaging host directly; `whoosh-nmhost` stays a host-side install
  (see `crates/nmhost/README.md`) until a portal-based design exists.
- **Permissions:** network, Wayland/X11, GPU, the Downloads folder and the
  tray name. Anything wider (whole home, other folders) should go through the
  file chooser portal instead.

## Placeholders until the name and signing are settled

- App id `com.coderzonebd.whoosh` (also the `identifier` in `tauri.conf.json`),
  command `whoosh-desktop`, every `github.com/coderzonebd/whoosh-download-manager` URL.
- The `.deb` URL and zeroed `sha256`; x86_64 only.
- The icon paths inside the `.deb` follow what Tauri 2 produced in a local
  build on 2026-11-10; recheck them if the bundler changes.
