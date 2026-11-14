// SPDX-License-Identifier: GPL-3.0-or-later

// Store-build policy (MASTER-PLAN §6). The Chrome Web Store and Firefox AMO
// forbid extensions that help download media from YouTube and similar
// streaming sites. The store build therefore never captures, sniffs, offers
// or heals anything on these sites. The GitHub / desktop build (`build`) is
// unchanged: there the list is not applied at all.
//
// `scripts/build.mjs --store` replaces `__WHOOSH_STORE__` with `true`; every
// other build with `false` (test/policy.test.ts checks both bundles).

import storeList from "./store-blocklist.json";
import { hostMatches, hostOf } from "./url";

declare const __WHOOSH_STORE__: boolean | undefined;

/** True in the store build only. */
export const STORE_BUILD: boolean = typeof __WHOOSH_STORE__ === "boolean" ? __WHOOSH_STORE__ : false;

/**
 * Sites the store build stays away from: video platforms whose terms forbid
 * downloading, and their media CDNs. A host matches its subdomains too
 * (`youtube.com` covers `m.youtube.com`). The list lives in
 * `store-blocklist.json` so the manifest script can read it as well; it is a
 * policy decision, documented in `store/README.md`, with no site-specific logic.
 */
export const STORE_BLOCKED_SITES: readonly string[] = Object.freeze([...storeList.sites]);

/** Whether any of `urls` (download, original link, referrer, page…) is on a blocked site. */
export function isPolicyBlocked(urls: readonly (string | undefined)[], blocklist: readonly string[]): boolean {
  if (blocklist.length === 0) return false;
  return urls.some((u) => {
    const host = hostOf(u);
    return host !== undefined && blocklist.some((site) => hostMatches(host, site));
  });
}

/** The list in force for this build: the store list in the store build, nothing otherwise. */
export const ACTIVE_BLOCKLIST: readonly string[] = STORE_BUILD ? STORE_BLOCKED_SITES : [];

/** Whether this build must leave these URLs alone. */
export function blockedByStorePolicy(...urls: (string | undefined)[]): boolean {
  return isPolicyBlocked(urls, ACTIVE_BLOCKLIST);
}
