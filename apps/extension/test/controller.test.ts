import type { AddParams } from "@whoosh/api-client";
import { describe, expect, it, vi } from "vitest";

import { CaptureController, type CaptureDeps } from "../src/background/capture/controller";
import type { BrowserDownloadState } from "../src/background/capture/handoff";
import type { CandidateDownload } from "../src/background/capture/rules";
import { BypassTracker, RequestMethodTracker } from "../src/background/capture/trackers";
import { DEFAULT_SETTINGS, normalizeSettings, type Settings } from "../src/shared/settings";
import { TransportError } from "../src/transport/rpc";

const item: CandidateDownload = {
  id: 7,
  url: "https://cdn.example.com/signed/linux.iso?sig=abc",
  originalUrl: "https://example.com/get/linux",
  referrer: "https://example.com/downloads#latest",
  filename: "linux.iso",
  totalBytes: 3_000_000_000,
  incognito: false,
  state: "in_progress",
};

function harness(opts: { settings?: Partial<Settings>; daemon?: "accepts" | "missing" } = {}) {
  const states = new Map<number, BrowserDownloadState>([[item.id, "in_progress"]]);
  const added: AddParams[] = [];
  const cookieQueries: unknown[] = [];
  const settings = normalizeSettings({ ...DEFAULT_SETTINGS, ...opts.settings });
  const deps: CaptureDeps = {
    daemon: {
      call: (async (method: string, params: unknown) => {
        if (method !== "download.add") throw new Error(`unexpected ${method}`);
        if (opts.daemon === "missing") throw new TransportError("host-missing", "not installed");
        added.push(params as AddParams);
        return { id: "dl_9" };
      }) as CaptureDeps["daemon"]["call"],
    },
    downloads: {
      cancel: async (id) => void states.set(id, "interrupted"),
      erase: async (id) => void states.delete(id),
      state: async (id) => states.get(id) ?? "missing",
    },
    cookies: {
      getAll: async (details) => {
        cookieQueries.push(details);
        return [
          { name: "sid", value: "s3cret", path: "/" },
          { name: "cdn", value: "edge", path: "/signed/" },
        ];
      },
    },
    settings: async () => settings,
    bypass: new BypassTracker(),
    methods: new RequestMethodTracker(),
    userAgent: "Mozilla/5.0 (X11; Linux x86_64) Test",
    isFirefox: false,
    now: () => 1_000,
  };
  return { deps, states, added, cookieQueries, controller: new CaptureController(deps) };
}

