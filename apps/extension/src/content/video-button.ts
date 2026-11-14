// SPDX-License-Identifier: GPL-3.0-or-later

// "Download with WDM" over videos, like IDM's button. Pointing at a video of
// a reasonable size (or starting one) shows a small button at its top-right
// corner; clicking it hands the page (the frame's, for an embedded player) to
// the app, whose add dialog probes it with yt-dlp and lets the user choose
// video or audio, the quality and subtitles. Nothing is downloaded from here,
// so streaming sites that play through MSE (no file to catch) work as well.
//
// It must never break the page: one element, `position: fixed` (no layout
// shift), in a closed shadow root with every style reset, and only while a
// video is pointed at. The pure helpers below decide where and whether; the
// controller only wires them to the DOM.

/** Smaller videos (previews, avatars, ads) get no button. */
export const MIN_VIDEO_WIDTH = 200;
export const MIN_VIDEO_HEIGHT = 120;
/** Below this video width the button is only its icon. */
export const COMPACT_BELOW = 360;
/** Gap between the button and the video's edges. */
export const MARGIN = 8;
/** How long the button stays after the pointer left the video. */
export const LINGER_MS = 1500;
/** How long it shows when a video starts playing without the pointer on it. */
export const FLASH_MS = 3000;

export interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** What the controller knows about a video element. */
export interface VideoFacts {
  box: Box;
  /** Playing, or has something to play (a src, a source, MSE/blob, a stream). */
  hasMedia: boolean;
  /** display:none, visibility:hidden or fully transparent. */
  hidden: boolean;
}

/** Whether a video gets a button at all. */
export function isEligible(v: VideoFacts): boolean {
  return !v.hidden && v.hasMedia && v.box.width >= MIN_VIDEO_WIDTH && v.box.height >= MIN_VIDEO_HEIGHT;
}

/** Whether a video element is playing or has something to play. */
export function videoHasMedia(video: {
  currentSrc?: string;
  src?: string;
  srcObject?: unknown;
  paused?: boolean;
  readyState?: number;
  querySelector?: (s: string) => unknown;
}): boolean {
  return Boolean(
    video.currentSrc ||
      video.src ||
      video.srcObject ||
      video.paused === false ||
      (video.readyState ?? 0) > 0 ||
      video.querySelector?.("source[src]"),
  );
}

export interface Placement {
  left: number;
  top: number;
  /** Icon only: the video is narrow. */
  compact: boolean;
}

/**
 * Where the button goes: the top-right corner of the part of the video that
 * is on screen, `MARGIN` in. `undefined` when too little of it is visible to
 * hold the button.
 */
export function place(video: Box, viewport: { width: number; height: number }, button: { width: number; height: number }): Placement | undefined {
  const left = Math.max(video.left, 0);
  const top = Math.max(video.top, 0);
  const right = Math.min(video.left + video.width, viewport.width);
  const bottom = Math.min(video.top + video.height, viewport.height);
  if (right - left < button.width + 2 * MARGIN || bottom - top < button.height + 2 * MARGIN) return undefined;
  return {
    left: Math.round(right - MARGIN - button.width),
    top: Math.round(top + MARGIN),
    compact: video.width < COMPACT_BELOW,
  };
}

/** The eligible video under a point; the smallest wins when they overlap (a preview over a player). */
export function videoAt<T>(videos: Iterable<{ el: T; facts: VideoFacts }>, x: number, y: number): T | undefined {
  let best: { el: T; area: number } | undefined;
  for (const { el, facts } of videos) {
    if (!isEligible(facts)) continue;
    const b = facts.box;
    if (x < b.left || y < b.top || x > b.left + b.width || y > b.top + b.height) continue;
    const area = b.width * b.height;
    if (!best || area < best.area) best = { el, area };
  }
  return best?.el;
}

/** Whether the button may show on this page at all. */
export function buttonAllowed(opts: { enabled: boolean; excluded: boolean; blocked: boolean }): boolean {
  return opts.enabled && !opts.excluded && !opts.blocked;
}

// ---------------------------------------------------------------- the DOM part

export interface VideoButtonOptions {
  doc: Document;
  win: Window;
  /** Checked whenever the button would show: the option, excluded sites, the store policy. */
  allowed: () => boolean;
  /** Hand the page over; resolves to whether the app got it. */
  activate: () => Promise<{ ok: boolean; error?: string }>;
  text: { label: string; sent: string; failed: string };
}

const SVG_NS = "http://www.w3.org/2000/svg";

/** The download arrow, built node by node (pages with Trusted Types refuse innerHTML). */
function icon(doc: Document): SVGSVGElement {
  const svg = doc.createElementNS(SVG_NS, "svg");
  const svgAttrs: [string, string][] = [
    ["viewBox", "0 0 24 24"],
    ["width", "16"],
    ["height", "16"],
    ["aria-hidden", "true"],
    ["focusable", "false"],
  ];
  for (const [k, v] of svgAttrs) svg.setAttribute(k, v);
  const path = doc.createElementNS(SVG_NS, "path");
  const attrs: [string, string][] = [
    ["d", "M12 4v11M7 10.5l5 5 5-5M5 20h14"],
    ["fill", "none"],
    ["stroke", "currentColor"],
    ["stroke-width", "2"],
    ["stroke-linecap", "round"],
    ["stroke-linejoin", "round"],
  ];
  for (const [k, v] of attrs) path.setAttribute(k, v);
  svg.append(path);
  return svg;
}

