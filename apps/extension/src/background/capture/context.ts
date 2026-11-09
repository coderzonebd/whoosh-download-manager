// SPDX-License-Identifier: GPL-3.0-or-later

// Builds the RequestContext that lets the daemon repeat the browser's
// request: cookies for the URL, the referrer, the user agent.

import type { AddParams, RequestContext } from "@vayu/api-client";

import { chooseFilename } from "../../shared/filename";
import { refererFrom } from "../../shared/url";

export interface CookieLike {
  name: string;
  value: string;
  path: string;
}

/**
 * The `Cookie` header the browser would send: longer paths first
 * (RFC 6265 §5.4, the browser's creation-time tiebreak is not exposed, so
 * the store's order is kept), `name=value` pairs joined by "; ".
 */
export function cookieHeader(cookies: readonly CookieLike[]): string | undefined {
  if (cookies.length === 0) return undefined;
  const sorted = cookies
    .map((cookie, index) => ({ cookie, index }))
    .sort((a, b) => b.cookie.path.length - a.cookie.path.length || a.index - b.index)
    .map(({ cookie }) => (cookie.name === "" ? cookie.value : `${cookie.name}=${cookie.value}`));
  return sorted.join("; ");
}

export interface ContextInput {
  cookies: readonly CookieLike[];
  referrer?: string | undefined;
  userAgent?: string | undefined;
}

/** Omits what is empty (the protocol never sends nulls); undefined when nothing is left. */
export function buildRequestContext(input: ContextInput): RequestContext | undefined {
  const context: RequestContext = {};
  const cookie = cookieHeader(input.cookies);
  if (cookie) context.cookie = cookie;
  const referer = refererFrom(input.referrer);
  if (referer) context.referer = referer;
  if (input.userAgent) context.userAgent = input.userAgent;
  return Object.keys(context).length > 0 ? context : undefined;
}

export interface AddInput extends ContextInput {
  url: string;
  filename?: string | undefined;
}

export function buildAddParams(input: AddInput): AddParams {
  const params: AddParams = { url: input.url };
  const request = buildRequestContext(input);
  if (request) params.request = request;
  const filename = chooseFilename(input.filename, input.url);
  if (filename) params.filename = filename;
  return params;
}

/** The cookie store of a private window, per browser. */
export function privateCookieStore(isFirefox: boolean): string {
  return isFirefox ? "firefox-private" : "1";
}

export interface CookieSource {
  getAll(details: { url: string; storeId?: string }): Promise<CookieLike[]>;
}

/** Cookies for `url`; an empty list if the store refuses (e.g. Firefox first-party isolation). */
export async function cookiesFor(source: CookieSource, url: string, storeId: string | undefined): Promise<CookieLike[]> {
  try {
    return await source.getAll(storeId === undefined ? { url } : { url, storeId });
  } catch {
    return [];
  }
}
