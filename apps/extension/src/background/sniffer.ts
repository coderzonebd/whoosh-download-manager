// SPDX-License-Identifier: GPL-3.0-or-later

// Media sniffer: watches response headers (read-only, never blocking) for
// audio, video, HLS and DASH, and keeps a short per-tab list for the popup.
// Generic by design: no site-specific logic (store policy, MASTER-PLAN §6).

import { extensionOf, filenameFromUrl } from "../shared/filename";
import type { MediaEntry } from "../shared/messages";
import { parseHttpUrl, stripFragment } from "../shared/url";

const HLS_TYPES = new Set([
  "application/vnd.apple.mpegurl",
  "application/x-mpegurl",
  "audio/mpegurl",
  "audio/x-mpegurl",
]);
const DASH_TYPES = new Set(["application/dash+xml"]);
/** Pieces of a stream, not something to download on its own. */
const SEGMENT_TYPES = new Set(["video/mp2t", "audio/mp2t", "video/iso.segment", "audio/iso.segment"]);
const SEGMENT_EXTENSIONS = new Set(["ts", "m4s", "m4f", "cmfv", "cmfa", "cmft"]);

export type MediaKind = MediaEntry["kind"];

export function classifyMedia(url: string, contentType: string | undefined): MediaKind | undefined {
  if (!parseHttpUrl(url)) return undefined;
  const mime = contentType?.split(";")[0]?.trim().toLowerCase() ?? "";
  const ext = extensionOf(filenameFromUrl(url));
  if (HLS_TYPES.has(mime) || ext === "m3u8") return "hls";
  if (DASH_TYPES.has(mime) || ext === "mpd") return "dash";
  if (SEGMENT_TYPES.has(mime) || SEGMENT_EXTENSIONS.has(ext)) return undefined;
  if (mime.startsWith("video/")) return "video";
  if (mime.startsWith("audio/")) return "audio";
  return undefined;
}

export interface HeaderLike {
  name: string;
  value?: string | undefined;
}

export function header(headers: readonly HeaderLike[] | undefined, name: string): string | undefined {
  const lower = name.toLowerCase();
  return headers?.find((h) => h.name.toLowerCase() === lower)?.value;
}

/** Full size from `Content-Range: bytes a-b/total` (206) or `Content-Length` (200). */
export function sizeFromHeaders(statusCode: number, headers: readonly HeaderLike[] | undefined): number | undefined {
  if (statusCode === 206) {
    const range = header(headers, "content-range");
    const total = range?.match(/\/(\d+)\s*$/)?.[1];
    return total ? Number(total) : undefined;
  }
  const length = header(headers, "content-length");
  if (length && /^\d+$/.test(length.trim())) {
    const n = Number(length.trim());
    return n > 0 ? n : undefined;
  }
  return undefined;
}

/** Per-tab media lists, newest last, de-duplicated by URL, capped. */
export class TabMediaStore {
  private readonly tabs = new Map<number, MediaEntry[]>();

  constructor(
    private readonly onChange: (snapshot: Record<string, MediaEntry[]>) => void = () => {},
    private readonly maxPerTab = 50,
  ) {}

  add(tabId: number, entry: MediaEntry): boolean {
    if (tabId < 0) return false;
    const url = stripFragment(entry.url);
    const list = this.tabs.get(tabId) ?? [];
    const existing = list.findIndex((e) => e.url === url);
    if (existing !== -1) {
      const old = list[existing];
      // Keep what we knew; a later range response may add the size.
      if (old && old.size === undefined && entry.size !== undefined) old.size = entry.size;
      return false;
    }
    list.push({ ...entry, url });
    while (list.length > this.maxPerTab) list.shift();
    this.tabs.set(tabId, list);
    this.onChange(this.snapshot());
    return true;
  }

  list(tabId: number): MediaEntry[] {
    return [...(this.tabs.get(tabId) ?? [])];
  }

  clear(tabId: number): void {
    if (this.tabs.delete(tabId)) this.onChange(this.snapshot());
  }

  snapshot(): Record<string, MediaEntry[]> {
    return Object.fromEntries([...this.tabs].map(([id, list]) => [String(id), list]));
  }

  /** Restores a snapshot (service workers lose memory when they sleep). */
  load(snapshot: unknown): void {
    if (typeof snapshot !== "object" || snapshot === null) return;
    for (const [id, list] of Object.entries(snapshot)) {
      const tabId = Number(id);
      if (Number.isInteger(tabId) && Array.isArray(list) && !this.tabs.has(tabId)) {
        this.tabs.set(tabId, list as MediaEntry[]);
      }
    }
  }
}