// Everything reset, so the page's styles cannot reach in (closed shadow root)
// and ours cannot leak out.
const STYLE = `
:host { all: initial; }
button {
  all: initial;
  box-sizing: border-box;
  display: inline-flex; align-items: center; gap: 6px;
  height: 32px; padding: 0 12px 0 10px;
  border-radius: 16px;
  background: rgba(17, 24, 39, 0.88); color: #fff;
  border: 1px solid rgba(255, 255, 255, 0.28);
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.35);
  font: 600 13px/1 system-ui, -apple-system, "Segoe UI", "Noto Sans Bengali", "Noto Sans", sans-serif;
  white-space: nowrap; cursor: pointer; pointer-events: auto;
  transition: background-color 120ms ease, opacity 120ms ease;
}
button:hover { background: rgba(37, 99, 235, 0.95); }
button:focus-visible { outline: 2px solid #60a5fa; outline-offset: 2px; }
button.compact { width: 32px; padding: 0; justify-content: center; }
button.compact .text { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }
button[data-state="sent"] { background: rgba(21, 128, 61, 0.95); }
button[data-state="failed"] { background: rgba(185, 28, 28, 0.95); }
svg { display: block; flex: none; }
@media (prefers-reduced-motion: reduce) { button { transition: none; } }
`;

/** Inline and `!important`: page rules for our tag lose to these. */
const HOST_STYLE =
  "all: initial !important; position: fixed !important; z-index: 2147483647 !important; top: 0 !important; left: 0 !important; " +
  "margin: 0 !important; padding: 0 !important; border: 0 !important; pointer-events: none !important; display: block !important;";

export class VideoButtonController {
  private readonly o: VideoButtonOptions;
  private host: HTMLElement | undefined;
  private button: HTMLButtonElement | undefined;
  private text: HTMLSpanElement | undefined;
  private current: HTMLVideoElement | undefined;
  private hideTimer: ReturnType<typeof setTimeout> | undefined;
  private follow: ReturnType<typeof setInterval> | undefined;
  private frame = 0;
  private lastPointer: { x: number; y: number } | undefined;
  private overButton = false;
  private readonly off: (() => void)[] = [];

  constructor(options: VideoButtonOptions) {
    this.o = options;
  }

  start(): void {
    const { doc, win } = this.o;
    const on = (target: EventTarget, type: string, handler: (e: Event) => void, opts: AddEventListenerOptions = { capture: true, passive: true }) => {
      target.addEventListener(type, handler, opts);
      this.off.push(() => target.removeEventListener(type, handler, opts));
    };
    on(doc, "pointermove", (e) => {
      const p = e as PointerEvent;
      this.lastPointer = { x: p.clientX, y: p.clientY };
      if (this.frame) return;
      this.frame = win.requestAnimationFrame(() => {
        this.frame = 0;
        this.onPointer();
      });
    });
    // The pointer left the page (to the browser's toolbar, another window).
    on(doc.documentElement, "pointerleave", () => {
      if (this.current && !this.overButton) this.scheduleHide(LINGER_MS);
    });
    // Starting playback shows it for a moment (media events do not bubble; capture sees them).
    on(doc, "play", (e) => {
      if (e.target instanceof HTMLVideoElement) this.flash(e.target);
    });
    on(doc, "focusin", (e) => {
      if (e.target instanceof HTMLVideoElement) this.flash(e.target);
    });
    on(win, "scroll", () => this.reposition());
    on(win, "resize", () => this.reposition());
    on(doc, "fullscreenchange", () => this.reposition());
    on(doc, "webkitfullscreenchange", () => this.reposition());
    // Single-page sites (YouTube's own navigation event, history changes): start over.
    on(win, "yt-navigate-finish", () => this.hide());
    on(win, "popstate", () => this.hide());
    on(win, "pagehide", () => this.hide());
  }

  stop(): void {
    for (const off of this.off.splice(0)) off();
    this.hide();
    this.host?.remove();
    this.host = undefined;
  }

  private facts(v: HTMLVideoElement): VideoFacts {
    const r = v.getBoundingClientRect();
    const style = this.o.win.getComputedStyle(v);
    return {
      box: { left: r.left, top: r.top, width: r.width, height: r.height },
      hasMedia: videoHasMedia(v),
      hidden: style.display === "none" || style.visibility === "hidden" || Number(style.opacity) === 0,
    };
  }

  private onPointer(): void {
    const p = this.lastPointer;
    if (!p) return;
    if (this.overButton) return;
    if (!this.o.allowed()) {
      this.hide();
      return;
    }
    const videos = Array.from(this.o.doc.getElementsByTagName("video"), (el) => ({ el, facts: this.facts(el) }));
    const v = videoAt(videos, p.x, p.y);
    if (v) {
      this.show(v);
      this.scheduleHide(undefined);
    } else if (this.current) {
      this.scheduleHide(LINGER_MS);
    }
  }

