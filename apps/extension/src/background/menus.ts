// SPDX-License-Identifier: GPL-3.0-or-later

import type { CollectLinksMessage, CollectLinksReply, LinkInfo } from "../shared/messages";
import type { Api } from "./browser";

export const MENU = {
  link: "vayu-link",
  selection: "vayu-selection",
  page: "vayu-all-links",
} as const;

/** (Re)creates the context menu items. Safe to call on every install/startup. */
export async function createMenus(api: Api): Promise<void> {
  await api.contextMenus.removeAll();
  const documentUrlPatterns = ["http://*/*", "https://*/*"];
  api.contextMenus.create({ id: MENU.link, title: "Download link with Vayu", contexts: ["link"], documentUrlPatterns });
  api.contextMenus.create({
    id: MENU.selection,
    title: "Download selected links with Vayu",
    contexts: ["selection"],
    documentUrlPatterns,
  });
  api.contextMenus.create({ id: MENU.page, title: "Download all links with Vayu", contexts: ["page"], documentUrlPatterns });
}

function isReply(value: unknown): value is CollectLinksReply {
  return typeof value === "object" && value !== null && Array.isArray((value as CollectLinksReply).links);
}

/**
 * Asks the content script in the clicked frame for its links. Pages opened
 * before the extension was installed have no content script yet, so inject
 * it once and ask again.
 */
export async function collectLinks(
  api: Api,
  tabId: number,
  frameId: number,
  scope: CollectLinksMessage["scope"],
): Promise<LinkInfo[]> {
  const message: CollectLinksMessage = { type: "vayu/collect-links", scope };
  const ask = async () => {
    const reply: unknown = await api.tabs.sendMessage(tabId, message, { frameId });
    if (!isReply(reply)) throw new Error("no link list from the page");
    return reply.links.filter(
      (l): l is LinkInfo => typeof l === "object" && l !== null && typeof l.url === "string" && typeof l.text === "string",
    );
  };
  try {
    return await ask();
  } catch {
    await api.scripting.executeScript({ target: { tabId, frameIds: [frameId] }, files: ["content.js"] });
    return ask();
  }
}
