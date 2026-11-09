// SPDX-License-Identifier: Apache-2.0
import type { SegmentInfo } from "@vayu/api-client";
import { describe, expect, it } from "vitest";
import { etaTemplate, interpolate, scaleBytes, splitDuration } from "../src/lib/format.ts";
import { bn } from "../src/lib/i18n/bn.ts";
import { en, type MessageKey } from "../src/lib/i18n/en.ts";
import { I18n } from "../src/lib/i18n/i18n.svelte.ts";
import { blockCoverage, estimateSegments } from "../src/lib/segments.ts";
import { parsePrefs } from "../src/lib/state/prefs.svelte.ts";
import { pageSize, rowDomId, scrollTopFor, visibleRange } from "../src/lib/virtual.ts";

describe("format", () => {
  it.each([
    [0, 0, "B"],
    [900, 900, "B"],
    [1023.9 * 1024, 1, "MB"], // promoted instead of "1024 KB"
    [1536, 1.5, "KB"],
    [12.5 * 1024 ** 2, 12.5, "MB"],
    [6.2 * 1024 ** 3, 6.2, "GB"],
  ])("scaleBytes(%d) → %d %s", (n, value, unit) => {
    const s = scaleBytes(n);
    expect(s.unit).toBe(unit);
    expect(s.value).toBeCloseTo(value, 1);
  });

  it("keeps three significant figures", () => {
    expect(scaleBytes(1.234 * 1024 ** 3).digits).toBe(2);
    expect(scaleBytes(12.34 * 1024 ** 2).digits).toBe(1);
    expect(scaleBytes(512 * 1024).digits).toBe(0);
  });

  it("splits durations and picks a calm ETA template", () => {
    expect(splitDuration(93_784)).toEqual({ d: 1, h: 2, m: 3, s: 4 });
    expect(etaTemplate(42)).toBe("eta.s");
    expect(etaTemplate(305)).toBe("eta.ms");
    expect(etaTemplate(1800)).toBe("eta.m");
    expect(etaTemplate(7200)).toBe("eta.hm");
    expect(etaTemplate(200_000)).toBe("eta.dh");
  });

  it("interpolates placeholders and leaves unknown ones", () => {
    expect(interpolate("{a} of {b} {c}", { a: 1, b: "two" })).toBe("1 of two {c}");
  });
});

describe("i18n", () => {
  it("formats sizes, speeds, ETAs and percentages in English", () => {
    const i = new I18n("en");
    expect(i.bytes(12.5 * 1024 ** 2)).toBe("12.5 MB");
    expect(i.bytes(undefined)).toBe("Unknown size");
    expect(i.speed(2.5 * 1024 ** 2)).toBe("2.50 MB/s");
    expect(i.eta(3 * 3600 + 12 * 60)).toBe("3h 12m");
    expect(i.eta(undefined)).toBe("—");
    expect(i.percent(0.4237)).toBe("42.3%");
    expect(i.percent(0.9999)).toBe("99.9%"); // never shows 100% before it is done
    expect(i.t("connections.count", { count: 1 })).toBe("1 connection");
    expect(i.t("connections.count", { count: 8 })).toBe("8 connections");
  });

  it("uses Bangla words and Bangla digits", () => {
    const i = new I18n("bn");
    expect(i.intlLocale).toBe("bn-BD");
    expect(i.bytes(12.5 * 1024 ** 2)).toBe("১২.৫ MB");
    expect(i.bytes(900)).toBe("৯০০ বাইট");
    expect(i.speed(512 * 1024)).toBe("৫১২ KB/সে");
    expect(i.eta(3 * 3600 + 12 * 60)).toBe("৩ ঘণ্টা ১২ মিনিট");
    expect(i.t("connections.count", { count: 8 })).toBe("৮টি সংযোগ");
    expect(i.t("toolbar.pause")).toBe("থামান");
    expect(i.percent(0.5)).toBe("৫০.০%");
  });

  it("falls back to English for missing keys and unknown locales", () => {
    const i = new I18n("fr-CA");
    expect(i.locale).toBe("en");
    i.register("xx", { intl: "en", nativeName: "Test", messages: { "toolbar.pause": "PAUSE" } });
    i.setLocale("xx");
    expect(i.t("toolbar.pause")).toBe("PAUSE");
    expect(i.t("toolbar.resume")).toBe("Resume");
  });

  it("derives text direction from the locale", () => {
    const i = new I18n("en");
    expect(i.dir).toBe("ltr");
    i.register("ar", { intl: "ar", nativeName: "العربية", messages: {} });
    i.setLocale("ar");
    expect(i.dir).toBe("rtl");
  });

  it("translates every key into Bangla (no English left behind)", () => {
    // Identical strings are allowed only where they are not words.
    const sameOnPurpose = new Set<MessageKey>(["app.name", "add.urlPlaceholder", "eta.unknown", "detail.segmentRange"]);
    const keys = Object.keys(en) as MessageKey[];
    for (const k of keys) {
      expect(bn[k], k).toBeDefined();
      if (!sameOnPurpose.has(k)) expect(JSON.stringify(bn[k]), k).not.toBe(JSON.stringify(en[k]));
    }
    // Bangla strings keep the same placeholders as English.
    const ph = (m: unknown) => [...JSON.stringify(m).matchAll(/\{(\w+)\}/g)].map((x) => x[1]).sort().join(",");
    for (const k of keys) {
      const enPh = new Set(ph(en[k]).split(","));
      for (const p of ph(bn[k]).split(",")) if (p) expect(enPh.has(p), `${k}: {${p}}`).toBe(true);
    }
  });
});

