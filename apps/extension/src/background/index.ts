// SPDX-License-Identifier: GPL-3.0-or-later

// Background script (Chromium: service worker; Firefox: event page).
// Every listener is registered synchronously at the top level so the browser
// can wake the script for it.

import { PreconnectThrottle, selectLinks } from "../shared/links";
import {
  asContentMessage,
  asPopupMessage,
  type ActionReply,
  type ContentMessage,
  type MediaEntry,
  type PopupMessage,
  type PopupState,
} from "../shared/messages";
import { blockedByStorePolicy } from "../shared/policy";
import { loadSettings, normalizeSettings, SETTINGS_KEY, updateSettings, type Settings } from "../shared/settings";
import { hostMatches, hostOf, isSiteExcluded, normalizeSitePattern } from "../shared/url";
import { NativeMessagingTransport, type NativePort } from "../transport/native";
import { isUnavailable } from "../transport/rpc";
import { canHoldDownloads, downloadControl, getApi, isFirefox, toCandidate } from "./browser";
import { buildAddParams, cookiesFor, privateCookieStore } from "./capture/context";
import { CaptureController, type CaptureResult } from "./capture/controller";
import type { CandidateDownload } from "./capture/rules";
import { BypassTracker, RecentClicks, RequestMethodTracker } from "./capture/trackers";
import { Coalescer, listUnfinished, RisingEdge } from "./catch-up";
import { createPageReader, openPage } from "./healing/pages";
import { collectLinks, createMenus, MENU } from "./menus";
import { LinkHealer, type HealAsk, type Provenance } from "./self-healing";
import { classifyMedia, header, sizeFromHeaders, TabMediaStore } from "./sniffer";

const api = getApi();
const firefox = isFirefox(api);
const HTTP_URLS = ["http://*/*", "https://*/*"];

const transport = new NativeMessagingTransport({
  connectNative: (name) => api.runtime.connectNative(name) as unknown as NativePort,
  lastError: () => api.runtime.lastError?.message,
  // Keep the event stream while downloads we handed over may still need us
  // (an open port also keeps the service worker alive).
  holdOpen: () => healer.hasActive(),
});
const cookieSource = { getAll: (details: { url: string; storeId?: string }) => api.cookies.getAll(details) };

// ------------------------------------------------------------------ settings

let settingsCache: Settings | undefined;

async function settings(): Promise<Settings> {
  settingsCache ??= await loadSettings(api.storage.local);
  return settingsCache;
}

async function changeSettings(patch: Partial<Settings>): Promise<Settings> {
  settingsCache = await updateSettings(api.storage.local, patch);
  void updateBadge(settingsCache);
  return settingsCache;
}

api.storage.onChanged.addListener((changes, area) => {
  const change = changes[SETTINGS_KEY];
  if (area !== "local" || !change) return;
  settingsCache = normalizeSettings(change.newValue);
  void updateBadge(settingsCache);
});

let openAsks = 0;

async function updateBadge(s: Settings): Promise<void> {
  if (openAsks > 0) {
    // Self-healing needs the user: the popup says what.
    await api.action.setBadgeBackgroundColor({ color: "#d97706" });
    await api.action.setBadgeText({ text: "!" });
    return;
  }
  await api.action.setBadgeBackgroundColor({ color: "#6b7280" });
  await api.action.setBadgeText({ text: s.captureEnabled ? "" : "off" });
}

// ------------------------------------------------------------------ self-healing

const healer = new LinkHealer({
  store: api.storage.local,
  daemon: transport,
  cookies: cookieSource,
  pageLinks: createPageReader(api, firefox),
  openPage: (pageUrl, storeId) => openPage(api, firefox, pageUrl, storeId),
  settings,
  blocked: (urls) => blockedByStorePolicy(...urls),
  userAgent: navigator.userAgent,
  now: () => Date.now(),
  onAsksChanged: (asks: HealAsk[]) => {
    openAsks = asks.length;
    void settings().then(updateBadge);
  },
});

function remember(provenance: Provenance): void {
  void healer.remember(provenance).catch((e: unknown) => console.warn("whoosh: could not remember a download", e));
}

