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
  | { type: "wdm/preconnect"; url: string }
  /** The user Alt+clicked: let the browser keep the next download. */
  | { type: "wdm/bypass-next" }
  /** The user Alt+clicked with "send to WDM": capture the next download whatever the rules say. */
  | { type: "wdm/force-next" }
  /** The user clicked a download-looking link; its text helps self-healing find it again. */
  | { type: "wdm/link-click"; url: string; text: string }
  /**
   * "Download with WDM" on a video: hand this page (the frame's, for an embedded player) to the
   * app's media picker. Answered with an {@link ActionReply}. The background trusts the sender's
   * frame URL over `url`.
   */
  | { type: "wdm/offer-video"; url: string; title: string };

/** Longest page title passed on. */
export const MAX_TITLE = 300;

/** Background → content script. */
export type CollectLinksMessage = { type: "wdm/collect-links"; scope: "all" | "selection" };
export interface CollectLinksReply {
  links: LinkInfo[];
}

/** Popup → background. */
export type PopupMessage =
  | { type: "wdm/popup-state"; tabId?: number; pageUrl?: string }
  | { type: "wdm/set-capture"; enabled: boolean }
  | { type: "wdm/set-site-excluded"; host: string; excluded: boolean }
  | { type: "wdm/download-media"; url: string; pageUrl?: string }
  /** "Download this video with WDM": the current tab's page to the app's media picker. */
  | { type: "wdm/offer-page"; tabId?: number; url: string; title?: string }
  | { type: "wdm/heal-answer"; id: string; answer: HealAnswer };

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
  /** "Download this video with WDM" is offered: an http(s) page the policy allows. */
  canOffer: boolean;
  /** What Alt+click does (for the hint). */
  altClick: "browser" | "wdm" | "off";
}

export type ActionReply = { ok: true } | { ok: false; error: string };

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** Narrows an untrusted message from a content script. */
export function asContentMessage(value: unknown): ContentMessage | undefined {
  if (!isObject(value)) return undefined;
  if (value["type"] === "wdm/preconnect" && typeof value["url"] === "string") {
    return { type: "wdm/preconnect", url: value["url"] };
  }
  if (value["type"] === "wdm/bypass-next") return { type: "wdm/bypass-next" };
  if (value["type"] === "wdm/force-next") return { type: "wdm/force-next" };
  if (value["type"] === "wdm/link-click" && typeof value["url"] === "string" && typeof value["text"] === "string") {
    return { type: "wdm/link-click", url: value["url"], text: value["text"].slice(0, 200) };
  }
  if (value["type"] === "wdm/offer-video" && typeof value["url"] === "string") {
    const title = typeof value["title"] === "string" ? value["title"].slice(0, MAX_TITLE) : "";
    return { type: "wdm/offer-video", url: value["url"], title };
  }
  return undefined;
}

/** Narrows a message that claims to come from the popup. */
export function asPopupMessage(value: unknown): PopupMessage | undefined {
  if (!isObject(value)) return undefined;
  const optionalNumber = (v: unknown) => (typeof v === "number" ? v : undefined);
  const optionalString = (v: unknown) => (typeof v === "string" ? v : undefined);
  switch (value["type"]) {
    case "wdm/popup-state": {
      const msg: PopupMessage = { type: "wdm/popup-state" };
      const tabId = optionalNumber(value["tabId"]);
      const pageUrl = optionalString(value["pageUrl"]);
      if (tabId !== undefined) msg.tabId = tabId;
      if (pageUrl !== undefined) msg.pageUrl = pageUrl;
      return msg;
    }
    case "wdm/set-capture":
      return typeof value["enabled"] === "boolean" ? { type: "wdm/set-capture", enabled: value["enabled"] } : undefined;
    case "wdm/set-site-excluded":
      return typeof value["host"] === "string" && typeof value["excluded"] === "boolean"
        ? { type: "wdm/set-site-excluded", host: value["host"], excluded: value["excluded"] }
        : undefined;
    case "wdm/download-media": {
      if (typeof value["url"] !== "string") return undefined;
      const msg: PopupMessage = { type: "wdm/download-media", url: value["url"] };
      const pageUrl = optionalString(value["pageUrl"]);
      if (pageUrl !== undefined) msg.pageUrl = pageUrl;
      return msg;
    }
    case "wdm/offer-page": {
      if (typeof value["url"] !== "string") return undefined;
      const msg: PopupMessage = { type: "wdm/offer-page", url: value["url"] };
      const tabId = optionalNumber(value["tabId"]);
      const title = optionalString(value["title"]);
      if (tabId !== undefined) msg.tabId = tabId;
      if (title !== undefined) msg.title = title.slice(0, MAX_TITLE);
      return msg;
    }
    case "wdm/heal-answer":
      return typeof value["id"] === "string" &&
        typeof value["answer"] === "string" &&
        HEAL_ANSWERS.includes(value["answer"])
        ? { type: "wdm/heal-answer", id: value["id"], answer: value["answer"] as HealAnswer }
        : undefined;
    default:
      return undefined;
  }
}
