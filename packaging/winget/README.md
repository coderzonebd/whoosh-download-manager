# winget manifest (template)

Multi-file manifest (schema 1.10) for `microsoft/winget-pkgs`, describing the
NSIS installer (per-user) and the MSI (per-machine) that `release.yml` builds.

## Per release

The simplest path is [wingetcreate](https://github.com/microsoft/winget-create),
which downloads the installers, computes the hashes and opens the PR:

```powershell
wingetcreate update CoderZoneBD.Whoosh --version 0.1.0 `
  --urls https://github.com/coderzonebd/whoosh-download-manager/releases/download/v0.1.0/Whoosh_0.1.0_x64-setup.exe `
         https://github.com/coderzonebd/whoosh-download-manager/releases/download/v0.1.0/Whoosh_0.1.0_x64_en-US.msi `
  --submit
```

For the very first submission use `wingetcreate new` or copy these three files
into `manifests/c/CoderZoneBD/Whoosh/<version>/`, replace the version, URLs,
`ReleaseDate` and `InstallerSha256` (from `SHA256SUMS`), then
`winget validate <dir>` and `winget install --manifest <dir>` in a Windows
Sandbox before opening the PR. Automating this from `release.yml` needs a
fine-grained token with `public_repo` on a fork of winget-pkgs
(`WINGET_TOKEN`, not used yet).

## Placeholders until the name and signing are settled

- `PackageIdentifier` `CoderZoneBD.Whoosh`, `PackageName`, `Moniker` and every
  `github.com/coderzonebd/whoosh-download-manager` URL.
- Zeroed `InstallerSha256` values and version `0.0.1`.
- **Wait for signed installers.** winget-pkgs runs malware scans on every
  submission and users see SmartScreen warnings for unsigned installers;
  submit once SignPath signing is live (docs/release.md).
- ARM64 entries come when `release.yml` builds Windows ARM64.
