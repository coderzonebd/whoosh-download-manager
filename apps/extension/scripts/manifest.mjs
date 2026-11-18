// SPDX-License-Identifier: GPL-3.0-or-later
// The extension manifest, generated per browser from one description.

import { readFileSync } from "node:fs";

/**
 * Public half of the development signing key. It pins the unpacked Chromium
 * extension to ID clhjgofaikdmabdnebgcmdffdjenagfl, which `wdm-nmhost
 * --register` allows. The private key is not in the repository (it is not
 * needed to load an unpacked extension). Store builds omit the key; the
 * stores assign their own IDs.
 */
export const DEV_PUBLIC_KEY =
  "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAxQcv1gIjAJjOk8qNpf3bxBfbXMBTABSU9ErgQ3yxkLb2Y6CJF4OT4TJ7keqMuN3rWLbGcBO2kmlIJtwJiNjwQg5gUKVaxu75M4TZQrflPQktgxojl5kIcfC5c8qaR9WJd2Wm1OYrSZxYJYfYXOXjGNgo2U3Z5guXt4qXJbJqaEKwLDaScAe0HuoKXAQHGrGVlyohOkPtOfw34cHqOI0BC62fyDOCL4abUfeOTt7qKZrK0FaxNQeLqnOgUQ7jImU6sFk9nStcGgaou0UvJf/cYkC8NITvNIkTBtOB//2v5bRXCF/X9gnELPOIYFklGsBixBGhZIOwZa9s/nV1KuvtzwIDAQAB";
export const DEV_EXTENSION_ID = "clhjgofaikdmabdnebgcmdffdjenagfl";
export const GECKO_ID = "whoosh@coderzonebd.com";

const HTTP = ["http://*/*", "https://*/*"];

/** Sites the store build stays away from (shared with src/shared/policy.ts). */
export const STORE_BLOCKED_SITES = JSON.parse(
  readFileSync(new URL("../src/shared/store-blocklist.json", import.meta.url), "utf8"),
).sites;

/**
 * `exclude_matches` patterns for a list of sites. In a match pattern,
 * `*.example.com` covers `example.com` itself and every subdomain.
 */
export function excludeMatchPatterns(sites) {
  return sites.map((site) => `*://*.${site}/*`);
}

/**
 * The browsers' version for an app version. Extension versions are one to four numbers
 * (0–65535) that must grow with every upload, so a pre-release `X.Y.Z-beta.N` cannot be used
 * as it is: it becomes a version just below `X.Y.Z` (`1.0.0-beta.2` → `0.65535.65535.2`,
 * `1.2.3-beta.2` → `1.2.2.2`), which still grows from beta to beta and is passed by the
 * release. The full version is kept in `version_name` (shown by Chromium).
 * @param {string} version
 * @returns {{ version: string, versionName?: string }}
 */
export function browserVersion(version) {
  const m = /^(\d+)\.(\d+)\.(\d+)(?:-(.+))?$/.exec(version);
  if (!m) throw new Error(`not a release version: ${version}`);
  const [x, y, z] = [m[1], m[2], m[3]].map(Number);
  if (!m[4]) return { version: `${x}.${y}.${z}` };
  const n = Number(/(\d+)$/.exec(m[4])?.[1] ?? 0);
  const max = 65535;
  const below = z > 0 ? [x, y, z - 1] : y > 0 ? [x, y - 1, max] : x > 0 ? [x - 1, max, max] : null;
  if (!below) throw new Error(`no browser version below ${version}`);
  return { version: `${below.join(".")}.${Math.min(n, max)}`, versionName: version };
}

/**
 * @param {"chromium" | "firefox"} target
 * @param {{ version: string, store: boolean }} options
 *   `store`: the Chrome Web Store / AMO build — no development `key`, and the
 *   video-site blocklist applied (MASTER-PLAN §6).
 */
export function manifestFor(target, { version, store }) {
  const icons = { 16: "icons/16.png", 32: "icons/32.png", 48: "icons/48.png", 128: "icons/128.png" };
  const manifest = {
    manifest_version: 3,
    // Translated: _locales/<lang>/messages.json (extName, extDescription).
    name: "__MSG_extName__",
    short_name: "__MSG_extShortName__",
    ...(() => {
      const v = browserVersion(version);
      // Firefox has no version_name; Chromium shows it instead of the numbers.
      return target === "chromium" && v.versionName ? { version: v.version, version_name: v.versionName } : { version: v.version };
    })(),
    description: "__MSG_extDescription__",
    default_locale: "en",
    icons,
    action: {
      default_title: "__MSG_extName__",
      default_popup: "popup.html",
      default_icon: { 16: icons[16], 32: icons[32] },
    },
    permissions: [
      "contextMenus",
      "cookies",
      "downloads",
      "nativeMessaging",
      "scripting",
      "storage",
      "webRequest",
    ],
    host_permissions: HTTP,
    options_ui: { page: "options.html", open_in_tab: true },
    content_scripts: [
      {
        matches: HTTP,
        // The store build does not even run its content script (hover
        // preconnect, Alt+click, link collection) on blocked sites.
        ...(store ? { exclude_matches: excludeMatchPatterns(STORE_BLOCKED_SITES) } : {}),
        js: ["content.js"],
        run_at: "document_start",
        all_frames: true,
      },
    ],
  };

  if (target === "chromium") {
    return {
      ...manifest,
      minimum_chrome_version: "116",
      background: { service_worker: "background.js" },
      ...(store ? {} : { key: DEV_PUBLIC_KEY }),
    };
  }
  return {
    ...manifest,
    background: { scripts: ["background.js"] },
    browser_specific_settings: {
      gecko: {
        id: GECKO_ID,
        strict_min_version: "140.0",
        // Nothing is sent to us or to third parties; data goes only to the
        // user's own wdm app through native messaging.
        data_collection_permissions: { required: ["none"] },
      },
      // Firefox for Android reads data_collection_permissions from 142 on.
      gecko_android: { strict_min_version: "142.0" },
    },
  };
}
