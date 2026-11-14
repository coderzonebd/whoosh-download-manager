# Store submission material

Nothing here is submitted automatically; the owner submits from the store accounts.

| File | For |
|---|---|
| [`chrome-web-store.md`](chrome-web-store.md) | Chrome Web Store listing (also covers Edge Add-ons, which accepts the same package) |
| [`amo.md`](amo.md) | Firefox Add-ons listing and reviewer notes |
| [`permissions.md`](permissions.md) | Single purpose, permission justifications, data-use answers |
| [`../PRIVACY.md`](../PRIVACY.md) | Privacy policy (link it from both listings) |
| [`screenshots/`](screenshots/) | 1280×800 screenshots |

## The store build

```bash
corepack pnpm --filter @whoosh/extension build:store            # dist/chromium, dist/firefox
corepack pnpm --filter @whoosh/extension package:store          # plus dist/whoosh-<target>-<version>-store.zip
```

Differences from the GitHub / desktop build (`build`):

- No development `key` in the Chromium manifest (the stores assign the ID).
- **Video-site blocklist** (MASTER-PLAN §6): on the sites in [`src/shared/store-blocklist.json`](../src/shared/store-blocklist.json) — YouTube and its media hosts, and subscription streaming services — the store build does not capture downloads, does not detect media, does not offer context-menu downloads or hover preconnects, does not repair links, and its content script is not injected (`exclude_matches`). A site covers its subdomains. The popup says the extension does not work there.
- Everything else is the same code. `test/policy.test.ts` builds both variants and checks that the store bundle leaves a YouTube download to the browser without contacting the host while the plain bundle offers it to Whoosh, that the store bundle does not list YouTube media, and that only the store manifests exclude the sites.

Changing the list is a policy decision: edit the JSON, rebuild, and mention it in the changelog.

## Screenshots

```bash
cargo build -p whoosh-nmhost -p whoosh-daemon
corepack pnpm --filter @whoosh/extension build
WHOOSH_E2E_CHROME=/path/to/chrome-for-testing node apps/extension/scripts/store-screenshots.mjs
```

The script runs Chrome for Testing with a throwaway profile, the real host and `whoosh-daemon --mock` (demo data, no network), and writes `screenshots/*.png`.
