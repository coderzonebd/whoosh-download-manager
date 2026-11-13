// SPDX-License-Identifier: GPL-3.0-or-later

// Messages between the content script / popup and the background script.
// The background validates every one: content scripts run inside web pages.

import type { ConnectionStatus } from "../transport/types";

export interface MediaEntry {
  url: string;
  kind: "hls" | "dash" | "video" | "audio";
  mime?: string;
  /** Bytes, when the response said. */
  size?: number;
  seenAt: number;
}

export interface LinkInfo {
  url: string;
  text: string;
}

/** Content script → background. */
export type ContentMessage =
  | { type: "whoosh/preconnect"; url: string }
  /** The user Alt+clicked: let the browser keep the next download. */
  | { type: "whoosh/bypass-next" };

/** Background → content script. */
export type CollectLinksMessage = { type: "whoosh/collect-links"; scope: "all" | "selection" };
export interface CollectLinksReply {
  links: LinkInfo[];
}

/** Popup → background. */
export type PopupMessage =
  | { type: "whoosh/popup-state"; tabId?: number; pageUrl?: string }
  | { type: "whoosh/set-capture"; enabled: boolean }
  | { type: "whoosh/set-site-excluded"; host: string; excluded: boolean }
  | { type: "whoosh/download-media"; url: string; pageUrl?: string };

export interface PopupState {
  status: ConnectionStatus;
  active?: number;
  queued?: number;
  captureEnabled: boolean;
  host?: string;
  siteExcluded: boolean;
  media: MediaEntry[];
  lastCapture?: { url: string; outcome: string; at: number };
}

export type ActionReply = { ok: true } | { ok: false; error: string };

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** Narrows an untrusted message from a content script. */
export function asContentMessage(value: unknown): ContentMessage | undefined {
  if (!isObject(value)) return undefined;
  if (value["type"] === "whoosh/preconnect" && typeof value["url"] === "string") {
    return { type: "whoosh/preconnect", url: value["url"] };
  }
  if (value["type"] === "whoosh/bypass-next") return { type: "whoosh/bypass-next" };
  return undefined;
}

/** Narrows a message that claims to come from the popup. */
export function asPopupMessage(value: unknown): PopupMessage | undefined {
  if (!isObject(value)) return undefined;
  const optionalNumber = (v: unknown) => (typeof v === "number" ? v : undefined);
  const optionalString = (v: unknown) => (typeof v === "string" ? v : undefined);
  switch (value["type"]) {
    case "whoosh/popup-state": {
      const msg: PopupMessage = { type: "whoosh/popup-state" };
      const tabId = optionalNumber(value["tabId"]);
      const pageUrl = optionalString(value["pageUrl"]);
      if (tabId !== undefined) msg.tabId = tabId;
      if (pageUrl !== undefined) msg.pageUrl = pageUrl;
      return msg;
    }
    case "whoosh/set-capture":
      return typeof value["enabled"] === "boolean" ? { type: "whoosh/set-capture", enabled: value["enabled"] } : undefined;
    case "whoosh/set-site-excluded":
      return typeof value["host"] === "string" && typeof value["excluded"] === "boolean"
        ? { type: "whoosh/set-site-excluded", host: value["host"], excluded: value["excluded"] }
        : undefined;
    case "whoosh/download-media": {
      if (typeof value["url"] !== "string") return undefined;
      const msg: PopupMessage = { type: "whoosh/download-media", url: value["url"] };
      const pageUrl = optionalString(value["pageUrl"]);
      if (pageUrl !== undefined) msg.pageUrl = pageUrl;
      return msg;
    }
    default:
      return undefined;
  }
}
