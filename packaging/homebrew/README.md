# Homebrew cask (template)

`vayu.rb` installs the universal `.dmg` that `release.yml` attaches to each
GitHub release.

## Where it goes

1. **Own tap first:** a repository `coderzonebd/homebrew-tap` with this file
   at `Casks/vayu.rb`; users run `brew install --cask coderzonebd/tap/vayu`.
2. **homebrew/cask later:** it only accepts apps that are signed with a
   Developer ID and notarized (Gatekeeper must pass), with a stable download
   URL and enough users ("notable"). Submit once releases are notarized.

## Per release

```bash
shasum -a 256 Vayu_0.1.0_universal.dmg            # or take it from SHA256SUMS
brew bump-cask-pr --version 0.1.0 vayu            # in the tap or homebrew/cask
brew audit --cask --new vayu && brew style vayu.rb
```

Automating the bump from `release.yml` needs a token that can push to the tap
(`HOMEBREW_TAP_TOKEN`, not used yet).

## Placeholders until the name and signing are settled

- Token `vayu`, `name "Vayu"`, `Vayu.app`, the bundle id in `zap` paths
  (`com.coderzonebd.vayu`) and every `github.com/coderzonebd/vayu` URL.
- Zeroed `sha256` and version `0.0.1`.
- `depends_on macos: ">= :monterey"` follows `bundle.macOS.minimumSystemVersion`
  (12.0) in `tauri.conf.json`; keep them in step.
- `zap` paths are what Tauri/WebKit normally create; check them against a
  real install before submitting.
