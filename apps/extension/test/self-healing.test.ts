import type { DownloadSummary, RefreshUrlParams } from "@whoosh/api-client";
import { describe, expect, it } from "vitest";

import { isSameFile, matchLink, siteOf, type HealTarget } from "../src/background/healing/match";
import {
  ARM_MS,
  LinkHealer,
  MAX_ENTRIES,
  MAX_FAILED_ROUNDS,
  ROUND_GAP_MS,
  STORAGE_KEY,
  type HealAsk,
  type HealerDeps,
  type Provenance,
} from "../src/background/self-healing";
import type { LinkInfo } from "../src/shared/messages";
import { DEFAULT_SETTINGS, normalizeSettings, type Settings } from "../src/shared/settings";
import { RpcError } from "../src/transport/rpc";

const link = (url: string, text = ""): LinkInfo => ({ url, text });

describe("matchLink", () => {
  const target: HealTarget = {
    url: "https://cdn.example.com/files/linux.iso?sig=old&exp=1",
    linkUrl: "https://example.com/get/linux",
    filename: "linux.iso",
    originPageUrl: "https://example.com/downloads",
  };

  it("prefers the stable link the download started from", () => {
    const links = [link("https://example.com/about"), link("https://example.com/get/linux#top")];
    expect(matchLink(links, target)).toEqual({ url: "https://example.com/get/linux", reason: "same-link", confidence: "sure" });
  });

  it("takes a re-signed copy of the same path", () => {
    const fresh = "https://cdn.example.com/files/linux.iso?sig=new&exp=2";
    expect(matchLink([link("https://example.com/"), link(fresh)], { url: target.url, filename: "linux.iso" })).toEqual({
      url: fresh,
      reason: "same-path",
      confidence: "sure",
    });
  });

  it("never proposes the expired URL itself", () => {
    expect(matchLink([link(target.url)], { url: target.url })).toBeUndefined();
  });

  it("is unsure when one path has several fresh copies", () => {
    const m = matchLink(
      [link("https://cdn.example.com/files/linux.iso?part=1"), link("https://cdn.example.com/files/linux.iso?part=2")],
      { url: target.url },
    );
    expect(m?.confidence).toBe("unsure");
  });

  it("matches the same file name on the same site, and only guesses on another host", () => {
    const sameSite = matchLink([link("https://mirror.example.com/pub/LINUX.ISO")], { url: target.url, filename: "linux.iso" });
    expect(sameSite).toMatchObject({ reason: "same-name", confidence: "sure" });
    const otherHost = matchLink([link("https://elsewhere.net/linux.iso")], { url: target.url, filename: "linux.iso" });
    expect(otherHost).toMatchObject({ reason: "same-name-other-host", confidence: "unsure" });
  });

  it("falls back to the clicked link's text, as a guess", () => {
    const m = matchLink([link("https://example.com/dl?id=9", "  Download   Linux ")], {
      url: "https://cdn.example.com/x?sig=1",
      linkText: "download linux",
    });
    expect(m).toEqual({ url: "https://example.com/dl?id=9", reason: "same-text", confidence: "unsure" });
  });

  it("finds nothing among unrelated links", () => {
    expect(matchLink([link("https://example.com/about", "About")], target)).toBeUndefined();
    expect(matchLink([link("javascript:void(0)")], target)).toBeUndefined();
  });
});

