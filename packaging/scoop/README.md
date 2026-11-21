# Scoop manifest (template)

`whoosh-download-manager.json` installs the portable build (`Whoosh-Download-Manager_<version>_x64_portable.zip`,
the signed `wdm-desktop.exe` alone) that `release.yml` attaches to each
release, and adds a Start-menu shortcut. `checkver`/`autoupdate` read new
versions from GitHub and the hash from the release's `SHA256SUMS`.

The release run keeps `wdm-<version>-package-manifests.tar.gz` (Actions → *Release* → the run for the tag → artifact `release-internal`; not on the release page); its `scoop/`
folder holds this manifest with the release's version, URLs and SHA-256s
filled in (`.github/scripts/package-manifests.mjs`). Start from that copy.

## Where it goes

1. **Own bucket first:** a repository `coderzonebd/scoop-bucket` with this
   file at `bucket/whoosh-download-manager.json`; users run
   `scoop bucket add wdm https://github.com/coderzonebd/scoop-bucket` and
   `scoop install wdm/whoosh-download-manager`. The bucket template's Excavator workflow then
   runs `checkver -u` on a schedule, so no token is needed in this repo.
2. **ScoopInstaller/Extras** later (GUI apps go there, not `main`).

Check locally with `scoop install .\whoosh-download-manager.json` and
`.\bin\checkver.ps1 -Dir . -App wdm -Update` from a bucket checkout.

## Placeholders until the name, portable mode and signing are settled

- App name `wdm`, `wdm-desktop.exe`, shortcut name and every
  `github.com/coderzonebd/whoosh-download-manager` URL; zeroed `hash`, version `0.0.1`.
- No `persist` yet: the app does not have a portable mode (data next to the
  executable, MASTER-PLAN §3.1). Until it does, settings live in
  `%APPDATA%` and survive `scoop update` anyway.
- The native messaging host (`wdm-nmhost`) is not part of this manifest; it
  ships in the command-line archive `wdm-<version>-windows-x64.zip`.
