// SPDX-License-Identifier: GPL-3.0-or-later

/** Parses an absolute http(s) URL; anything else (blob:, data:, file:, …) is undefined. */
export function parseHttpUrl(value: string | undefined): URL | undefined {
  if (!value) return undefined;
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return undefined;
  }
  return url.protocol === "http:" || url.protocol === "https:" ? url : undefined;
}

export function isHttpUrl(value: string | undefined): boolean {
  return parseHttpUrl(value) !== undefined;
}

/** Lower-case host name of an http(s) URL. */
export function hostOf(value: string | undefined): string | undefined {
  return parseHttpUrl(value)?.hostname.toLowerCase();
}

/** The URL without its #fragment, which is never sent to a server. */
export function stripFragment(value: string): string {
  const hash = value.indexOf("#");
  return hash === -1 ? value : value.slice(0, hash);
}

/**
 * What a browser would put in `Referer`: no fragment, no user:password.
 * Undefined for non-http(s) pages (the browser sends none for those either).
 */
export function refererFrom(value: string | undefined): string | undefined {
  const url = parseHttpUrl(value);
  if (!url) return undefined;
  url.hash = "";
  url.username = "";
  url.password = "";
  return url.href;
}

/**
 * Normalises a site-exclusion entry: accepts `example.com`, `*.example.com`,
 * `.example.com` or a pasted URL; returns the bare lower-case host.
 */
export function normalizeSitePattern(pattern: string): string {
  let p = pattern.trim().toLowerCase();
  if (p.includes("://")) {
    try {
      p = new URL(p).hostname;
    } catch {
      return "";
    }
  }
  p = p.replace(/^\*\./, "").replace(/^\./, "");
  const slash = p.indexOf("/");
  if (slash !== -1) p = p.slice(0, slash);
  return p.replace(/:\d+$/, "").replace(/\.$/, "");
}

/** `example.com` matches `example.com` and every subdomain of it. */
export function hostMatches(host: string, pattern: string): boolean {
  const p = normalizeSitePattern(pattern);
  if (!p) return false;
  const h = host.toLowerCase().replace(/\.$/, "");
  return h === p || h.endsWith(`.${p}`);
}

export function isSiteExcluded(urls: (string | undefined)[], patterns: readonly string[]): boolean {
  if (patterns.length === 0) return false;
  return urls.some((u) => {
    const host = hostOf(u);
    return host !== undefined && patterns.some((p) => hostMatches(host, p));
  });
}
