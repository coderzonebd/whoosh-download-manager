import { describe, expect, it, vi } from "vitest";

import { classifyMedia, sizeFromHeaders, TabMediaStore } from "../src/background/sniffer";
import { BypassTracker, RequestMethodTracker } from "../src/background/capture/trackers";
import { dedupeLinks, looksLikeDownloadLink, PreconnectThrottle, selectLinks } from "../src/shared/links";

const TYPES = ["zip", "iso", "mp4"];

describe("links", () => {
  it("de-duplicates, drops fragments and non-http links, keeps document order", () => {
    expect(
      dedupeLinks([
        { url: "https://x.org/a.zip#one", text: "A" },
        { url: "javascript:void(0)", text: "js" },
        { url: "mailto:a@b.c", text: "mail" },
        { url: "https://x.org/b.iso", text: "B" },
        { url: "https://x.org/a.zip#two", text: "A again" },
        { url: "https://x.org/a.zip", text: "A plain" },
      ]),
    ).toEqual([
      { url: "https://x.org/a.zip", text: "A" },
      { url: "https://x.org/b.iso", text: "B" },
    ]);
  });

  it("'all links' keeps downloads only; 'selected links' keeps everything selected", () => {
    const links = [
      { url: "https://x.org/", text: "home" },
      { url: "https://x.org/a.zip", text: "a" },
      { url: "https://x.org/about", text: "about" },
      { url: "https://x.org/v.MP4?x=1", text: "v" },
    ];
    expect(selectLinks(links, "all", TYPES)).toEqual(["https://x.org/a.zip", "https://x.org/v.MP4?x=1"]);
    expect(selectLinks(links, "selection", TYPES)).toHaveLength(4);
    expect(selectLinks(links, "selection", TYPES, 2)).toHaveLength(2);
  });

  it("spots download-looking links", () => {
    expect(looksLikeDownloadLink("https://x.org/a.zip", TYPES, false)).toBe(true);
    expect(looksLikeDownloadLink("https://x.org/page", TYPES, false)).toBe(false);
    expect(looksLikeDownloadLink("https://x.org/page", TYPES, true)).toBe(true);
    expect(looksLikeDownloadLink("ftp://x.org/a.zip", TYPES, true)).toBe(false);
  });
});

describe("PreconnectThrottle", () => {
  it("allows one preconnect per origin per 10 s and reveals only the origin", () => {
    const t = new PreconnectThrottle(10_000);
    expect(t.allow("https://cdn.x.org/a/b.zip?token=secret", 0)).toBe("https://cdn.x.org/");
    expect(t.allow("https://cdn.x.org/other.iso", 5_000)).toBeUndefined();
    expect(t.allow("https://mirror.y.net/c.iso", 5_000)).toBe("https://mirror.y.net/");
    expect(t.allow("https://cdn.x.org/again.zip", 10_000)).toBe("https://cdn.x.org/");
    expect(t.allow("http://cdn.x.org/plain.zip", 10_001)).toBe("http://cdn.x.org/");
    expect(t.allow("blob:https://x/1", 0)).toBeUndefined();
  });

  it("forgets the oldest hosts beyond its cap", () => {
    const t = new PreconnectThrottle(10_000, 2);
    t.allow("https://a.org/x.zip", 0);
    t.allow("https://b.org/x.zip", 0);
    t.allow("https://c.org/x.zip", 0);
    expect(t.allow("https://a.org/x.zip", 1)).toBe("https://a.org/");
  });
});

describe("trackers", () => {
  it("Alt+click bypasses exactly the next download within the window", () => {
    const b = new BypassTracker(5_000);
    expect(b.consume(0)).toBe(false);
    b.arm(1_000);
    expect(b.consume(2_000)).toBe(true);
    expect(b.consume(2_001)).toBe(false);
    b.arm(10_000);
    expect(b.consume(15_001)).toBe(false);
  });

  it("remembers non-GET navigations for a while", () => {
    const m = new RequestMethodTracker(60_000, 2);
    m.record("https://x.org/export#f", "POST", 0);
    m.record("https://x.org/page", "GET", 0);
    expect(m.wasNonGet(["https://x.org/export"], 1_000)).toBe(true);
    expect(m.wasNonGet(["https://x.org/page"], 1_000)).toBe(false);
    expect(m.wasNonGet([undefined, "https://x.org/export"], 61_000)).toBe(false);
    m.record("https://x.org/1", "POST", 0);
    m.record("https://x.org/2", "PUT", 0);
    expect(m.wasNonGet(["https://x.org/export"], 1)).toBe(false); // evicted
  });
});

describe("media sniffer", () => {
  it.each([
    ["https://v.org/live/master.m3u8?x=1", undefined, "hls"],
    ["https://v.org/playlist", "application/vnd.apple.mpegurl", "hls"],
    ["https://v.org/stream", "application/x-mpegURL; charset=utf-8", "hls"],
    ["https://v.org/manifest.mpd", "text/xml", "dash"],
    ["https://v.org/x", "application/dash+xml", "dash"],
    ["https://v.org/movie.mp4", "video/mp4", "video"],
    ["https://v.org/song", "audio/mpeg", "audio"],
  ])("classifies %s (%s) as %s", (url, type, kind) => {
    expect(classifyMedia(url, type)).toBe(kind);
  });

  it.each([
    ["https://v.org/seg-001.ts", "video/mp2t"],
    ["https://v.org/chunk-5.m4s", "video/mp4"],
    ["https://v.org/x", "video/iso.segment"],
    ["https://v.org/page.html", "text/html"],
    ["https://v.org/data.json", "application/json"],
    ["blob:https://v.org/1", "video/mp4"],
  ])("ignores %s (%s)", (url, type) => {
    expect(classifyMedia(url, type)).toBeUndefined();
  });

  it("reads the full size from 200 and 206 responses", () => {
    expect(sizeFromHeaders(200, [{ name: "Content-Length", value: "1234" }])).toBe(1234);
    expect(sizeFromHeaders(206, [{ name: "content-range", value: "bytes 0-1/987654" }])).toBe(987654);
    expect(sizeFromHeaders(206, [{ name: "content-range", value: "bytes 0-1/*" }])).toBeUndefined();
    expect(sizeFromHeaders(200, [{ name: "content-length", value: "abc" }])).toBeUndefined();
    expect(sizeFromHeaders(200, undefined)).toBeUndefined();
  });

  it("keeps a de-duplicated, capped list per tab and survives a restart", () => {
    const changes = vi.fn();
    const store = new TabMediaStore(changes, 2);
    expect(store.add(1, { url: "https://v.org/a.mp4#t=5", kind: "video", seenAt: 1 })).toBe(true);
    expect(store.add(1, { url: "https://v.org/a.mp4", kind: "video", size: 99, seenAt: 2 })).toBe(false);
    expect(store.list(1)).toEqual([{ url: "https://v.org/a.mp4", kind: "video", size: 99, seenAt: 1 }]);
    store.add(1, { url: "https://v.org/b.mp4", kind: "video", seenAt: 3 });
    store.add(1, { url: "https://v.org/c.mp4", kind: "video", seenAt: 4 });
    expect(store.list(1).map((e) => e.url)).toEqual(["https://v.org/b.mp4", "https://v.org/c.mp4"]);
    expect(store.add(-1, { url: "https://v.org/bg.mp4", kind: "video", seenAt: 5 })).toBe(false);

    const restored = new TabMediaStore();
    restored.load(changes.mock.calls.at(-1)?.[0]);
    expect(restored.list(1)).toEqual(store.list(1));

    store.clear(1);
    expect(store.list(1)).toEqual([]);
  });
});
