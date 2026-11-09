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
  | "too-small";

export type CaptureDecision = { capture: true } | { capture: false; reason: SkipReason };

export interface RuleContext {
  /** The user asked for this one to stay in the browser (Alt+click). */
  bypass: boolean;
  /** The download came from a non-GET request we cannot replay faithfully. */
  nonGetRequest: boolean;
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

export function evaluateCapture(c: CandidateDownload, settings: Settings, ctx: RuleContext): CaptureDecision {
  const skip = (reason: SkipReason): CaptureDecision => ({ capture: false, reason });

  if (!settings.captureEnabled) return skip("disabled");
  if (ctx.bypass) return skip("bypassed");
  if (!isHttpUrl(c.url)) return skip("not-http");
  if (c.incognito && !settings.captureIncognito) return skip("incognito");
  if (c.byExtensionId) return skip("extension");
  if (c.danger !== undefined && !HANDOFF_DANGER.has(c.danger)) return skip("dangerous");
  if (c.state !== undefined && c.state !== "in_progress") return skip("not-in-progress");
  if (ctx.nonGetRequest) return skip("post");
  if (isSiteExcluded([c.url, c.originalUrl, c.referrer], settings.excludedSites)) return skip("excluded-site");
  if (!matchesFileTypes(c, settings.fileTypes)) return skip("file-type");
  if (settings.minSizeBytes > 0 && c.totalBytes !== undefined && c.totalBytes > 0 && c.totalBytes < settings.minSizeBytes) {
    return skip("too-small");
  }
  return { capture: true };
}
