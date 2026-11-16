// SPDX-License-Identifier: GPL-3.0-or-later
// Handing a page to the app's media picker (`media.offer`), and the popup's
// media list without page sounds.
import { describe, expect, it } from "vitest";

import type { MediaOfferParams } from "@wdm/api-client";

import { offerPage, type OfferDeps } from "../src/background/offer";
import { MIN_LISTED_MEDIA_BYTES, worthListing } from "../src/background/sniffer";
import { asContentMessage, asPopupMessage, type MediaEntry } from "../src/shared/messages";
import { isPolicyBlocked, STORE_BLOCKED_SITES } from "../src/shared/policy";
import { DEFAULT_SETTINGS, type Settings } from "../src/shared/settings";

function deps(opts: { settings?: Partial<Settings>; store?: boolean; fail?: string } = {}) {
  const calls: MediaOfferParams[] = [];
  const d: OfferDeps = {
    daemon: {
      call: async (_m, params) => {
        calls.push(params);
        if (opts.fail) throw new Error(opts.fail);
        return { id: "of_1" };
      },
    },
    settings: async () => ({ ...DEFAULT_SETTINGS, ...opts.settings }),
    blocked: (...urls) => isPolicyBlocked(urls, opts.store ? STORE_BLOCKED_SITES : []),
  };
  return { d, calls };
}

describe("offering a page to the app", () => {
  it("sends the page and its cleaned-up title", async () => {
    const { d, calls } = deps();
    const reply = await offerPage(d, { url: "https://www.youtube.com/watch?v=abc", title: "  A\n video - YouTube ", fromButton: true });
    expect(reply).toEqual({ ok: true });
    expect(calls).toEqual([{ url: "https://www.youtube.com/watch?v=abc", title: "A video - YouTube" }]);
  });

  it("leaves out an empty title and refuses what is not a web page", async () => {
    const { d, calls } = deps();
    await offerPage(d, { url: "https://vimeo.com/1", title: "   ", fromButton: false });
    expect(calls).toEqual([{ url: "https://vimeo.com/1" }]);
    expect((await offerPage(d, { url: "file:///tmp/x.mp4", fromButton: false })).ok).toBe(false);
    expect(calls).toHaveLength(1);
  });

  it("the button respects the option and excluded sites; the popup's explicit request does not", async () => {
    const off = deps({ settings: { videoButton: false } });
    expect((await offerPage(off.d, { url: "https://vimeo.com/1", fromButton: true })).ok).toBe(false);
    expect(off.calls).toHaveLength(0);
    const excluded = deps({ settings: { excludedSites: ["vimeo.com"] } });
    expect((await offerPage(excluded.d, { url: "https://player.vimeo.com/video/1", fromButton: true })).ok).toBe(false);
    // An embedded player whose page is excluded.
    expect((await offerPage(excluded.d, { url: "https://cdn.example/embed/1", tabUrl: "https://vimeo.com/x", fromButton: true })).ok).toBe(false);
    expect((await offerPage(excluded.d, { url: "https://vimeo.com/1", fromButton: false })).ok).toBe(true);
  });

  it("the store policy blocks YouTube and similar sites, also as the embedding page", async () => {
    const store = deps({ store: true });
    expect((await offerPage(store.d, { url: "https://www.youtube.com/watch?v=x", fromButton: false })).ok).toBe(false);
    expect((await offerPage(store.d, { url: "https://www.youtube-nocookie.com/embed/x", tabUrl: "https://blog.example/", fromButton: true })).ok).toBe(false);
    expect((await offerPage(store.d, { url: "https://blog.example/talk", tabUrl: "https://www.youtube.com/", fromButton: true })).ok).toBe(false);
    expect(store.calls).toHaveLength(0);
    expect((await offerPage(store.d, { url: "https://vimeo.com/1", fromButton: true })).ok).toBe(true);
  });

  it("sends what the browser would send to the page, from the tab's cookie store", async () => {
    const { d, calls } = deps();
    const asked: (string | undefined)[][] = [];
    d.request = async (url, referrer, store) => {
      asked.push([url, referrer, store]);
      return { cookie: "sessionid=abc", userAgent: "UA" };
    };
    await offerPage(d, {
      url: "https://www.instagram.com/reel/xyz/",
      title: "A reel",
      tabUrl: "https://www.instagram.com/someone/",
      fromButton: true,
      cookieStoreId: "firefox-container-2",
    });
    expect(asked).toEqual([["https://www.instagram.com/reel/xyz/", "https://www.instagram.com/someone/", "firefox-container-2"]]);
    expect(calls[0]?.request).toEqual({ cookie: "sessionid=abc", userAgent: "UA" });
  });

  it("still offers the page when the cookies cannot be read", async () => {
    const { d, calls } = deps();
    d.request = async () => {
      throw new Error("no access");
    };
    expect(await offerPage(d, { url: "https://vimeo.com/1", fromButton: false })).toEqual({ ok: true });
    expect(calls).toEqual([{ url: "https://vimeo.com/1" }]);
  });

  it("reports the daemon's error", async () => {
    const { d } = deps({ fail: "the wdm daemon is not running" });
    expect(await offerPage(d, { url: "https://vimeo.com/1", fromButton: true })).toEqual({ ok: false, error: "the wdm daemon is not running" });
  });

  it("validates the new messages", () => {
    expect(asContentMessage({ type: "wdm/offer-video", url: "https://v.example/", title: "x".repeat(500) })).toEqual({
      type: "wdm/offer-video",
      url: "https://v.example/",
      title: "x".repeat(300),
    });
    expect(asContentMessage({ type: "wdm/offer-video", url: "https://v.example/", title: 5 })).toEqual({ type: "wdm/offer-video", url: "https://v.example/", title: "" });
    expect(asContentMessage({ type: "wdm/offer-video" })).toBeUndefined();
    expect(asPopupMessage({ type: "wdm/offer-page", url: "https://v.example/", tabId: 4, title: "T" })).toEqual({ type: "wdm/offer-page", url: "https://v.example/", tabId: 4, title: "T" });
    expect(asPopupMessage({ type: "wdm/offer-page" })).toBeUndefined();
  });
});

describe("the popup's media list", () => {
  const entry = (kind: MediaEntry["kind"], size?: number): MediaEntry => (size === undefined ? { url: "https://e/x", kind, seenAt: 0 } : { url: "https://e/x", kind, size, seenAt: 0 });

  it("leaves out page sounds and previews under 100 KiB", () => {
    // What YouTube plays for its own UI: failure.mp3 (6 KiB), open.mp3…
    expect(worthListing(entry("audio", 6 * 1024))).toBe(false);
    expect(worthListing(entry("video", MIN_LISTED_MEDIA_BYTES - 1))).toBe(false);
    expect(worthListing(entry("video", MIN_LISTED_MEDIA_BYTES))).toBe(true);
    expect(worthListing(entry("audio", 4 * 1024 * 1024))).toBe(true);
  });

  it("keeps streams and files of unknown size", () => {
    expect(worthListing(entry("hls", 900))).toBe(true);
    expect(worthListing(entry("dash", 900))).toBe(true);
    expect(worthListing(entry("audio"))).toBe(true);
  });
});
