# Homebrew cask (template)

`whoosh-download-manager.rb` installs the universal `.dmg` that `release.yml` attaches to each
GitHub release.

## Where it goes

1. **Own tap first:** a repository `coderzonebd/homebrew-tap` with this file
   at `Casks/whoosh-download-manager.rb`; users run `brew install --cask coderzonebd/tap/whoosh-download-manager`.
2. **homebrew/cask later:** it only accepts apps that are signed with a
   Developer ID and notarized (Gatekeeper must pass), with a stable download
   URL and enough users ("notable"). Submit once releases are notarized.

## Per release

Each release carries `wdm-<version>-package-manifests.tar.gz`; its `homebrew/`
folder holds this manifest with the release's version, URLs and SHA-256s
filled in (`.github/scripts/package-manifests.mjs`). Start from that copy.


```bash
shasum -a 256 Whoosh-Download-Manager_0.1.0_universal.dmg            # or take it from SHA256SUMS
brew bump-cask-pr --version 0.1.0 wdm            # in the tap or homebrew/cask
brew audit --cask --new whoosh-download-manager && brew style whoosh-download-manager.rb
```

Automating the bump from `release.yml` needs a token that can push to the tap
(`HOMEBREW_TAP_TOKEN`, not used yet).

## Placeholders until the name and signing are settled

- Token `wdm`, `name "Whoosh Download Manager"`, `Whoosh Download Manager.app`, the bundle id in `zap` paths
  (`com.coderzonebd.whoosh`) and every `github.com/coderzonebd/whoosh-download-manager` URL.
- Zeroed `sha256` and version `0.0.1`.
- `depends_on macos: ">= :monterey"` follows `bundle.macOS.minimumSystemVersion`
  (12.0) in `tauri.conf.json`; keep them in step.
- `zap` paths are what Tauri/WebKit normally create; check them against a
  real install before submitting.