describe("virtual list maths", () => {
  it("renders only the rows in view plus overscan", () => {
    expect(visibleRange(0, 600, 30, 10_000, 6)).toEqual({ start: 0, end: 27 });
    expect(visibleRange(30_000, 600, 30, 10_000, 6)).toEqual({ start: 994, end: 1027 });
    expect(visibleRange(299_970, 600, 30, 10_000, 6)).toEqual({ start: 9993, end: 10_000 });
    expect(visibleRange(0, 600, 30, 0)).toEqual({ start: 0, end: 0 });
  });

  it("scrolls the minimum needed to reveal a row below a sticky header", () => {
    expect(scrollTopFor(5, 0, 632, 30, 32)).toBeUndefined();
    expect(scrollTopFor(40, 0, 632, 30, 32)).toBe(41 * 30 - 600);
    expect(scrollTopFor(2, 300, 632, 30, 32)).toBe(60);
    expect(pageSize(632, 30, 32)).toBe(19);
  });

  it("makes valid DOM ids from opaque ids", () => {
    expect(rowDomId("dl_01")).toBe("vy-row-dl_01");
    expect(rowDomId("a b/c")).toMatch(/^vy-row-[A-Za-z0-9_-]+$/);
  });
});

describe("segment estimation", () => {
  const seg = (start: number, end: number, done: number, speed: number): SegmentInfo => ({ start, end, downloadedBytes: done, speedBps: speed, source: "" });

  it("distributes new bytes by segment speed and keeps the total exact", () => {
    const segs = [seg(0, 100, 50, 3), seg(100, 200, 10, 1)];
    const fills = estimateSegments(segs, 60 + 40);
    expect(fills.reduce((a, f) => a + f.done, 0)).toBe(100);
    expect(fills[0]?.done).toBe(80);
    expect(fills[1]?.done).toBe(20);
  });

  it("hands surplus from full segments to open ones", () => {
    const fills = estimateSegments([seg(0, 100, 95, 10), seg(100, 200, 0, 0)], 95 + 50);
    expect(fills[0]?.done).toBe(100);
    expect(fills[1]?.done).toBeGreaterThan(40);
  });

  it("maps coverage to blocks", () => {
    const blocks = blockCoverage([{ start: 0, end: 50, done: 50 }, { start: 50, end: 100, done: 25 }], 100, 4);
    expect(blocks).toEqual([1, 1, 1, 0]);
    expect(blockCoverage([{ start: 0, end: 100, done: 10 }], 100, 4)[0]).toBeCloseTo(0.4);
  });
});

describe("prefs", () => {
  it("survives corrupt storage and clamps widths", () => {
    expect(parsePrefs("{not json").layout).toBe("classic");
    const p = parsePrefs(JSON.stringify({ layout: "modern", widths: { name: 5, size: 99_999 }, sort: { key: "speed", dir: "asc" } }));
    expect(p.layout).toBe("modern");
    expect(p.widths.name).toBe(56);
    expect(p.widths.size).toBe(900);
    expect(p.sort).toEqual({ key: "speed", dir: "asc" });
    expect(parsePrefs(JSON.stringify({ sort: { key: "evil", dir: "asc" } })).sort.key).toBe("added");
  });
});