  private flash(v: HTMLVideoElement): void {
    if (!this.o.allowed() || !isEligible(this.facts(v))) return;
    this.show(v);
    if (!this.overButton) this.scheduleHide(FLASH_MS);
  }

  private scheduleHide(ms: number | undefined): void {
    clearTimeout(this.hideTimer);
    this.hideTimer = ms === undefined ? undefined : setTimeout(() => this.hide(), ms);
  }

  private ensureHost(): void {
    if (this.host) return;
    const { doc } = this.o;
    const host = doc.createElement("wdm-video-button");
    host.setAttribute("style", HOST_STYLE);
    const root = host.attachShadow({ mode: "closed" });
    const style = doc.createElement("style");
    style.textContent = STYLE;
    const button = doc.createElement("button");
    button.type = "button";
    button.append(icon(doc));
    const text = doc.createElement("span");
    text.className = "text";
    button.append(text);
    button.addEventListener("pointerenter", () => {
      this.overButton = true;
      this.scheduleHide(undefined);
    });
    button.addEventListener("pointerleave", () => {
      this.overButton = false;
      this.scheduleHide(LINGER_MS);
    });
    button.addEventListener("focus", () => this.scheduleHide(undefined));
    button.addEventListener("blur", () => this.scheduleHide(LINGER_MS));
    button.addEventListener("click", (e) => {
      // The player under it must not take the click (pause, open the video).
      e.preventDefault();
      e.stopPropagation();
      void this.activate();
    });
    for (const type of ["pointerdown", "mousedown", "mouseup", "dblclick"]) {
      button.addEventListener(type, (e) => e.stopPropagation());
    }
    root.append(style, button);
    this.host = host;
    this.button = button;
    this.text = text;
    this.setState(undefined);
  }

  private setState(state: "sent" | "failed" | undefined, detail?: string): void {
    if (!this.button || !this.text) return;
    const { label, sent, failed } = this.o.text;
    const words = state === "sent" ? sent : state === "failed" ? failed : label;
    this.text.textContent = words;
    this.button.setAttribute("aria-label", words);
    this.button.title = detail ? `${words}: ${detail}` : words;
    if (state) this.button.dataset["state"] = state;
    else delete this.button.dataset["state"];
  }

  private async activate(): Promise<void> {
    const button = this.button;
    if (!button || button.dataset["busy"]) return;
    button.dataset["busy"] = "1";
    let reply: { ok: boolean; error?: string };
    try {
      reply = await this.o.activate();
    } catch (e) {
      reply = { ok: false, error: e instanceof Error ? e.message : String(e) };
    }
    delete button.dataset["busy"];
    this.setState(reply.ok ? "sent" : "failed", reply.ok ? undefined : reply.error);
    setTimeout(() => this.setState(undefined), 2500);
    this.scheduleHide(FLASH_MS);
  }

  private show(v: HTMLVideoElement): void {
    this.ensureHost();
    const host = this.host;
    if (!host) return;
    if (this.current !== v) {
      this.current = v;
      this.setState(undefined);
    }
    this.mount();
    this.reposition();
    clearInterval(this.follow);
    // Players move (theater mode, mini players): follow while shown.
    this.follow = setInterval(() => this.reposition(), 500);
  }

  /** In fullscreen only the fullscreen element's subtree is drawn: go inside it. */
  private mount(): void {
    const { doc } = this.o;
    const host = this.host;
    if (!host) return;
    const fs = doc.fullscreenElement ?? (doc as { webkitFullscreenElement?: Element | null }).webkitFullscreenElement ?? null;
    const parent = fs && !(fs instanceof HTMLMediaElement) ? fs : doc.documentElement;
    if (host.parentNode !== parent) parent.append(host);
  }

  private reposition(): void {
    const v = this.current;
    const host = this.host;
    const button = this.button;
    if (!v || !host || !button) return;
    const { doc, win } = this.o;
    const fs = doc.fullscreenElement;
    if (!v.isConnected || !this.o.allowed() || fs === v) {
      this.hide();
      return;
    }
    this.mount();
    const facts = this.facts(v);
    if (!isEligible(facts)) {
      this.hide();
      return;
    }
    const compact = facts.box.width < COMPACT_BELOW;
    button.classList.toggle("compact", compact);
    host.style.setProperty("visibility", "hidden", "important");
    const size = { width: button.offsetWidth || (compact ? 32 : 170), height: button.offsetHeight || 32 };
    const spot = place(facts.box, { width: win.innerWidth, height: win.innerHeight }, size);
    if (!spot) {
      host.style.setProperty("visibility", "hidden", "important");
      return;
    }
    host.style.setProperty("transform", `translate(${spot.left}px, ${spot.top}px)`, "important");
    host.style.setProperty("visibility", "visible", "important");
  }

  hide(): void {
    clearTimeout(this.hideTimer);
    clearInterval(this.follow);
    this.hideTimer = undefined;
    this.follow = undefined;
    this.current = undefined;
    this.overButton = false;
    this.host?.remove();
  }
}
