# Scoop manifest (template)

`vayu.json` installs the portable build (`Vayu_<version>_x64_portable.zip`,
the signed `vayu-desktop.exe` alone) that `release.yml` attaches to each
release, and adds a Start-menu shortcut. `checkver`/`autoupdate` read new
versions from GitHub and the hash from the release's `SHA256SUMS`.

## Where it goes

1. **Own bucket first:** a repository `coderzonebd/scoop-bucket` with this
   file at `bucket/vayu.json`; users run
   `scoop bucket add vayu https://github.com/coderzonebd/scoop-bucket` and
   `scoop install vayu/vayu`. The bucket template's Excavator workflow then
   runs `checkver -u` on a schedule, so no token is needed in this repo.
2. **ScoopInstaller/Extras** later (GUI apps go there, not `main`).

Check locally with `scoop install .\vayu.json` and
`.\bin\checkver.ps1 -Dir . -App vayu -Update` from a bucket checkout.

## Placeholders until the name, portable mode and signing are settled

- App name `vayu`, `vayu-desktop.exe`, shortcut name and every
  `github.com/coderzonebd/vayu` URL; zeroed `hash`, version `0.0.1`.
- No `persist` yet: the app does not have a portable mode (data next to the
  executable, MASTER-PLAN §3.1). Until it does, settings live in
  `%APPDATA%` and survive `scoop update` anyway.
- The native messaging host (`vayu-nmhost`) is not part of this manifest; it
  ships separately in `vayu-nmhost-<version>-windows-x64.zip`.
