// SPDX-License-Identifier: GPL-3.0-or-later

import { DEFAULT_FILE_TYPES } from "./constants";
import { normalizeSitePattern } from "./url";

/** User-adjustable behaviour, stored in `storage.local` under {@link SETTINGS_KEY}. */
export interface Settings {
  /** Master switch for catching browser downloads (the popup toggle). */
  captureEnabled: boolean;
  /** Extensions (lower case, no dot) to catch; empty means every type. */
  fileTypes: string[];
  /** Leave smaller downloads to the browser. 0 = no minimum. Unknown sizes are caught. */
  minSizeBytes: number;
  /** Sites never captured, matched against the download URL and its referrer. */
  excludedSites: string[];
  /** Catch downloads from private / incognito windows too. Off by default. */
  captureIncognito: boolean;
  /** How long the browser waits for the daemon to accept a download before keeping it. */
  handoffTimeoutMs: number;
  /** Warm up connections to download hosts the user hovers. */
  preconnect: boolean;
  /** Watch responses for audio/video/HLS/DASH and list them in the popup. */
  sniffMedia: boolean;
  /**
   * What holding Alt while clicking a link does: let the browser keep that
   * download (`browser`), send it to Whoosh even when the rules would not
   * (`whoosh`), or nothing (`off`).
   */
  altClick: AltClick;
  /** Find a fresh address by itself when a captured download's link expires. */
  selfHealing: boolean;
}

export type AltClick = "browser" | "whoosh" | "off";
export const ALT_CLICK_VALUES: readonly AltClick[] = ["browser", "whoosh", "off"];

export const SETTINGS_KEY = "settings";

export const DEFAULT_SETTINGS: Readonly<Settings> = Object.freeze({
  captureEnabled: true,
  fileTypes: [...DEFAULT_FILE_TYPES],
  minSizeBytes: 0,
  excludedSites: [],
  captureIncognito: false,
  handoffTimeoutMs: 4000,
  preconnect: true,
  sniffMedia: true,
  altClick: "browser",
  selfHealing: true,
});

function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function count(value: unknown, fallback: number, min: number, max: number): number {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(max, Math.max(min, Math.round(value)))
    : fallback;
}

function strings(value: unknown, fallback: readonly string[], clean: (s: string) => string): string[] {
  if (!Array.isArray(value)) return [...fallback];
  const out = new Set<string>();
  for (const item of value) {
    if (typeof item !== "string") continue;
    const c = clean(item);
    if (c) out.add(c);
  }
  return [...out];
}

function oneOf<T extends string>(value: unknown, allowed: readonly T[], fallback: T): T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;
}

/** Accepts whatever is in storage (old versions, hand edits) and returns valid settings. */
export function normalizeSettings(raw: unknown): Settings {
  const r = (typeof raw === "object" && raw !== null ? raw : {}) as Record<string, unknown>;
  const d = DEFAULT_SETTINGS;
  return {
    captureEnabled: bool(r["captureEnabled"], d.captureEnabled),
    fileTypes: strings(r["fileTypes"], d.fileTypes, (s) => s.trim().toLowerCase().replace(/^\*?\./, "")),
    minSizeBytes: count(r["minSizeBytes"], d.minSizeBytes, 0, Number.MAX_SAFE_INTEGER),
    excludedSites: strings(r["excludedSites"], d.excludedSites, normalizeSitePattern),
    captureIncognito: bool(r["captureIncognito"], d.captureIncognito),
    handoffTimeoutMs: count(r["handoffTimeoutMs"], d.handoffTimeoutMs, 500, 30_000),
    preconnect: bool(r["preconnect"], d.preconnect),
    sniffMedia: bool(r["sniffMedia"], d.sniffMedia),
    altClick: oneOf(r["altClick"], ALT_CLICK_VALUES, d.altClick),
    selfHealing: bool(r["selfHealing"], d.selfHealing),
  };
}

/** The slice of `chrome.storage.StorageArea` this module uses. */
export interface KeyValueStore {
  get(key: string): Promise<Record<string, unknown>>;
  set(items: Record<string, unknown>): Promise<void>;
}

export async function loadSettings(store: KeyValueStore): Promise<Settings> {
  const items = await store.get(SETTINGS_KEY);
  return normalizeSettings(items[SETTINGS_KEY]);
}

export async function updateSettings(store: KeyValueStore, patch: Partial<Settings>): Promise<Settings> {
  const current = await loadSettings(store);
  const next = normalizeSettings({ ...current, ...patch });
  await store.set({ [SETTINGS_KEY]: next });
  return next;
}
