// SPDX-License-Identifier: GPL-3.0-or-later

// Reading the links of a download's page for self-healing: from a tab that
// already shows it, or else from a muted background tab that is closed again
// afterwards. At most one healing tab at a time. No new permissions: host
// access already covers reading the tab and injecting the content script.

import type { LinkInfo } from "../../shared/messages";
import { stripFragment } from "../../shared/url";
import type { Api } from "../browser";
import { collectLinks } from "../menus";

export const PAGE_LOAD_TIMEOUT_MS = 30_000;

function samePage(a: string | undefined, b: string): boolean {
  return a !== undefined && stripFragment(a) === stripFragment(b);
}

function waitForLoad(api: Api, tabId: number, timeoutMs: number): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      api.tabs.onUpdated.removeListener(listener);
      reject(new Error("the page did not load in time"));
    }, timeoutMs);
    const listener = (id: number, change: chrome.tabs.OnUpdatedInfo) => {
      if (id !== tabId || change.status !== "complete") return;
      clearTimeout(timer);
      api.tabs.onUpdated.removeListener(listener);
      resolve();
    };
    api.tabs.onUpdated.addListener(listener);
    // It may have finished before the listener was added.
    void api.tabs.get(tabId).then(
      (tab) => {
        if (tab.status === "complete") listener(tabId, { status: "complete" } as chrome.tabs.OnUpdatedInfo);
      },
      () => {},
    );
  });
}

export function createPageReader(api: Api, firefox: boolean, timeoutMs = PAGE_LOAD_TIMEOUT_MS) {
  let queue: Promise<unknown> = Promise.resolve();

  async function read(pageUrl: string, cookieStoreId: string | undefined): Promise<LinkInfo[]> {
    // A tab that already shows the page (same container on Firefox).
    const tabs = await api.tabs.query({});
    const open = tabs.find(
      (t) =>
        t.id !== undefined &&
        !t.incognito &&
        t.status === "complete" &&
        samePage(t.url, pageUrl) &&
        (!firefox || (t as { cookieStoreId?: string }).cookieStoreId === (cookieStoreId ?? "firefox-default")),
    );
    if (open?.id !== undefined) return collectLinks(api, open.id, 0, "all");

    const props: chrome.tabs.CreateProperties & { cookieStoreId?: string } = { url: pageUrl, active: false };
    if (firefox && cookieStoreId) props.cookieStoreId = cookieStoreId;
    const tab = await api.tabs.create(props);
    const tabId = tab.id;
    if (tabId === undefined) throw new Error("no tab");
    try {
      await api.tabs.update(tabId, { muted: true }).catch(() => {});
      await waitForLoad(api, tabId, timeoutMs);
      return await collectLinks(api, tabId, 0, "all");
    } finally {
      await api.tabs.remove(tabId).catch(() => {});
    }
  }

  /** One healing tab at a time. */
  return (pageUrl: string, cookieStoreId: string | undefined): Promise<LinkInfo[]> => {
    const run = queue.then(
      () => read(pageUrl, cookieStoreId),
      () => read(pageUrl, cookieStoreId),
    );
    queue = run.catch(() => {});
    return run;
  };
}

/** "Open page": the page in a foreground tab, in the download's container. */
export async function openPage(api: Api, firefox: boolean, pageUrl: string, cookieStoreId: string | undefined): Promise<void> {
  const props: chrome.tabs.CreateProperties & { cookieStoreId?: string } = { url: pageUrl, active: true };
  if (firefox && cookieStoreId) props.cookieStoreId = cookieStoreId;
  await api.tabs.create(props);
}
