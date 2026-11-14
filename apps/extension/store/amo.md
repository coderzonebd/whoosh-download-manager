# Firefox Add-ons (AMO) listing

Upload `dist/whoosh-firefox-<version>-store.zip` from `pnpm --filter @whoosh/extension package:store`. The add-on ID is fixed in the manifest (`whoosh@coderzonebd.com`); `whoosh-nmhost --register` already allows it. Minimum Firefox: 140.

## Listing

- **Name:** Whoosh
- **Add-on URL slug:** `whoosh-download-manager`
- **Summary** (≤ 250 characters):
  > Sends Firefox downloads to the Whoosh download manager app with the same cookies and referrer: multi-connection, resumable downloads that survive restarts, and expired links repaired from the page they came from. Needs the Whoosh app.
- **Categories:** Download Management
- **Tags:** download manager, downloads, accelerator, resume
- **Support site:** https://github.com/coderzonebd/whoosh-download-manager
- **Support email:** (the owner's support address)
- **License:** GNU General Public License v3.0 or later
- **Privacy policy:** the text of [`PRIVACY.md`](../PRIVACY.md)

## Description

Same text as the Chrome Web Store description ([`chrome-web-store.md`](chrome-web-store.md)), with "Hold Alt" unchanged (Firefox on macOS: Option).

## Screenshots

[`screenshots/`](screenshots/) (taken in Chrome for Testing; the popup and options page are the same code in Firefox).

## Notes for reviewers

> The extension forwards downloads to a desktop app through native messaging (`com.coderzonebd.whoosh`, host `whoosh-nmhost` from the same repository). It has no network traffic of its own and no remote code.
>
> The JavaScript is bundled from TypeScript, not minified. Source and build:
> - Source: https://github.com/coderzonebd/whoosh-download-manager (directory `apps/extension`), or the attached source archive.
> - Requirements: Node.js 24 or newer, pnpm (via corepack).
> - Build: `corepack pnpm install --frozen-lockfile`, then `corepack pnpm --filter @whoosh/extension build:store`. The add-on is `apps/extension/dist/firefox/`. The output is byte-for-byte the uploaded package (`package:store` writes the same files as a deterministic zip).
>
> Permissions are explained in `apps/extension/store/permissions.md`. `data_collection_permissions` is `none`: nothing is sent to us or third parties; download addresses and their cookies go only to the user's own Whoosh app.
>
> To try it without the app, the popup shows "Not connected" and downloads stay in Firefox. With the app: install Whoosh, run `whoosh-nmhost --register`, start any download of a listed type.

AMO requires the source code for bundled add-ons: upload a source archive of the repository at the release tag (`git archive`) with the submission.