// Catch up on what happened while no port was open: downloads that finished
// or were removed are forgotten, expired ones are healed.
const catchUp = new Coalescer(
  async () => {
    if ((await healer.ids()).length === 0) return;
    const summaries = await listUnfinished(transport);
    const outcomes = await healer.sync(summaries);
    for (const outcome of outcomes) console.debug("whoosh: self-healing (catch-up)", outcome.status);
  },
  (error) => console.debug("whoosh: catch-up skipped:", error instanceof Error ? error.message : String(error)),
);
const daemonUp = new RisingEdge();
transport.onOpen(() => void catchUp.trigger());
transport.onStatus((status) => {
  if (daemonUp.update(status.daemonRunning)) void catchUp.trigger();
});

/** After a browser restart: reopen the port if a remembered download may need us. */
async function resumeWatching(): Promise<void> {
  const asks = await healer.asks();
  openAsks = asks.length;
  if ((await healer.ids()).length > 0) await transport.status();
}

// ------------------------------------------------------------------ capture

const bypass = new BypassTracker();
const force = new BypassTracker();
const methods = new RequestMethodTracker();
const clicks = new RecentClicks();
let lastCapture: PopupState["lastCapture"];

function describe(result: CaptureResult): string {
  if (result.outcome === "sent") return "sent to whoosh";
  if (result.outcome === "healed") return "continued an expired whoosh download";
  if (result.outcome === "skipped") return `left to the browser (${result.reason})`;
  return `kept by the browser (${result.reason})`;
}

function report(candidate: CandidateDownload, result: CaptureResult): void {
  if (result.outcome !== "skipped") {
    lastCapture = { url: candidate.url, outcome: describe(result), at: Date.now() };
  }
  console.debug("whoosh: download", candidate.id, describe(result));
  if (result.outcome === "sent" && !candidate.incognito) {
    // Private-window downloads are never remembered.
    const now = Date.now();
    const provenance: Provenance = { daemonId: result.daemonId, url: candidate.url, capturedAt: now };
    if (candidate.originalUrl) provenance.linkUrl = candidate.originalUrl;
    if (candidate.referrer) provenance.originPageUrl = candidate.referrer;
    if (candidate.filename) provenance.filename = candidate.filename;
    if (candidate.totalBytes) provenance.totalBytes = candidate.totalBytes;
    if (candidate.cookieStoreId) provenance.cookieStoreId = candidate.cookieStoreId;
    const text = clicks.textFor([candidate.originalUrl, candidate.url], now);
    if (text) provenance.linkText = text;
    remember(provenance);
  } else if (result.outcome === "kept" && "late" in result && result.late) {
    void result.late.then((late) => console.debug("whoosh: late answer for download", candidate.id, late.outcome));
  }
}

const capture = new CaptureController({
  daemon: transport,
  downloads: downloadControl(api),
  cookies: cookieSource,
  settings,
  bypass,
  force,
  methods,
  blocked: (urls) => blockedByStorePolicy(...urls),
  expired: healer,
  userAgent: navigator.userAgent,
  isFirefox: firefox,
  now: () => Date.now(),
  report,
});

if (canHoldDownloads(api)) {
  // Chromium: the download waits until `suggest` is called.
  api.downloads.onDeterminingFilename.addListener((item, suggest) => {
    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      suggest();
    };
    void capture.onBrowserDownload(toCandidate(item), release);
    return true;
  });
} else {
  // Firefox: the download is already running; it continues until the daemon confirms.
  api.downloads.onCreated.addListener((item) => {
    void capture.onBrowserDownload(toCandidate(item), () => {});
  });
}

// Remember non-GET navigations so their downloads stay in the browser, and
// start a fresh media list when a tab navigates.
api.webRequest.onBeforeRequest.addListener(
  (details) => {
    methods.record(details.url, details.method, Date.now());
    if (details.type === "main_frame") media.clear(details.tabId);
    return undefined;
  },
  { urls: HTTP_URLS, types: ["main_frame", "sub_frame"] },
);

// ------------------------------------------------------------------ sending links

interface SendOptions {
  referrer?: string | undefined;
  /** For a single explicit link: if whoosh cannot take it, let the browser download it. */
  fallbackToBrowser: boolean;
  incognito?: boolean | undefined;
  cookieStoreId?: string | undefined;
}

