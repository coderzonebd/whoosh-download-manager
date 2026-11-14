// SPDX-License-Identifier: GPL-3.0-or-later

// Whether a browser download should go to the daemon. Pure: everything it
// needs is passed in, so the rules are unit-tested without a browser.

import { MIME_EXTENSIONS } from "../../shared/constants";
import { extensionOf, filenameFromUrl } from "../../shared/filename";
import type { Settings } from "../../shared/settings";
import { isHttpUrl, isSiteExcluded } from "../../shared/url";

/** A browser download in browser-neutral form. */
export interface CandidateDownload {
  /** The browser's download id. */
  id: number;
  /** URL that served the file (Chromium: `finalUrl`, after redirects). */
  url: string;
  /** URL the download started from, when different. */
  originalUrl?: string;
  referrer?: string;
  /** The browser's chosen file name or path. */
  filename?: string;
  mime?: string;
  /** Bytes; absent or ≤ 0 when unknown. */
  totalBytes?: number;
  incognito: boolean;
  /** Set when an extension started the download. */
  byExtensionId?: string;
  /** Chromium's Safe Browsing verdict. */
  danger?: string;
  /** Firefox container / cookie store of the download. */
  cookieStoreId?: string;
  state?: string;
}

export type SkipReason =
  | "disabled"
  | "bypassed"
  | "not-http"
  | "incognito"
  | "extension"
  | "dangerous"
  | "not-in-progress"
  | "post"
  | "excluded-site"
  | "file-type"
  | "too-small"
  /** Store build: a site the store extension stays away from (MASTER-PLAN §6). */
  | "store-policy";

export type CaptureDecision = { capture: true } | { capture: false; reason: SkipReason };

export interface RuleContext {
  /** The user asked for this one to stay in the browser (Alt+click). */
  bypass: boolean;
  /** The download came from a non-GET request we cannot replay faithfully. */
  nonGetRequest: boolean;
  /**
   * The user asked for this one to go to Whoosh (Alt+click with the
   * "send to Whoosh" setting): capture on/off, site, type and size rules are
   * skipped; the safety rules still apply.
   */
  force?: boolean;
  /** Store build: the download, its link or its page is on a blocked site. */
  blocked?: boolean;
}

/**
 * Chromium `danger` values that still allow a hand-off. Safe Browsing
 * verdicts (`url`, `content`, `host`, `uncommon`, `unwanted`, …) keep the
 * download in the browser so its warning UI stays in charge. `file` only
 * means "executable type"; the daemon writes mark-of-the-web for those.
 */
const HANDOFF_DANGER = new Set(["safe", "accepted", "file", "allowlistedByPolicy", "deepScannedSafe"]);

/** The extension from the name, the URL, or the MIME type, in that order. */
export function fileTypeOf(c: Pick<CandidateDownload, "filename" | "url" | "mime">): string {
  const fromName = c.filename ? extensionOf(c.filename) : "";
  if (fromName) return fromName;
  const fromUrl = extensionOf(filenameFromUrl(c.url));
  if (fromUrl) return fromUrl;
  const mime = c.mime?.split(";")[0]?.trim().toLowerCase();
  return (mime && MIME_EXTENSIONS[mime]) || "";
}

export function matchesFileTypes(c: Pick<CandidateDownload, "filename" | "url" | "mime">, types: readonly string[]): boolean {
  if (types.length === 0) return true;
  const type = fileTypeOf(c);
  return type !== "" && types.includes(type);
}

/**
 * The rules no user choice overrides: what Whoosh cannot repeat faithfully,
 * what the browser must keep showing a warning for, and the store policy.
 * Also used before a browser download is taken over to refresh an expired one.
 */
export function safetySkip(c: CandidateDownload, settings: Settings, ctx: RuleContext): SkipReason | undefined {
  if (!isHttpUrl(c.url)) return "not-http";
  if (ctx.blocked) return "store-policy";
  if (c.incognito && !settings.captureIncognito) return "incognito";
  if (c.byExtensionId) return "extension";
  if (c.danger !== undefined && !HANDOFF_DANGER.has(c.danger)) return "dangerous";
  if (c.state !== undefined && c.state !== "in_progress") return "not-in-progress";
  if (ctx.nonGetRequest) return "post";
  return undefined;
}

export function evaluateCapture(c: CandidateDownload, settings: Settings, ctx: RuleContext): CaptureDecision {
  const skip = (reason: SkipReason): CaptureDecision => ({ capture: false, reason });

  if (!settings.captureEnabled && !ctx.force) return skip("disabled");
  if (ctx.bypass) return skip("bypassed");
  const unsafe = safetySkip(c, settings, ctx);
  if (unsafe) return skip(unsafe);
  if (ctx.force) return { capture: true };
  if (isSiteExcluded([c.url, c.originalUrl, c.referrer], settings.excludedSites)) return skip("excluded-site");
  if (!matchesFileTypes(c, settings.fileTypes)) return skip("file-type");
  if (settings.minSizeBytes > 0 && c.totalBytes !== undefined && c.totalBytes > 0 && c.totalBytes < settings.minSizeBytes) {
    return skip("too-small");
  }
  return { capture: true };
}