describe("siteOf / isSameFile", () => {
  it("reduces hosts to their site", () => {
    expect(siteOf("dl.cdn.example.com")).toBe("example.com");
    expect(siteOf("files.example.co.uk")).toBe("example.co.uk");
    expect(siteOf("127.0.0.1")).toBe("127.0.0.1");
  });

  const target: HealTarget = {
    url: "https://cdn.example.com/a/linux.iso?sig=1",
    originPageUrl: "https://example.com/downloads",
    filename: "linux.iso",
    totalBytes: 1000,
  };

  it("needs the same name, the same size when known, and the same site unless armed", () => {
    expect(isSameFile({ url: "https://cdn2.example.com/linux.iso?sig=2", totalBytes: 1000 }, target, false)).toBe(true);
    expect(isSameFile({ url: "https://cdn2.example.com/linux.iso?sig=2", totalBytes: 999 }, target, false)).toBe(false);
    expect(isSameFile({ url: "https://other.net/linux.iso" }, target, false)).toBe(false);
    expect(isSameFile({ url: "https://other.net/linux.iso" }, target, true)).toBe(true);
    expect(isSameFile({ url: "https://cdn.example.com/a/other.iso" }, target, true)).toBe(false);
    expect(
      isSameFile({ url: "https://other.net/x?y=1", filename: "/home/u/Downloads/linux.iso", referrer: "https://example.com/" }, target, false),
    ).toBe(true);
  });
});

// ---------------------------------------------------------------- LinkHealer

function memoryStore() {
  const data: Record<string, unknown> = {};
  return {
    data,
    async get(key: string) {
      return key in data ? { [key]: structuredClone(data[key]) } : {};
    },
    async set(items: Record<string, unknown>) {
      Object.assign(data, structuredClone(items));
    },
  };
}

const PAGE = "https://example.com/downloads";
const EXPIRED = "https://cdn.example.com/files/linux.iso?sig=old";

function provenance(patch: Partial<Provenance> = {}): Provenance {
  return {
    daemonId: "dl_1",
    url: EXPIRED,
    originPageUrl: PAGE,
    filename: "linux.iso",
    totalBytes: 1000,
    capturedAt: 0,
    ...patch,
  };
}

function healer(
  opts: {
    pages?: Record<string, LinkInfo[] | "fail">;
    settings?: Partial<Settings>;
    refuse?: "not-found" | "invalid";
    blocked?: boolean;
  } = {},
) {
  let clock = 1_000_000;
  const store = memoryStore();
  const refreshes: RefreshUrlParams[] = [];
  const pageReads: string[] = [];
  const opened: string[] = [];
  const asksSeen: HealAsk[][] = [];
  const settings = normalizeSettings({ ...DEFAULT_SETTINGS, ...opts.settings });
  const deps: HealerDeps = {
    store,
    daemon: {
      call: (async (method: string, params: unknown) => {
        if (method !== "download.refreshUrl") throw new Error(`unexpected ${method}`);
        if (opts.refuse === "not-found") throw new RpcError({ code: -32002, message: "no such download" });
        if (opts.refuse === "invalid") throw new RpcError({ code: -32003, message: "completed" });
        refreshes.push(params as RefreshUrlParams);
        return null;
      }) as HealerDeps["daemon"]["call"],
    },
    cookies: { getAll: async ({ url }) => [{ name: "sid", value: new URL(url).hostname, path: "/" }] },
    pageLinks: async (url) => {
      pageReads.push(url);
      const page = opts.pages?.[url];
      if (page === "fail" || page === undefined) throw new Error("did not load");
      return page;
    },
    openPage: async (url) => void opened.push(url),
    settings: async () => settings,
    blocked: () => opts.blocked === true,
    userAgent: "UA",
    now: () => clock,
    onAsksChanged: (asks) => void asksSeen.push(asks),
  };
  return {
    h: new LinkHealer(deps),
    store,
    refreshes,
    pageReads,
    opened,
    asksSeen,
    advance: (ms: number) => (clock += ms),
    now: () => clock,
  };
}

const expired = { id: "dl_1", state: "needs_refresh" as const, error: { code: "link_expired", message: "expired" } };

