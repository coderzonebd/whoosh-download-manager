// SPDX-License-Identifier: GPL-3.0-or-later

// Finding the fresh address of an expired download (docs/self-healing.md).
// Pure functions: given what was remembered at hand-off and what the browser
// shows now, propose a URL and say how sure the proposal is. "Unsure"
// proposals are never used without asking the user. Whatever is proposed,
// the daemon still checks that it is the same file before continuing.

import { basename, filenameFromUrl } from "../../shared/filename";
import type { LinkInfo } from "../../shared/messages";
import { hostOf, parseHttpUrl, stripFragment } from "../../shared/url";

/** What the healer knows about the download it wants to continue. */
export interface HealTarget {
  /** The URL that was handed over (often signed, now expired). */
  url: string;
  /** The link the download started from, when it differs from `url`. */
  linkUrl?: string;
  linkText?: string;
  filename?: string;
  totalBytes?: number;
  originPageUrl?: string;
}

export type MatchReason =
  /** The page links the very link the download started from. */
  | "same-link"
  /** Same host and path, different query: a freshly signed copy of the link. */
  | "same-path"
  /** Same file name on the same site. */
  | "same-name"
  /** Same file name on another host. */
  | "same-name-other-host"
  /** The link text is the one the user clicked. */
  | "same-text";

export interface LinkMatch {
  url: string;
  reason: MatchReason;
  /** "sure" proposals are sent to the daemon at once; "unsure" ones go to the user first. */
  confidence: "sure" | "unsure";
}

function normalizedText(text: string | undefined): string {
  return (text ?? "").replace(/\s+/g, " ").trim().toLowerCase();
}

function sameName(a: string | undefined, b: string | undefined): boolean {
  if (!a || !b) return false;
  return basename(a).toLowerCase() === basename(b).toLowerCase();
}

/** The site part of a host, for "same site" checks (`dl.example.com` → `example.com`). */
export function siteOf(host: string): string {
  const parts = host.split(".");
  if (parts.length <= 2 || /^\d+$/.test(parts.at(-1) ?? "")) return host;
  // A two-letter country domain with a short second level (co.uk, com.bd,
  // ac.jp …) keeps three labels. Good enough without the public suffix list.
  const last = parts.at(-1) ?? "";
  const second = parts.at(-2) ?? "";
  if (last.length === 2 && second.length <= 3) return parts.slice(-3).join(".");
  return parts.slice(-2).join(".");
}

export function sameSite(a: string | undefined, b: string | undefined): boolean {
  const ha = hostOf(a);
  const hb = hostOf(b);
  return ha !== undefined && hb !== undefined && siteOf(ha) === siteOf(hb);
}

/**
 * Picks the link on the download's page that most likely leads to the same
 * file. Links equal to the expired `url` are skipped: asking for them again
 * would only fail again (fresh cookies on the old link are tried separately).
 */
export function matchLink(links: readonly LinkInfo[], target: HealTarget): LinkMatch | undefined {
  const expired = stripFragment(target.url);
  const candidates: { url: URL; text: string }[] = [];
  const seen = new Set<string>();
  for (const link of links) {
    const url = parseHttpUrl(link.url);
    if (!url) continue;
    url.hash = "";
    if (url.href === expired || seen.has(url.href)) continue;
    seen.add(url.href);
    candidates.push({ url, text: link.text });
  }
  if (candidates.length === 0) return undefined;

  const references = [target.linkUrl, target.url]
    .map((u) => parseHttpUrl(u))
    .filter((u): u is URL => u !== undefined);

  // 1. The stable link itself.
  const link = target.linkUrl ? stripFragment(target.linkUrl) : undefined;
  if (link) {
    const hit = candidates.find((c) => c.url.href === link);
    if (hit) return { url: hit.url.href, reason: "same-link", confidence: "sure" };
  }

  // 2. Same origin and path, new query (a re-signed URL). Several different
  //    ones (e.g. one path, many `?part=` values) make it a guess.
  const samePath = candidates.filter((c) =>
    references.some((r) => r.origin === c.url.origin && r.pathname === c.url.pathname),
  );
  if (samePath.length === 1 && samePath[0]) {
    return { url: samePath[0].url.href, reason: "same-path", confidence: "sure" };
  }
  if (samePath.length > 1 && samePath[0]) {
    return { url: samePath[0].url.href, reason: "same-path", confidence: "unsure" };
  }

  // 3. The same file name in the link's path.
  const name = target.filename ?? filenameFromUrl(target.url);
  if (name) {
    const named = candidates.filter((c) => sameName(filenameFromUrl(c.url.href), name));
    const onSite = named.filter((c) => references.some((r) => sameSite(r.href, c.url.href)));
    if (onSite.length === 1 && onSite[0]) {
      return { url: onSite[0].url.href, reason: "same-name", confidence: "sure" };
    }
    const first = onSite[0] ?? named[0];
    if (first) {
      return {
        url: first.url.href,
        reason: onSite.length > 0 ? "same-name" : "same-name-other-host",
        confidence: "unsure",
      };
    }
  }

  // 4. The text of the link the user clicked.
  const text = normalizedText(target.linkText);
  if (text) {
    const hit = candidates.find((c) => normalizedText(c.text) === text);
    if (hit) return { url: hit.url.href, reason: "same-text", confidence: "unsure" };
  }
  return undefined;
}

/** A download the browser just started, as far as same-file matching needs it. */
export interface BrowserFile {
  url: string;
  originalUrl?: string;
  referrer?: string;
  filename?: string;
  totalBytes?: number;
}

/**
 * Whether a new browser download is the same file as the expired one.
 * `armed`: the user asked to open the page for this download, so the next
 * matching download from anywhere counts; otherwise it must come from the
 * same site as the original.
 */
export function isSameFile(file: BrowserFile, target: HealTarget, armed: boolean): boolean {
  const fileName = file.filename ?? filenameFromUrl(file.url);
  const targetName = target.filename ?? filenameFromUrl(target.url);
  const names = sameName(fileName, targetName);
  const sizesKnown = file.totalBytes !== undefined && target.totalBytes !== undefined;
  if (sizesKnown && file.totalBytes !== target.totalBytes) return false;
  if (!names) return false;
  if (armed) return true;
  const theirs = [file.url, file.originalUrl, file.referrer];
  const ours = [target.url, target.linkUrl, target.originPageUrl];
  return theirs.some((a) => ours.some((b) => sameSite(a, b)));
}
