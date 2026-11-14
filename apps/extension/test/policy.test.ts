import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runInNewContext } from "node:vm";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { evaluateCapture } from "../src/background/capture/rules";
import { isPolicyBlocked, STORE_BLOCKED_SITES, STORE_BUILD, blockedByStorePolicy } from "../src/shared/policy";
import { DEFAULT_SETTINGS } from "../src/shared/settings";

// @ts-expect-error -- plain ESM build script without type declarations
import { buildExtension } from "../scripts/build.mjs";

describe("store policy (unit)", () => {
  it("matches the blocked sites and their subdomains, nothing else", () => {
    const blocked = (url: string) => isPolicyBlocked([url], STORE_BLOCKED_SITES);
    expect(blocked("https://www.youtube.com/watch?v=x")).toBe(true);
    expect(blocked("https://m.youtube.com/")).toBe(true);
    expect(blocked("https://youtu.be/x")).toBe(true);
    expect(blocked("https://rr3---sn-abc.googlevideo.com/videoplayback?x=1")).toBe(true);
    expect(blocked("https://www.netflix.com/title/1")).toBe(true);
    expect(blocked("https://notyoutube.com/a.zip")).toBe(false);
    expect(blocked("https://youtube.com.example.org/a.zip")).toBe(false);
    expect(blocked("https://example.com/video.mp4")).toBe(false);
    expect(isPolicyBlocked([undefined, "https://example.com/", "https://youtube.com/"], STORE_BLOCKED_SITES)).toBe(true);
    expect(isPolicyBlocked(["https://youtube.com/"], [])).toBe(false);
  });

  it("is off outside the store build (tests run the plain build)", () => {
    expect(STORE_BUILD).toBe(false);
    expect(blockedByStorePolicy("https://www.youtube.com/watch?v=x")).toBe(false);
  });

  it("keeps the store skip reason ahead of the user's choices", () => {
    const candidate = { id: 1, url: "https://rr1.googlevideo.com/v.mp4", incognito: false, state: "in_progress" };
    const decision = evaluateCapture(candidate, DEFAULT_SETTINGS, { bypass: false, nonGetRequest: false, force: true, blocked: true });
    expect(decision).toEqual({ capture: false, reason: "store-policy" });
  });
});

// ------------------------------------------------------------------ the real bundles

type Listener = (...args: unknown[]) => unknown;

/**
 * Runs a built background.js against a minimal fake WebExtension API and
 * reports what it did: which listeners it registered and whether it ever
 * opened the native port.
 */
function loadBackground(file: string) {
  const listeners = new Map<string, Listener[]>();
  const native: string[] = [];
  const event = (name: string) => ({
    addListener: (fn: Listener) => listeners.set(name, [...(listeners.get(name) ?? []), fn]),
    removeListener: () => {},
  });
  const storage = {
    get: async () => ({}),
    set: async () => {},
  };
  // The fake host answers nm.status ("daemon not running") and refuses the rest.
  const port = {
    postMessage: (message: { id?: number; method?: string }) => {
      setTimeout(() => {
        const reply =
          message.method === "nm.status"
            ? { jsonrpc: "2.0", id: message.id, result: { daemonRunning: false, hostVersion: "test" } }
            : { jsonrpc: "2.0", id: message.id, error: { code: -32007, message: "not running" } };
        for (const fn of listeners.get("port.onMessage") ?? []) fn(reply);
      }, 0);
    },
    disconnect: () => {},
    onMessage: event("port.onMessage"),
    onDisconnect: event("port.onDisconnect"),
  };
  const chrome = {
    runtime: {
      id: "test",
      getURL: (p: string) => `chrome-extension://test/${p}`,
      connectNative: (name: string) => {
        native.push(name);
        return port;
      },
      onInstalled: event("runtime.onInstalled"),
      onStartup: event("runtime.onStartup"),
      onMessage: event("runtime.onMessage"),
      openOptionsPage: async () => {},
    },
    storage: { local: storage, session: storage, onChanged: event("storage.onChanged") },
    action: { setBadgeText: async () => {}, setBadgeBackgroundColor: async () => {} },
    downloads: {
      onDeterminingFilename: event("downloads.onDeterminingFilename"),
      onCreated: event("downloads.onCreated"),
      search: async () => [],
      cancel: async () => {},
      erase: async () => [],
      download: async () => 1,
    },
    cookies: { getAll: async () => [] },
    webRequest: { onBeforeRequest: event("webRequest.onBeforeRequest"), onHeadersReceived: event("webRequest.onHeadersReceived") },
    contextMenus: { onClicked: event("contextMenus.onClicked"), removeAll: async () => {}, create: () => {} },
    tabs: { onRemoved: event("tabs.onRemoved"), onUpdated: event("tabs.onUpdated"), query: async () => [], sendMessage: async () => {} },
    scripting: { executeScript: async () => [] },
  };
  runInNewContext(readFileSync(file, "utf8"), {
    chrome,
    navigator: { userAgent: "test" },
    console: { debug: () => {}, warn: () => {}, error: () => {}, log: () => {} },
    setTimeout,
    clearTimeout,
    URL,
    structuredClone,
  });
  const listener = (name: string) => {
    const fn = listeners.get(name)?.[0];
    if (!fn) throw new Error(`no ${name} listener`);
    return fn;
  };
  return { listener, native };
}

