// SPDX-License-Identifier: GPL-3.0-or-later

// Messages between the content script / popup and the background script.
// The background validates every one: content scripts run inside web pages.

import type { ConnectionStatus } from "../transport/types";

/** A question from self-healing for the user (mirrors `HealAsk` in the background). */
export interface HealQuestion {
  id: string;
  filename: string;
  pageUrl?: string;
  kind: "open-page" | "confirm";
  candidateUrl?: string;
  askedAt: number;
}

export type HealAnswer = "open-page" | "use-candidate" | "dismiss";
const HEAL_ANSWERS: readonly string[] = ["open-page", "use-candidate", "dismiss"];

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
  | { type: "whoosh/bypass-next" }
  /** The user Alt+clicked with "send to Whoosh": capture the next download whatever the rules say. */
  | { type: "whoosh/force-next" }
  /** The user clicked a download-looking link; its text helps self-healing find it again. */
  | { type: "whoosh/link-click"; url: string; text: string };

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
  | { type: "whoosh/download-media"; url: string; pageUrl?: string }
  | { type: "whoosh/heal-answer"; id: string; answer: HealAnswer };

export interface PopupState {
  status: ConnectionStatus;
  active?: number;
  queued?: number;
  captureEnabled: boolean;
  host?: string;
  siteExcluded: boolean;
  media: MediaEntry[];
  lastCapture?: { url: string; outcome: string; at: number };
  /** Expired downloads that need the user (self-healing). */
  heals: HealQuestion[];
  /** Store build: this page is on a site the store extension does not work on. */
  pageBlocked: boolean;
  /** What Alt+click does (for the hint). */
  altClick: "browser" | "whoosh" | "off";
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
  if (value["type"] === "whoosh/force-next") return { type: "whoosh/force-next" };
  if (value["type"] === "whoosh/link-click" && typeof value["url"] === "string" && typeof value["text"] === "string") {
    return { type: "whoosh/link-click", url: value["url"], text: value["text"].slice(0, 200) };
  }
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
    case "whoosh/heal-answer":
      return typeof value["id"] === "string" &&
        typeof value["answer"] === "string" &&
        HEAL_ANSWERS.includes(value["answer"])
        ? { type: "whoosh/heal-answer", id: value["id"], answer: value["answer"] as HealAnswer }
        : undefined;
    default:
      return undefined;
  }
}
