import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

// @ts-expect-error -- plain ESM build script without type declarations
import { DEV_EXTENSION_ID, DEV_PUBLIC_KEY, GECKO_ID, manifestFor } from "../scripts/manifest.mjs";

const root = join(import.meta.dirname, "..");

/** Chromium derives an extension ID from its public key like this. */
function extensionIdFromKey(base64Key: string): string {
  const hash = createHash("sha256").update(Buffer.from(base64Key, "base64")).digest("hex").slice(0, 32);
  return [...hash].map((c) => String.fromCharCode("a".charCodeAt(0) + parseInt(c, 16))).join("");
}

describe("manifest", () => {
  it("pins the development ID with the public key, and nmhost allows that ID", () => {
    expect(extensionIdFromKey(DEV_PUBLIC_KEY)).toBe(DEV_EXTENSION_ID);
    const nmhost = readFileSync(join(root, "../../crates/nmhost/src/lib.rs"), "utf8");
    expect(nmhost).toContain(`DEV_CHROMIUM_EXTENSION_ID: &str = "${DEV_EXTENSION_ID}"`);
    expect(nmhost).toContain(`FIREFOX_EXTENSION_ID: &str = "${GECKO_ID}"`);
  });

  it("builds a Chromium manifest with a service worker and the dev key", () => {
    const m = manifestFor("chromium", { version: "1.2.3", store: false });
    expect(m.manifest_version).toBe(3);
    expect(m.background).toEqual({ service_worker: "background.js" });
    expect(m.key).toBe(DEV_PUBLIC_KEY);
    expect(m.browser_specific_settings).toBeUndefined();
    expect(m.permissions).toContain("nativeMessaging");
  });

  it("leaves the key out of store builds", () => {
    expect(manifestFor("chromium", { version: "1.2.3", store: true }).key).toBeUndefined();
  });

  it("builds a Firefox manifest with background scripts and the gecko ID", () => {
    const m = manifestFor("firefox", { version: "1.2.3", store: false });
    expect(m.background).toEqual({ scripts: ["background.js"] });
    expect(m.browser_specific_settings.gecko.id).toBe("whoosh@coderzonebd.com");
    expect(m.key).toBeUndefined();
  });

  it("never asks for more than it uses", () => {
    const m = manifestFor("chromium", { version: "1.2.3", store: true });
    expect([...m.permissions].sort()).toEqual(
      ["contextMenus", "cookies", "downloads", "nativeMessaging", "scripting", "storage", "webRequest"].sort(),
    );
    expect(m.host_permissions).toEqual(["http://*/*", "https://*/*"]);
  });

  it("has an options page in both browsers", () => {
    for (const target of ["chromium", "firefox"] as const) {
      expect(manifestFor(target, { version: "1.2.3", store: false }).options_ui).toEqual({ page: "options.html", open_in_tab: true });
    }
  });

  it("keeps the content script off blocked sites in store builds only", () => {
    const store = manifestFor("chromium", { version: "1.2.3", store: true });
    expect(store.content_scripts[0].exclude_matches).toContain("*://*.youtube.com/*");
    expect(manifestFor("chromium", { version: "1.2.3", store: false }).content_scripts[0].exclude_matches).toBeUndefined();
  });
});