describe("CaptureController", () => {
  it("hands a matching download over with the browser's request context", async () => {
    const { controller, added, states, cookieQueries } = harness();
    const release = vi.fn();
    const result = await controller.onBrowserDownload(item, release);
    expect(result).toEqual({ outcome: "sent", daemonId: "dl_9" });
    expect(added).toEqual([
      {
        url: "https://cdn.example.com/signed/linux.iso?sig=abc",
        filename: "linux.iso",
        request: {
          cookie: "cdn=edge; sid=s3cret",
          referer: "https://example.com/downloads",
          userAgent: "Mozilla/5.0 (X11; Linux x86_64) Test",
        },
      },
    ]);
    expect(cookieQueries).toEqual([{ url: "https://cdn.example.com/signed/linux.iso?sig=abc" }]);
    expect(states.has(item.id)).toBe(false); // cancelled and erased
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("never loses the download when the host is missing", async () => {
    const { controller, states } = harness({ daemon: "missing" });
    const release = vi.fn();
    const result = await controller.onBrowserDownload(item, release);
    expect(result).toMatchObject({ outcome: "kept", reason: "error" });
    expect(states.get(item.id)).toBe("in_progress");
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("releases skipped downloads immediately without touching the daemon", async () => {
    const { controller, added } = harness({ settings: { captureEnabled: false } });
    const release = vi.fn();
    await expect(controller.onBrowserDownload(item, release)).resolves.toEqual({ outcome: "skipped", reason: "disabled" });
    expect(release).toHaveBeenCalledTimes(1);
    expect(added).toEqual([]);
  });

  it("honours an Alt+click once", async () => {
    const { controller, deps } = harness();
    deps.bypass.arm(900);
    await expect(controller.onBrowserDownload(item, () => {})).resolves.toEqual({ outcome: "skipped", reason: "bypassed" });
    await expect(controller.onBrowserDownload({ ...item, id: 8 }, () => {})).resolves.toMatchObject({ outcome: "sent" });
  });

  it("does not spend an Alt+click while capture is off", async () => {
    const { controller, deps } = harness({ settings: { captureEnabled: false } });
    deps.bypass.arm(900);
    await controller.onBrowserDownload(item, () => {});
    expect(deps.bypass.consume(1_000)).toBe(true);
  });

  it("leaves downloads produced by a form POST in the browser", async () => {
    const { controller, deps } = harness();
    deps.methods.record("https://example.com/get/linux", "POST", 500);
    await expect(controller.onBrowserDownload(item, () => {})).resolves.toEqual({ outcome: "skipped", reason: "post" });
  });

  it("reads private-window cookies from the private store when allowed", async () => {
    const { controller, cookieQueries } = harness({ settings: { captureIncognito: true } });
    await controller.onBrowserDownload({ ...item, incognito: true }, () => {});
    expect(cookieQueries).toEqual([{ url: item.url, storeId: "1" }]);
  });

  it("uses a Firefox container's cookie store", async () => {
    const { controller, cookieQueries } = harness();
    await controller.onBrowserDownload({ ...item, cookieStoreId: "firefox-container-2" }, () => {});
    expect(cookieQueries).toEqual([{ url: item.url, storeId: "firefox-container-2" }]);
  });

  it("releases even if reading settings throws", async () => {
    const { controller, deps } = harness();
    deps.settings = async () => {
      throw new Error("storage broken");
    };
    const release = vi.fn();
    await expect(controller.onBrowserDownload(item, release)).resolves.toMatchObject({ outcome: "kept" });
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("sends an Alt+click to Whoosh when the setting says so, whatever the rules", async () => {
    const { controller, deps, added } = harness({ settings: { captureEnabled: false, fileTypes: ["mp4"] } });
    deps.force = new BypassTracker();
    deps.force.arm(900);
    await expect(controller.onBrowserDownload(item, () => {})).resolves.toMatchObject({ outcome: "sent" });
    expect(added).toHaveLength(1);
    await expect(controller.onBrowserDownload({ ...item, id: 8 }, () => {})).resolves.toEqual({ outcome: "skipped", reason: "disabled" });
  });

  it("leaves store-blocked downloads to the browser", async () => {
    const { controller, deps, added } = harness();
    deps.blocked = (urls) => urls.some((u) => u?.includes("example.com"));
    await expect(controller.onBrowserDownload(item, () => {})).resolves.toEqual({ outcome: "skipped", reason: "store-policy" });
    expect(added).toEqual([]);
  });

  describe("a fresh copy of an expired download", () => {
    function withExpired(refresh: "accepts" | "refuses" | "hangs") {
      const h = harness({ settings: { handoffTimeoutMs: 1_000 } });
      const refreshed: unknown[] = [];
      h.deps.expired = {
        match: async (file) => (file.url.includes("linux.iso") ? "dl_old" : undefined),
        refreshFromBrowser: async (id, file) => {
          if (refresh === "refuses") throw new Error("refused");
          if (refresh === "hangs") return new Promise<void>(() => {});
          refreshed.push({ id, ...file });
        },
      };
      return { ...h, refreshed };
    }

    it("continues the expired download and cancels the browser copy", async () => {
      const { controller, states, added, refreshed } = withExpired("accepts");
      const release = vi.fn();
      await expect(controller.onBrowserDownload(item, release)).resolves.toEqual({ outcome: "healed", daemonId: "dl_old" });
      expect(refreshed).toEqual([
        {
          id: "dl_old",
          url: item.url,
          originalUrl: item.originalUrl,
          referrer: item.referrer,
          filename: item.filename,
          totalBytes: item.totalBytes,
        },
      ]);
      expect(added).toEqual([]);
      expect(states.has(item.id)).toBe(false);
      expect(release).toHaveBeenCalledTimes(1);
    });

    it("keeps the browser copy when the daemon refuses or is too slow", async () => {
      for (const mode of ["refuses", "hangs"] as const) {
        const { controller, states } = withExpired(mode);
        const release = vi.fn();
        const result = await controller.onBrowserDownload(item, release);
        expect(result).toMatchObject({ outcome: "kept", reason: mode === "refuses" ? "error" : "timeout", healing: "dl_old" });
        expect(states.get(item.id)).toBe("in_progress");
        expect(release).toHaveBeenCalledTimes(1);
      }
    });

    it("is not used when the user Alt+clicked for the browser", async () => {
      const { controller, deps, refreshed } = withExpired("accepts");
      deps.bypass.arm(900);
      await expect(controller.onBrowserDownload(item, () => {})).resolves.toEqual({ outcome: "skipped", reason: "bypassed" });
      expect(refreshed).toEqual([]);
    });
  });
});
