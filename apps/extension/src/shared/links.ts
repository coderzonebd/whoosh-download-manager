// SPDX-License-Identifier: GPL-3.0-or-later

import { extensionOf, filenameFromUrl } from "./filename";
import type { LinkInfo } from "./messages";
import { parseHttpUrl, stripFragment } from "./url";

/**
 * Keeps http(s) links only, drops #fragments, and removes duplicates while
 * keeping the first occurrence (and its text) in document order.
 */
export function dedupeLinks(links: readonly LinkInfo[]): LinkInfo[] {
  const seen = new Set<string>();
  const out: LinkInfo[] = [];
  for (const link of links) {
    const url = parseHttpUrl(link.url);
    if (!url) continue;
    const key = stripFragment(url.href);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ url: key, text: link.text });
  }
  return out;
}

/** A link worth warming up a connection for: a known file type, or `<a download>`. */
export function looksLikeDownloadLink(href: string, fileTypes: readonly string[], hasDownloadAttribute: boolean): boolean {
  if (!parseHttpUrl(href)) return false;
  if (hasDownloadAttribute) return true;
  const ext = extensionOf(filenameFromUrl(href));
  return ext !== "" && (fileTypes.length === 0 || fileTypes.includes(ext));
}

/** The links a context-menu command should send. */
export function selectLinks(
  links: readonly LinkInfo[],
  scope: "all" | "selection",
  fileTypes: readonly string[],
  max = 500,
): string[] {
  let unique = dedupeLinks(links);
  if (scope === "all") {
    // "All links" on a page means its downloads, not its navigation.
    unique = unique.filter((l) => looksLikeDownloadLink(l.url, fileTypes, false));
  }
  return unique.slice(0, max).map((l) => l.url);
}

/**
 * At most one preconnect per host per interval. Returns the origin to warm
 * up — only the origin leaves the page, never the full link.
 */
export class PreconnectThrottle {
  private readonly last = new Map<string, number>();

  constructor(
    private readonly intervalMs = 10_000,
    private readonly maxHosts = 512,
  ) {}

  allow(href: string, now: number): string | undefined {
    const url = parseHttpUrl(href);
    if (!url) return undefined;
    const key = url.origin;
    const at = this.last.get(key);
    if (at !== undefined && now - at < this.intervalMs) return undefined;
    this.last.delete(key);
    this.last.set(key, now);
    if (this.last.size > this.maxHosts) {
      const oldest = this.last.keys().next().value;
      if (oldest !== undefined) this.last.delete(oldest);
    }
    return `${url.origin}/`;
  }
}
