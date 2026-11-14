import { describe, expect, it } from "vitest";

import { evaluateCapture, fileTypeOf, type CandidateDownload, type RuleContext } from "../src/background/capture/rules";
import { DEFAULT_SETTINGS, normalizeSettings, type Settings } from "../src/shared/settings";

const base: CandidateDownload = {
  id: 1,
  url: "https://cdn.example.com/files/ubuntu.iso",
  referrer: "https://example.com/download",
  filename: "ubuntu.iso",
  totalBytes: 5_000_000_000,
  incognito: false,
  state: "in_progress",
  danger: "safe",
};
const ctx: RuleContext = { bypass: false, nonGetRequest: false };
const settings = (patch: Partial<Settings> = {}): Settings => normalizeSettings({ ...DEFAULT_SETTINGS, ...patch });

describe("evaluateCapture", () => {
  it("captures a normal download of a listed type", () => {
    expect(evaluateCapture(base, settings(), ctx)).toEqual({ capture: true });
  });

  it.each<[string, Partial<CandidateDownload>, Partial<Settings>, Partial<RuleContext>, string]>([
    ["capture is switched off", {}, { captureEnabled: false }, {}, "disabled"],
    ["the user Alt+clicked", {}, {}, { bypass: true }, "bypassed"],
    ["blob: URLs cannot be fetched by the daemon", { url: "blob:https://example.com/1234" }, {}, {}, "not-http"],
    ["data: URLs neither", { url: "data:application/zip;base64,UEsDBA==" }, {}, {}, "not-http"],
    ["private windows by default", { incognito: true }, {}, {}, "incognito"],
    ["another extension started it", { byExtensionId: "abcdefghijklmnopabcdefghijklmnop" }, {}, {}, "extension"],
    ["Safe Browsing flagged it", { danger: "url" }, {}, {}, "dangerous"],
    ["it already finished", { state: "complete" }, {}, {}, "not-in-progress"],
    ["a form POST produced it", {}, {}, { nonGetRequest: true }, "post"],
    ["the download host is excluded", {}, { excludedSites: ["example.com"] }, {}, "excluded-site"],
    ["the referring site is excluded", { url: "https://other.net/a.iso" }, { excludedSites: ["example.com"] }, {}, "excluded-site"],
    ["its type is not listed", { filename: "page.html", url: "https://example.com/page.html" }, {}, {}, "file-type"],
    ["it is smaller than the minimum", { totalBytes: 10_000 }, { minSizeBytes: 1_000_000 }, {}, "too-small"],
    ["the store build stays off this site", {}, {}, { blocked: true }, "store-policy"],
  ])("skips when %s", (_name, item, s, c, reason) => {
    expect(evaluateCapture({ ...base, ...item }, settings(s), { ...ctx, ...c })).toEqual({ capture: false, reason });
  });

  it("captures private-window downloads when allowed", () => {
    expect(evaluateCapture({ ...base, incognito: true }, settings({ captureIncognito: true }), ctx).capture).toBe(true);
  });

  it("captures executables Chromium only marks as a dangerous *type*", () => {
    expect(evaluateCapture({ ...base, danger: "file", filename: "setup.exe" }, settings(), ctx).capture).toBe(true);
  });

  it("captures when the size is unknown even with a minimum", () => {
    const { totalBytes: _drop, ...unknownSize } = base;
    expect(evaluateCapture(unknownSize, settings({ minSizeBytes: 1_000_000 }), ctx).capture).toBe(true);
    expect(evaluateCapture({ ...base, totalBytes: -1 }, settings({ minSizeBytes: 1_000_000 }), ctx).capture).toBe(true);
  });

  it("an empty type list means every type", () => {
    const html = { ...base, filename: "page.html", url: "https://example.com/page.html" };
    expect(evaluateCapture(html, settings({ fileTypes: [] }), ctx).capture).toBe(true);
  });

  it("excluded sites also cover subdomains", () => {
    const item = { ...base, url: "https://dl.mirror.example.org/x.zip", referrer: "https://news.site/" };
    expect(evaluateCapture(item, settings({ excludedSites: ["mirror.example.org"] }), ctx)).toEqual({
      capture: false,
      reason: "excluded-site",
    });
    expect(evaluateCapture(item, settings({ excludedSites: ["ample.org"] }), ctx).capture).toBe(true);
  });
});

describe("fileTypeOf", () => {
  it("prefers the browser's file name", () => {
    expect(fileTypeOf({ filename: "/home/u/Downloads/Report.PDF", url: "https://x/dl?id=1" })).toBe("pdf");
    expect(fileTypeOf({ filename: "C:\\Users\\u\\Downloads\\setup.exe", url: "https://x/dl" })).toBe("exe");
  });

  it("falls back to the URL, then the MIME type", () => {
    expect(fileTypeOf({ url: "https://x/a/archive.tar.gz?sig=abc#frag" })).toBe("gz");
    expect(fileTypeOf({ url: "https://x/download?id=7", mime: "application/zip; charset=binary" })).toBe("zip");
    expect(fileTypeOf({ url: "https://x/download?id=7", mime: "application/octet-stream" })).toBe("");
  });
});

describe("Alt+click with \"send to Whoosh\" (force)", () => {
  const force: RuleContext = { bypass: false, nonGetRequest: false, force: true };

  it("overrides capture off, excluded sites, file types and the minimum size", () => {
    const s = settings({ captureEnabled: false, excludedSites: ["example.com"], fileTypes: ["mp4"], minSizeBytes: 1e12 });
    expect(evaluateCapture({ ...base, filename: "page.html", totalBytes: 10 }, s, force)).toEqual({ capture: true });
  });

  it("never overrides the safety rules", () => {
    expect(evaluateCapture({ ...base, danger: "url" }, settings(), force)).toEqual({ capture: false, reason: "dangerous" });
    expect(evaluateCapture({ ...base, url: "blob:https://x/1" }, settings(), force)).toEqual({ capture: false, reason: "not-http" });
    expect(evaluateCapture(base, settings(), { ...force, nonGetRequest: true })).toEqual({ capture: false, reason: "post" });
    expect(evaluateCapture({ ...base, incognito: true }, settings(), force)).toEqual({ capture: false, reason: "incognito" });
  });
});