async function sendLinks(urls: readonly string[], opts: SendOptions): Promise<ActionReply> {
  if (blockedByStorePolicy(opts.referrer, ...urls)) {
    return { ok: false, error: "Whoosh from the browser store does not download from this site." };
  }
  const storeId = opts.cookieStoreId ?? (opts.incognito ? privateCookieStore(firefox) : undefined);
  let failed = 0;
  let lastError = "";
  for (const url of urls) {
    try {
      const params = buildAddParams({
        url,
        cookies: await cookiesFor(cookieSource, url, storeId),
        referrer: opts.referrer,
        userAgent: navigator.userAgent,
      });
      const { id } = await transport.call("download.add", params);
      if (!opts.incognito) {
        const provenance: Provenance = { daemonId: id, url, linkUrl: url, capturedAt: Date.now() };
        if (opts.referrer) provenance.originPageUrl = opts.referrer;
        if (opts.cookieStoreId) provenance.cookieStoreId = opts.cookieStoreId;
        remember(provenance);
      }
    } catch (error) {
      failed++;
      lastError = error instanceof Error ? error.message : String(error);
      if (opts.fallbackToBrowser && urls.length === 1) {
        await api.downloads.download({ url });
        return { ok: true };
      }
      if (isUnavailable(error)) break;
    }
  }
  if (urls.length > 0) {
    lastCapture = {
      url: urls[0] ?? "",
      outcome: failed === 0 ? `sent ${urls.length} link(s) to whoosh` : `failed: ${lastError}`,
      at: Date.now(),
    };
  }
  return failed === 0 ? { ok: true } : { ok: false, error: lastError };
}

// ------------------------------------------------------------------ context menus

api.runtime.onInstalled.addListener(() => {
  void createMenus(api);
  void settings().then(updateBadge);
});
api.runtime.onStartup.addListener(() => {
  void createMenus(api);
  void resumeWatching()
    .catch(() => {})
    .then(() => settings().then(updateBadge));
});

api.contextMenus.onClicked.addListener((info, tab) => {
  void onMenu(info, tab);
});

async function onMenu(info: chrome.contextMenus.OnClickData, tab: chrome.tabs.Tab | undefined): Promise<void> {
  const tabStore = (tab as { cookieStoreId?: string } | undefined)?.cookieStoreId;
  const base = {
    referrer: info.frameUrl ?? info.pageUrl ?? tab?.url,
    incognito: tab?.incognito,
    cookieStoreId: tabStore,
  };
  if (blockedByStorePolicy(info.pageUrl, info.frameUrl, info.linkUrl)) {
    lastCapture = { url: info.linkUrl ?? info.pageUrl ?? "", outcome: "not available on this site", at: Date.now() };
    return;
  }
  if (info.menuItemId === MENU.link && info.linkUrl) {
    await sendLinks([info.linkUrl], { ...base, fallbackToBrowser: true });
    return;
  }
  if ((info.menuItemId === MENU.selection || info.menuItemId === MENU.page) && tab?.id !== undefined) {
    const scope = info.menuItemId === MENU.selection ? "selection" : "all";
    try {
      const links = await collectLinks(api, tab.id, info.frameId ?? 0, scope);
      const urls = selectLinks(links, scope, (await settings()).fileTypes);
      await sendLinks(urls, { ...base, fallbackToBrowser: false });
    } catch (error) {
      lastCapture = { url: tab.url ?? "", outcome: `could not read links: ${String(error)}`, at: Date.now() };
    }
  }
}

// ------------------------------------------------------------------ media sniffer

let persistTimer: ReturnType<typeof setTimeout> | undefined;
const media = new TabMediaStore((snapshot) => {
  // storage.session survives the service worker going to sleep.
  if (!api.storage.session) return;
  clearTimeout(persistTimer);
  persistTimer = setTimeout(() => void api.storage.session.set({ media: snapshot }), 250);
});
void api.storage.session?.get("media").then((items) => media.load(items["media"]));

api.webRequest.onHeadersReceived.addListener(
  (details) => {
    if (details.tabId < 0 || settingsCache?.sniffMedia === false) return undefined;
    // Store build: nothing is sniffed on blocked sites, nor their media hosts.
    if (blockedByStorePolicy(details.url, details.initiator, (details as { documentUrl?: string }).documentUrl)) {
      return undefined;
    }
    const contentType = header(details.responseHeaders, "content-type");
    const kind = classifyMedia(details.url, contentType);
    if (!kind) return undefined;
    const entry: MediaEntry = { url: details.url, kind, seenAt: Date.now() };
    const mime = contentType?.split(";")[0]?.trim();
    if (mime) entry.mime = mime;
    const size = sizeFromHeaders(details.statusCode, details.responseHeaders);
    if (size !== undefined) entry.size = size;
    media.add(details.tabId, entry);
    return undefined;
  },
  { urls: HTTP_URLS, types: ["main_frame", "sub_frame", "media", "xmlhttprequest", "object", "other"] },
  ["responseHeaders"],
);
api.tabs.onRemoved.addListener((tabId) => media.clear(tabId));

