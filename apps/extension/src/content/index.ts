// SPDX-License-Identifier: GPL-3.0-or-later

// Content script (every http(s) page and frame):
//  * Alt+click → tell the background to leave the next download to the browser
//    (or, per the options, to send it to WDM whatever the rules say).
//  * Clicking a download-looking link → its text, for self-healing.
//  * Hovering a download-looking link for ~150 ms → ask for a preconnect.
//  * Answer "collect links" for the context menus.
//  * "Download with WDM" over videos (video-button.ts): hands the page to the
//    app's media picker, unless turned off, the site is excluded, or (store
//    build) the site is store-blocked.

import { t } from "../shared/i18n";
import { looksLikeDownloadLink, PreconnectThrottle } from "../shared/links";
import { MAX_TITLE, type ActionReply, type CollectLinksMessage, type CollectLinksReply, type ContentMessage } from "../shared/messages";
import { blockedByStorePolicy } from "../shared/policy";
import { DEFAULT_SETTINGS, normalizeSettings, SETTINGS_KEY, type Settings } from "../shared/settings";
import { isSiteExcluded } from "../shared/url";
import { collectLinks, linkFromTarget } from "./links";
import { buttonAllowed, VideoButtonController } from "./video-button";

const HOVER_MS = 150;
const MARK = Symbol.for("wdm.content");

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
        // Alt+click: the browser keeps it, or WDM takes it, as set in the options.
        if (settings.altClick === "browser" && settings.captureEnabled) send({ type: "wdm/bypass-next" });
        else if (settings.altClick === "wdm") send({ type: "wdm/force-next" });
      }
      // The link's text lets self-healing find the same link again later.
      const link = linkFromTarget(event.target);
      if (link && looksLikeDownloadLink(link.href, settings.fileTypes, link.hasAttribute("download"))) {
        const text = (link.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 200);
        if (text) send({ type: "wdm/link-click", url: link.href, text });
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
        if (throttle.allow(link.href, Date.now())) send({ type: "wdm/preconnect", url: link.href });
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

  // The button over videos. The policy is read at each show: settings change, and single-page
  // sites change their address without reloading this script.
  const videoButton = new VideoButtonController({
    doc: document,
    win: window,
    allowed: () =>
      buttonAllowed({
        enabled: settings.videoButton,
        excluded: isSiteExcluded([location.href], settings.excludedSites),
        blocked: blockedByStorePolicy(location.href),
      }),
    activate: async () => {
      const message: ContentMessage = { type: "wdm/offer-video", url: location.href, title: document.title.slice(0, MAX_TITLE) };
      try {
        const reply = (await api.runtime.sendMessage(message)) as ActionReply | undefined;
        return reply ?? { ok: false, error: t("videoButtonFailed") };
      } catch (e) {
        return { ok: false, error: e instanceof Error ? e.message : String(e) };
      }
    },
    text: { label: t("videoButton"), sent: t("videoButtonSent"), failed: t("videoButtonFailed") },
  });
  videoButton.start();

  api.runtime.onMessage.addListener((message: unknown, _sender, sendResponse) => {
    const m = message as Partial<CollectLinksMessage> | null;
    if (m?.type !== "wdm/collect-links" || (m.scope !== "all" && m.scope !== "selection")) return false;
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
