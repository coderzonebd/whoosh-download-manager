// SPDX-License-Identifier: GPL-3.0-or-later

import { parseHttpUrl } from "./url";

/** Last component of a path the browser reports, with either separator. */
export function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] ?? "";
}

/** Lower-case extension without the dot; "" when there is none. */
export function extensionOf(name: string): string {
  const base = basename(name);
  const dot = base.lastIndexOf(".");
  if (dot <= 0 || dot === base.length - 1) return "";
  return base.slice(dot + 1).toLowerCase();
}

function safeDecode(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

/** The last non-empty path segment of an http(s) URL, percent-decoded. */
export function filenameFromUrl(value: string): string {
  const url = parseHttpUrl(value);
  if (!url) return "";
  const segments = url.pathname.split("/").filter((s) => s.length > 0);
  const last = segments[segments.length - 1];
  return last ? basename(safeDecode(last)) : "";
}

/**
 * Tidies a name before handing it over: no directories, no control
 * characters, no surrounding whitespace, never "." or "..". The daemon
 * sanitises again for the target file system; this only keeps obvious junk
 * out of the request.
 */
export function cleanFilename(name: string): string {
  // eslint-disable-next-line no-control-regex
  const cleaned = basename(name).replace(/[\u0000-\u001f\u007f]/g, "").trim();
  return cleaned === "." || cleaned === ".." ? "" : cleaned;
}

/**
 * The name to send with `download.add`: the browser's choice when it made
 * one (it has already parsed Content-Disposition), else the URL's last
 * segment. Undefined lets the daemon decide.
 */
export function chooseFilename(browserName: string | undefined, url: string): string | undefined {
  const fromBrowser = browserName ? cleanFilename(browserName) : "";
  if (fromBrowser) return fromBrowser;
  const fromUrl = cleanFilename(filenameFromUrl(url));
  return fromUrl || undefined;
}