// ------------------------------------------------------------------ messages

const preconnect = new PreconnectThrottle();

async function onContentMessage(message: ContentMessage): Promise<void> {
  const now = Date.now();
  if (message.type === "whoosh/bypass-next") {
    bypass.arm(now);
    return;
  }
  if (message.type === "whoosh/force-next") {
    force.arm(now);
    return;
  }
  if (message.type === "whoosh/link-click") {
    clicks.record(message.url, message.text, now);
    return;
  }
  const s = await settings();
  if (!s.preconnect || !transport.healthy || blockedByStorePolicy(message.url)) return;
  const origin = preconnect.allow(message.url, now);
  if (origin) await transport.call("net.preconnect", { url: origin }).catch(() => {});
}

async function onPopupMessage(message: PopupMessage): Promise<PopupState | ActionReply> {
  switch (message.type) {
    case "whoosh/popup-state":
      return popupState(message.tabId, message.pageUrl);
    case "whoosh/set-capture":
      await changeSettings({ captureEnabled: message.enabled });
      return { ok: true };
    case "whoosh/set-site-excluded": {
      const host = normalizeSitePattern(message.host);
      if (!host) return { ok: false, error: "not a host name" };
      const current = (await settings()).excludedSites;
      const excludedSites = message.excluded
        ? [...current, host]
        : current.filter((pattern) => !hostMatches(host, pattern));
      await changeSettings({ excludedSites });
      return { ok: true };
    }
    case "whoosh/download-media":
      return sendLinks([message.url], { referrer: message.pageUrl, fallbackToBrowser: false });
    case "whoosh/heal-answer": {
      const outcome = await healer.act(message.id, message.answer);
      if (outcome?.status === "failed" && outcome.reason === "daemon-refused") {
        return { ok: false, error: "Whoosh did not accept that address." };
      }
      return { ok: true };
    }
  }
}

async function popupState(tabId: number | undefined, pageUrl: string | undefined): Promise<PopupState> {
  const [status, s, heals] = await Promise.all([transport.status(), settings(), healer.asks()]);
  const pageBlocked = blockedByStorePolicy(pageUrl);
  const state: PopupState = {
    status,
    captureEnabled: s.captureEnabled,
    siteExcluded: false,
    media: tabId === undefined || pageBlocked ? [] : media.list(tabId),
    heals,
    pageBlocked,
    altClick: s.altClick,
  };
  if (status.daemonRunning) {
    try {
      const stats = await transport.call("stats.get", undefined);
      state.active = stats.active;
      state.queued = stats.queued;
    } catch {
      // The popup shows the status without counts.
    }
  }
  const host = hostOf(pageUrl);
  if (host) {
    state.host = host;
    state.siteExcluded = isSiteExcluded([pageUrl], s.excludedSites);
  }
  if (lastCapture) state.lastCapture = lastCapture;
  return state;
}

const extensionOrigin = api.runtime.getURL("");

api.runtime.onMessage.addListener((message: unknown, sender, sendResponse) => {
  if (sender.id !== api.runtime.id) return false;

  const content = asContentMessage(message);
  if (content) {
    void onContentMessage(content);
    return false;
  }

  // Popup commands are accepted from extension pages only, never from
  // content scripts (whose sender URL is the web page's).
  const popup = asPopupMessage(message);
  if (popup && (sender.url ?? "").startsWith(extensionOrigin)) {
    onPopupMessage(popup).then(sendResponse, (error: unknown) =>
      sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) }),
    );
    return true;
  }
  return false;
});

// ------------------------------------------------------------------ daemon events

transport.on("event.download.state", (event) => {
  void healer.onState(event).then(
    (outcome) => {
      if (outcome) console.debug("whoosh: self-healing", event.id, outcome.status);
    },
    (error: unknown) => console.warn("whoosh: self-healing failed", event.id, error),
  );
});
transport.on("event.download.removed", (event) => {
  void healer.onRemoved(event.ids);
});