describe("LinkHealer", () => {
  it("heals only downloads it handed over", async () => {
    const { h, refreshes } = healer();
    await expect(h.heal(expired)).resolves.toEqual({ status: "unsupported", reason: "unknown" });
    expect(refreshes).toEqual([]);
  });

  it("first retries the stable link with fresh cookies and the referrer", async () => {
    const { h, refreshes, store } = healer();
    await h.remember(provenance({ linkUrl: "https://example.com/get/linux", capturedAt: 999_000 }));
    expect(Object.keys(store.data[STORAGE_KEY] as object)).toEqual(["dl_1"]);
    const outcome = await h.heal(expired);
    expect(outcome).toMatchObject({ status: "refreshed", via: "cookies", url: "https://example.com/get/linux" });
    expect(refreshes).toEqual([
      {
        id: "dl_1",
        url: "https://example.com/get/linux",
        request: { cookie: "sid=example.com", referer: PAGE, userAgent: "UA" },
      },
    ]);
  });

  it("then reads the page and proposes the re-signed link", async () => {
    const fresh = "https://cdn.example.com/files/linux.iso?sig=new";
    const { h, refreshes, pageReads } = healer({ pages: { [PAGE]: [link("https://example.com/"), link(fresh, "Linux")] } });
    await h.remember(provenance({ capturedAt: 999_000 }));
    await h.heal(expired); // cookies on the same URL
    expect(refreshes.map((r) => r.url)).toEqual([EXPIRED]);
    // The daemon reports it is still expired.
    const outcome = await h.onState(expired);
    expect(outcome).toMatchObject({ status: "refreshed", via: "page", url: fresh });
    expect(pageReads).toEqual([PAGE]);
    expect(refreshes.at(-1)).toMatchObject({ url: fresh, request: { cookie: "sid=cdn.example.com", referer: PAGE } });
  });

  it("asks before using a guess, and uses it on yes", async () => {
    const guess = "https://mirror.other.net/linux.iso";
    const { h, refreshes, asksSeen } = healer({ pages: { [PAGE]: [link(guess)] } });
    await h.remember(provenance({ capturedAt: 999_000 }));
    await h.heal(expired);
    const asked = await h.onState(expired);
    expect(asked).toMatchObject({ status: "asked", ask: { kind: "confirm", candidateUrl: guess, filename: "linux.iso" } });
    expect(refreshes).toHaveLength(1); // only the cookie retry
    expect(asksSeen.at(-1)).toHaveLength(1);
    expect(await h.asks()).toHaveLength(1);
    // While asked, further events do not start new attempts.
    expect((await h.onState(expired))?.status).toBe("asked");

    await expect(h.act("dl_1", "use-candidate")).resolves.toMatchObject({ status: "refreshed", via: "user", url: guess });
    expect(refreshes.at(-1)?.url).toBe(guess);
    expect(await h.asks()).toEqual([]);
    expect(asksSeen.at(-1)).toEqual([]);
  });

  it("asks the user to open the page when nothing is found", async () => {
    const { h, opened } = healer({ pages: { [PAGE]: "fail" } });
    await h.remember(provenance({ capturedAt: 999_000 }));
    await h.heal(expired);
    const outcome = await h.onState(expired);
    expect(outcome).toMatchObject({ status: "asked", ask: { kind: "open-page", pageUrl: PAGE } });

    await h.act("dl_1", "open-page");
    expect(opened).toEqual([PAGE]);
    expect(await h.asks()).toEqual([]);
  });

  it("limits automatic rounds: ten minutes apart, three at most", async () => {
    const { h, refreshes, advance } = healer({ pages: { [PAGE]: [] } });
    await h.remember(provenance({ capturedAt: 999_000 }));
    const runRound = async () => {
      await h.onState(expired); // cookies
      return h.onState(expired); // page: nothing → round fails, ask
    };
    await runRound();
    expect(refreshes).toHaveLength(1);
    await h.act("dl_1", "open-page"); // clears the question; the user did not succeed
    // Too soon for another automatic round.
    advance(ROUND_GAP_MS - 1);
    expect((await h.onState(expired))?.status).toBe("asked");
    expect(refreshes).toHaveLength(1);

    for (let round = 2; round <= MAX_FAILED_ROUNDS; round++) {
      await h.act("dl_1", "open-page");
      advance(ROUND_GAP_MS);
      await runRound();
      expect(refreshes).toHaveLength(round);
    }
    await h.act("dl_1", "open-page");
    advance(ROUND_GAP_MS * 10);
    expect((await h.onState(expired))?.status).toBe("asked");
    expect(refreshes).toHaveLength(MAX_FAILED_ROUNDS);
  });

  it("starts afresh after an address worked", async () => {
    const { h, refreshes } = healer({ pages: { [PAGE]: [] } });
    await h.remember(provenance({ capturedAt: 999_000 }));
    await h.onState(expired);
    await h.onState({ id: "dl_1", state: "downloading" });
    // Expires again later: the cheap strategy is tried first again.
    await h.onState(expired);
    expect(refreshes.map((r) => r.url)).toEqual([EXPIRED, EXPIRED]);
  });

  it("stops after the user dismisses, or the daemon says the file changed", async () => {
    const a = healer({ pages: { [PAGE]: [] } });
    await a.h.remember(provenance({ capturedAt: 999_000 }));
    await a.h.onState(expired);
    await a.h.onState(expired);
    await a.h.act("dl_1", "dismiss");
    a.advance(ROUND_GAP_MS * 2);
    await expect(a.h.onState(expired)).resolves.toEqual({ status: "failed", reason: "user-declined" });
    expect(await a.h.asks()).toEqual([]);

    const b = healer();
    await b.h.remember(provenance({ capturedAt: 999_000 }));
    await b.h.onState(expired);
    await b.h.onState({ id: "dl_1", state: "failed", error: { code: "remote_changed", message: "changed" } });
    await expect(b.h.onState(expired)).resolves.toEqual({ status: "failed", reason: "user-declined" });
    expect(b.refreshes).toHaveLength(1);
  });

  it("does nothing when switched off, or for store-blocked sites", async () => {
    const off = healer({ settings: { selfHealing: false } });
    await off.h.remember(provenance({ capturedAt: 999_000 }));
    await expect(off.h.heal(expired)).resolves.toEqual({ status: "unsupported", reason: "disabled" });

    const blocked = healer({ blocked: true });
    await blocked.h.remember(provenance({ capturedAt: 999_000 }));
    expect(await blocked.h.ids()).toEqual([]);
  });

  it("forgets a download the daemon no longer knows", async () => {
    const { h } = healer({ refuse: "not-found" });
    await h.remember(provenance({ capturedAt: 999_000 }));
    await expect(h.heal(expired)).resolves.toBeDefined();
    expect(await h.ids()).toEqual([]);
  });

  it("forgets completed and removed downloads, keeps at most 500 for 30 days", async () => {
    const { h, now } = healer();
    await h.remember(provenance({ daemonId: "a", capturedAt: now() }));
    await h.remember(provenance({ daemonId: "b", capturedAt: now() }));
    await h.remember(provenance({ daemonId: "old", capturedAt: now() - 31 * 24 * 3600_000 }));
    expect(await h.ids()).toEqual(["a", "b"]);
    await h.onState({ id: "a", state: "completed" });
    await h.onRemoved(["b"]);
    expect(await h.ids()).toEqual([]);

    for (let i = 0; i < MAX_ENTRIES + 5; i++) await h.remember(provenance({ daemonId: `d${i}`, capturedAt: now() + i }));
    const ids = await h.ids();
    expect(ids).toHaveLength(MAX_ENTRIES);
    expect(ids).not.toContain("d0");
    expect(ids).toContain(`d${MAX_ENTRIES + 4}`);
  });

  it("survives a restart: records are read back from storage", async () => {
    const first = healer();
    await first.h.remember(provenance({ capturedAt: first.now() }));
    const second = new LinkHealer({
      store: first.store,
      daemon: { call: (async () => null) as HealerDeps["daemon"]["call"] },
      cookies: { getAll: async () => [] },
      pageLinks: async () => [],
      openPage: async () => {},
      settings: async () => DEFAULT_SETTINGS,
      blocked: () => false,
      userAgent: "UA",
      now: first.now,
    });
    expect(await second.ids()).toEqual(["dl_1"]);
    expect(second.hasActive()).toBe(true);
  });

  it("keeps the port open only while a remembered download may still run", async () => {
    const { h } = healer();
    expect(h.hasActive()).toBe(false);
    await h.remember(provenance({ capturedAt: 999_000 }));
    expect(h.hasActive()).toBe(true); // state unknown yet
    await h.onState({ id: "dl_1", state: "paused" });
    expect(h.hasActive()).toBe(false);
    await h.onState({ id: "dl_1", state: "downloading" });
    expect(h.hasActive()).toBe(true);
  });

  it("catches up: forgets what finished, heals what expired, while no events came", async () => {
    const { h, refreshes } = healer();
    for (const id of ["gone", "done", "running", "dl_1"]) await h.remember(provenance({ daemonId: id, capturedAt: 999_000 }));
    const summary = (id: string, state: DownloadSummary["state"]): DownloadSummary => ({
      id,
      url: EXPIRED,
      filename: "linux.iso",
      directory: "/d",
      state,
      downloadedBytes: 1,
      speedBps: 0,
      connections: 0,
      resumable: true,
      addedAt: 0,
    });
    const outcomes = await h.sync([summary("running", "downloading"), summary("dl_1", "needs_refresh"), summary("done", "completed")]);
    expect(outcomes).toEqual([expect.objectContaining({ status: "refreshed", via: "cookies" })]);
    expect((await h.ids()).sort()).toEqual(["dl_1", "running"]);
    expect(refreshes.map((r) => r.id)).toEqual(["dl_1"]);
  });

  it("recognises the same file downloaded again and refreshes from it", async () => {
    const { h, refreshes, advance } = healer({ pages: { [PAGE]: [] } });
    await h.remember(provenance({ capturedAt: 999_000 }));
    const settings = normalizeSettings(DEFAULT_SETTINGS);
    const again = { url: "https://cdn.example.com/files/linux.iso?sig=brand-new", referrer: PAGE, totalBytes: 1000 };
    // Not expired yet: no match.
    expect(await h.match(again, settings)).toBeUndefined();
    await h.onState(expired);
    await h.onState(expired); // asks to open the page
    expect(await h.match(again, settings)).toBe("dl_1");
    expect(await h.match({ ...again, totalBytes: 5 }, settings)).toBeUndefined();
    // With capture off, only an "Open page" answer arms it, from any site.
    const off = normalizeSettings({ ...DEFAULT_SETTINGS, captureEnabled: false });
    const elsewhere = { url: "https://mirror.net/linux.iso" };
    expect(await h.match(elsewhere, off)).toBeUndefined();
    await h.act("dl_1", "open-page");
    expect(await h.match(elsewhere, off)).toBe("dl_1");
    advance(ARM_MS + 1);
    expect(await h.match(elsewhere, off)).toBeUndefined();

    await h.refreshFromBrowser("dl_1", { ...again, cookieStoreId: "firefox-container-2" });
    expect(refreshes.at(-1)).toMatchObject({ id: "dl_1", url: again.url, request: { referer: PAGE } });
    expect(await h.asks()).toEqual([]);
  });

  it("reports a refused proposal", async () => {
    const { h } = healer({ refuse: "invalid" });
    await h.remember(provenance({ capturedAt: 999_000 }));
    await h.onState(expired);
    await expect(h.refreshFromBrowser("dl_1", { url: "https://cdn.example.com/x/linux.iso" })).rejects.toThrow();
  });

  it("applies an event that arrived before the hand-off finished", async () => {
    const { h, refreshes } = healer();
    // The daemon reports the link expired before remember() ran.
    await h.onState(expired);
    expect(refreshes).toEqual([]);
    await h.remember(provenance({ capturedAt: 999_000 }));
    expect(refreshes.map((r) => r.id)).toEqual(["dl_1"]);

    const late = healer();
    await late.h.onRemoved(["dl_1"]);
    await late.h.remember(provenance({ capturedAt: 999_000 }));
    expect(await late.h.ids()).toEqual([]);

    const stale = healer();
    await stale.h.onState(expired);
    stale.advance(61_000);
    await stale.h.onState({ id: "other", state: "paused" }); // prunes old early events
    await stale.h.remember(provenance({ capturedAt: stale.now() }));
    expect(stale.refreshes).toEqual([]);
  });
});