/** Starts a YouTube-hosted download in the bundle; resolves to whether the native host was contacted. */
async function offerVideoDownload(file: string): Promise<{ released: boolean; contactedHost: boolean }> {
  const bg = loadBackground(file);
  let released = false;
  bg.listener("downloads.onDeterminingFilename")(
    {
      id: 1,
      url: "https://rr5---sn-x.googlevideo.com/videoplayback/clip.mp4",
      finalUrl: "https://rr5---sn-x.googlevideo.com/videoplayback/clip.mp4",
      referrer: "https://www.youtube.com/watch?v=abc",
      filename: "clip.mp4",
      mime: "video/mp4",
      totalBytes: 10_000_000,
      fileSize: 0,
      incognito: false,
      state: "in_progress",
      danger: "safe",
    },
    () => {
      released = true;
    },
  );
  await new Promise((r) => setTimeout(r, 50));
  return { released, contactedHost: bg.native.length > 0 };
}

describe("store build (built bundles)", () => {
  const out = mkdtempSync(join(tmpdir(), "whoosh-policy-"));
  const storeDir = join(out, "store");
  const devDir = join(out, "dev");

  beforeAll(async () => {
    await buildExtension({ targets: ["chromium", "firefox"], store: true, outDir: storeDir, quiet: true });
    await buildExtension({ targets: ["chromium"], store: false, outDir: devDir, quiet: true });
  }, 60_000);

  afterAll(() => rmSync(out, { recursive: true, force: true }));

  it("the store bundle leaves a YouTube download to the browser without asking the host", async () => {
    const result = await offerVideoDownload(join(storeDir, "chromium", "background.js"));
    expect(result).toEqual({ released: true, contactedHost: false });
  });

  it("the GitHub/desktop bundle is unchanged: it offers the same download to Whoosh", async () => {
    const result = await offerVideoDownload(join(devDir, "chromium", "background.js"));
    expect(result.contactedHost).toBe(true);
  });

  it("the store manifests keep the content script off blocked sites; the plain one does not", () => {
    for (const target of ["chromium", "firefox"]) {
      const manifest = JSON.parse(readFileSync(join(storeDir, target, "manifest.json"), "utf8"));
      const excluded: string[] = manifest.content_scripts[0].exclude_matches;
      expect(excluded).toContain("*://*.youtube.com/*");
      expect(excluded).toContain("*://*.googlevideo.com/*");
      expect(excluded).toHaveLength(STORE_BLOCKED_SITES.length);
      expect(manifest.key).toBeUndefined();
    }
    const dev = JSON.parse(readFileSync(join(devDir, "chromium", "manifest.json"), "utf8"));
    expect(dev.content_scripts[0].exclude_matches).toBeUndefined();
  });

  it("the store bundle does not sniff media on blocked sites", async () => {
    const check = (file: string) => {
      const bg = loadBackground(file);
      const headers = bg.listener("webRequest.onHeadersReceived");
      headers({
        tabId: 3,
        url: "https://rr5---sn-x.googlevideo.com/videoplayback?mime=video%2Fmp4",
        statusCode: 200,
        responseHeaders: [{ name: "content-type", value: "video/mp4" }],
        initiator: "https://www.youtube.com",
      });
      return bg;
    };
    // The sniffer's per-tab list is only visible through the popup; the
    // observable difference is that the store bundle stores nothing. Ask the
    // popup handler for tab 3's media in both bundles.
    const media = async (file: string) => {
      const bg = check(file);
      const reply = await new Promise<{ media?: unknown[] }>((resolve) => {
        bg.listener("runtime.onMessage")(
          { type: "whoosh/popup-state", tabId: 3, pageUrl: "https://example.org/" },
          { id: "test", url: "chrome-extension://test/popup.html" },
          resolve,
        );
      });
      return reply.media ?? [];
    };
    expect(await media(join(storeDir, "chromium", "background.js"))).toEqual([]);
    expect(await media(join(devDir, "chromium", "background.js"))).toHaveLength(1);
  });
});
