// SPDX-License-Identifier: GPL-3.0-or-later
// "Download with WDM" over videos: which videos get the button, where it
// goes, and when it may show at all.
import { describe, expect, it } from "vitest";

import {
  buttonAllowed,
  COMPACT_BELOW,
  isEligible,
  MARGIN,
  MIN_VIDEO_HEIGHT,
  MIN_VIDEO_WIDTH,
  place,
  videoAt,
  videoHasMedia,
  type VideoFacts,
} from "../src/content/video-button";
import { isPolicyBlocked, STORE_BLOCKED_SITES } from "../src/shared/policy";
import { DEFAULT_SETTINGS, normalizeSettings } from "../src/shared/settings";
import { isSiteExcluded } from "../src/shared/url";

const facts = (box: VideoFacts["box"], extra: Partial<VideoFacts> = {}): VideoFacts => ({ box, hasMedia: true, hidden: false, ...extra });
const viewport = { width: 1280, height: 800 };
const button = { width: 170, height: 32 };

describe("which videos get a button", () => {
  it("needs a reasonable size", () => {
    expect(isEligible(facts({ left: 0, top: 0, width: MIN_VIDEO_WIDTH, height: MIN_VIDEO_HEIGHT }))).toBe(true);
    expect(isEligible(facts({ left: 0, top: 0, width: MIN_VIDEO_WIDTH - 1, height: 400 }))).toBe(false);
    expect(isEligible(facts({ left: 0, top: 0, width: 640, height: MIN_VIDEO_HEIGHT - 1 }))).toBe(false);
  });

  it("needs something to play, and to be visible", () => {
    const box = { left: 0, top: 0, width: 640, height: 360 };
    expect(isEligible(facts(box, { hasMedia: false }))).toBe(false);
    expect(isEligible(facts(box, { hidden: true }))).toBe(false);
  });

  it("counts playing, a source, MSE (blob) and streams as something to play", () => {
    expect(videoHasMedia({})).toBe(false);
    expect(videoHasMedia({ currentSrc: "", src: "", paused: true, readyState: 0 })).toBe(false);
    expect(videoHasMedia({ currentSrc: "blob:https://www.youtube.com/0b1c" })).toBe(true);
    expect(videoHasMedia({ src: "https://cdn.example/v.mp4" })).toBe(true);
    expect(videoHasMedia({ srcObject: {} })).toBe(true);
    expect(videoHasMedia({ paused: false })).toBe(true);
    expect(videoHasMedia({ readyState: 1 })).toBe(true);
    expect(videoHasMedia({ querySelector: (s: string) => (s === "source[src]" ? {} : null) })).toBe(true);
  });
});

describe("where the button goes", () => {
  it("sits in the top-right corner, inside the video", () => {
    const spot = place({ left: 100, top: 50, width: 854, height: 480 }, viewport, button);
    expect(spot).toEqual({ left: 100 + 854 - MARGIN - button.width, top: 50 + MARGIN, compact: false });
  });

  it("shrinks to its icon on narrow videos", () => {
    const spot = place({ left: 10, top: 10, width: COMPACT_BELOW - 1, height: 200 }, viewport, { width: 32, height: 32 });
    expect(spot?.compact).toBe(true);
    expect(place({ left: 10, top: 10, width: COMPACT_BELOW, height: 200 }, viewport, button)?.compact).toBe(false);
  });

  it("follows the visible part of a video scrolled half out of view", () => {
    // Top cut off by scrolling: the button stays on screen, at the top of what shows.
    const spot = place({ left: 0, top: -200, width: 800, height: 450 }, viewport, button);
    expect(spot).toEqual({ left: 800 - MARGIN - button.width, top: MARGIN, compact: false });
    // Wider than the window: the right edge of the window.
    expect(place({ left: 0, top: 0, width: 2000, height: 450 }, viewport, button)?.left).toBe(viewport.width - MARGIN - button.width);
  });

  it("hides when too little of the video is on screen", () => {
    expect(place({ left: 0, top: 780, width: 800, height: 450 }, viewport, button)).toBeUndefined();
    expect(place({ left: -700, top: 0, width: 800, height: 450 }, viewport, button)).toBeUndefined();
  });

  it("picks the video under the pointer, the smaller one when they overlap", () => {
    const player = { el: "player", facts: facts({ left: 0, top: 0, width: 1000, height: 560 }) };
    const preview = { el: "preview", facts: facts({ left: 600, top: 300, width: 320, height: 180 }) };
    const tiny = { el: "tiny", facts: facts({ left: 10, top: 10, width: 100, height: 60 }) };
    expect(videoAt([player, preview, tiny], 700, 350)).toBe("preview");
    expect(videoAt([player, preview, tiny], 100, 100)).toBe("player");
    expect(videoAt([player, preview, tiny], 20, 20)).toBe("player"); // the tiny one gets none
    expect(videoAt([player], 1100, 100)).toBeUndefined();
  });
});

describe("when the button may show", () => {
  it("is on by default and follows the option", () => {
    expect(DEFAULT_SETTINGS.videoButton).toBe(true);
    expect(normalizeSettings({}).videoButton).toBe(true);
    expect(normalizeSettings({ videoButton: false }).videoButton).toBe(false);
    expect(normalizeSettings({ videoButton: "no" }).videoButton).toBe(true);
    expect(buttonAllowed({ enabled: false, excluded: false, blocked: false })).toBe(false);
    expect(buttonAllowed({ enabled: true, excluded: false, blocked: false })).toBe(true);
  });

  it("disappears on excluded sites", () => {
    const excluded = isSiteExcluded(["https://www.example.com/talk"], ["example.com"]);
    expect(excluded).toBe(true);
    expect(buttonAllowed({ enabled: true, excluded, blocked: false })).toBe(false);
  });

  it("never shows on the store-blocked sites with the store's list", () => {
    for (const url of ["https://www.youtube.com/watch?v=x", "https://www.youtube-nocookie.com/embed/x", "https://www.netflix.com/watch/1"]) {
      const blocked = isPolicyBlocked([url], STORE_BLOCKED_SITES);
      expect(blocked, url).toBe(true);
      expect(buttonAllowed({ enabled: true, excluded: false, blocked })).toBe(false);
    }
    // The GitHub build applies no list.
    expect(isPolicyBlocked(["https://www.youtube.com/watch?v=x"], [])).toBe(false);
    expect(isPolicyBlocked(["https://vimeo.com/1"], STORE_BLOCKED_SITES)).toBe(false);
  });
});
