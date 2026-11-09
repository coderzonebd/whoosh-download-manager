// SPDX-License-Identifier: GPL-3.0-or-later

// One code base for Chromium and Firefox. Both offer promise-returning
// WebExtension APIs in Manifest V3; Firefox under `browser`, Chromium under
// `chrome`. The types come from @types/chrome; Firefox-only fields are read
// defensively.

import type { CandidateDownload } from "./capture/rules";
import type { BrowserDownloadState, DownloadControl } from "./capture/handoff";

export type Api = typeof chrome;

export function getApi(): Api {
  const g = globalThis as { browser?: Api; chrome?: Api };
  const api = g.browser ?? g.chrome;
  if (!api) throw new Error("no WebExtension API in this context");
  return api;
}

export function isFirefox(api: Api): boolean {
  return api.runtime.getURL("").startsWith("moz-extension:");
}

/** Chromium can hold a download until we decide; Firefox only tells us after it started. */
export function canHoldDownloads(api: Api): boolean {
  return typeof api.downloads.onDeterminingFilename?.addListener === "function";
}

export function toCandidate(item: chrome.downloads.DownloadItem): CandidateDownload {
  const finalUrl = item.finalUrl || item.url;
  const candidate: CandidateDownload = { id: item.id, url: finalUrl, incognito: item.incognito === true };
  if (item.url && item.url !== finalUrl) candidate.originalUrl = item.url;
  if (item.referrer) candidate.referrer = item.referrer;
  if (item.filename) candidate.filename = item.filename;
  if (item.mime) candidate.mime = item.mime;
  if (item.totalBytes > 0) candidate.totalBytes = item.totalBytes;
  else if (item.fileSize > 0) candidate.totalBytes = item.fileSize;
  if (item.byExtensionId) candidate.byExtensionId = item.byExtensionId;
  if (item.danger) candidate.danger = item.danger;
  if (item.state) candidate.state = item.state;
  const storeId = (item as { cookieStoreId?: unknown }).cookieStoreId;
  if (typeof storeId === "string" && storeId) candidate.cookieStoreId = storeId;
  return candidate;
}

export function downloadControl(api: Api): DownloadControl {
  return {
    async cancel(id) {
      await api.downloads.cancel(id);
    },
    async erase(id) {
      await api.downloads.erase({ id });
    },
    async state(id): Promise<BrowserDownloadState> {
      const [item] = await api.downloads.search({ id });
      if (!item) return "missing";
      return item.state as BrowserDownloadState;
    },
  };
}
