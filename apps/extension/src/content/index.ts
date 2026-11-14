// SPDX-License-Identifier: GPL-3.0-or-later

// Content script (every http(s) page and frame):
//  * Alt+click → tell the background to leave the next download to the browser
//    (or, per the options, to send it to Whoosh whatever the rules say).
//  * Clicking a download-looking link → its text, for self-healing.
//  * Hovering a download-looking link for ~150 ms → ask for a preconnect.
//  * Answer "collect links" for the context menus.

import { looksLikeDownloadLink, PreconnectThrottle } from "../shared/links";
import type { CollectLinksMessage, CollectLinksReply, ContentMessage } from "../shared/messages";
import { DEFAULT_SETTINGS, normalizeSettings, SETTINGS_KEY, type Settings } from "../shared/settings";
import { collectLinks, linkFromTarget } from "./links";

const HOVER_MS = 150;
const MARK = Symbol.for("whoosh.content");

type Api = typeof chrome;

function start(api: Api): void {
  let settings: Settings = { ...DEFAULT_SETTINGS };
  void api.storage.local
    .get(SETTINGS_KEY)
    .then((items) => {
      settings = normalizeSettings(items[SETTINGS_KEY]);
    })
    .catch(() => {});
  api.storage.onChanged.addListener((changes, area) => {
    const change = changes[SETTINGS_KEY];
    if (area === "local" && change) settings = normalizeSettings(change.newValue);
  });

  const send = (message: ContentMessage) => {
    // The extension may have been reloaded under this page; ignore that.
    try {
      void api.runtime.sendMessage(message).catch(() => {});
    } catch {
      // "Extension context invalidated"
    }
  };

  // pointerdown comes before the navigation that starts the download.
  window.addEventListener(
    "pointerdown",
    (event) => {
      if (event.button !== 0) return;
      if (event.altKey) {
        // Alt+click: the browser keeps it, or Whoosh takes it, as set in the options.
        if (settings.altClick === "browser" && settings.captureEnabled) send({ type: "whoosh/bypass-next" });
        else if (settings.altClick === "whoosh") send({ type: "whoosh/force-next" });
      }
      // The link's text lets self-healing find the same link again later.
      const link = linkFromTarget(event.target);
      if (link && looksLikeDownloadLink(link.href, settings.fileTypes, link.hasAttribute("download"))) {
        const text = (link.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 200);
        if (text) send({ type: "whoosh/link-click", url: link.href, text });
      }
    },
    { capture: true, passive: true },
  );

  // Hover intent → preconnect.
  const throttle = new PreconnectThrottle();
  let hovered: HTMLAnchorElement | HTMLAreaElement | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const cancel = () => {
    clearTimeout(timer);
    timer = undefined;
    hovered = undefined;
  };

  document.addEventListener(
    "pointerover",
    (event) => {
      const link = linkFromTarget(event.target);
      if (link === hovered) return;
      cancel();
      if (!link || !settings.preconnect) return;
      if (!looksLikeDownloadLink(link.href, settings.fileTypes, link.hasAttribute("download"))) return;
      hovered = link;
      timer = setTimeout(() => {
        if (hovered !== link) return;
        if (throttle.allow(link.href, Date.now())) send({ type: "whoosh/preconnect", url: link.href });
      }, HOVER_MS);
    },
    { capture: true, passive: true },
  );
  document.addEventListener(
    "pointerout",
    (event) => {
      if (!hovered) return;
      const to = event.relatedTarget;
      if (!(to instanceof Node) || !hovered.contains(to)) cancel();
    },
    { capture: true, passive: true },
  );

  api.runtime.onMessage.addListener((message: unknown, _sender, sendResponse) => {
    const m = message as Partial<CollectLinksMessage> | null;
    if (m?.type !== "whoosh/collect-links" || (m.scope !== "all" && m.scope !== "selection")) return false;
    const reply: CollectLinksReply = { links: collectLinks(document, m.scope) };
    sendResponse(reply);
    return false;
  });
}

// The context menu may inject this script into a page that already has it.
const g = globalThis as { browser?: Api; chrome?: Api; [MARK]?: boolean };
if (!g[MARK]) {
  g[MARK] = true;
  const api = g.browser ?? g.chrome;
  if (api?.runtime?.id) start(api);
}
