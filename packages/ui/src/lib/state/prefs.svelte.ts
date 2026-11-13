// SPDX-License-Identifier: Apache-2.0
// UI preferences that belong to this device, not to the daemon: layout,
// high contrast, column widths, sort. Persisted to localStorage when present.

import type { Sort, SortKey } from "./downloads.svelte.ts";

export type Layout = "classic" | "modern";

export const COLUMN_IDS = ["name", "size", "progress", "status", "speed", "eta", "connections", "added"] as const;
export type ColumnId = (typeof COLUMN_IDS)[number];

export const DEFAULT_WIDTHS: Record<ColumnId, number> = {
  name: 300,
  size: 92,
  progress: 168,
  status: 132,
  speed: 104,
  eta: 124,
  connections: 108,
  added: 112,
};
export const MIN_WIDTH = 56;
export const MAX_WIDTH = 900;

export interface PrefsData {
  layout: Layout;
  highContrast: boolean;
  widths: Record<ColumnId, number>;
  sort: Sort;
  detailOpen: boolean;
}

const KEY = "whoosh.ui.prefs.v1";
const SORT_KEYS: readonly SortKey[] = ["name", "size", "progress", "status", "speed", "eta", "connections", "added"];

function clampWidth(w: unknown, fallback: number): number {
  return typeof w === "number" && Number.isFinite(w) ? Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(w))) : fallback;
}

export function parsePrefs(raw: string | null | undefined): PrefsData {
  const d: PrefsData = { layout: "classic", highContrast: false, widths: { ...DEFAULT_WIDTHS }, sort: { key: "added", dir: "desc" }, detailOpen: true };
  if (!raw) return d;
  try {
    const p = JSON.parse(raw) as Partial<PrefsData>;
    if (p.layout === "classic" || p.layout === "modern") d.layout = p.layout;
    if (typeof p.highContrast === "boolean") d.highContrast = p.highContrast;
    if (typeof p.detailOpen === "boolean") d.detailOpen = p.detailOpen;
    if (p.widths && typeof p.widths === "object") {
      for (const c of COLUMN_IDS) d.widths[c] = clampWidth((p.widths as Record<string, unknown>)[c], DEFAULT_WIDTHS[c]);
    }
    if (p.sort && SORT_KEYS.includes(p.sort.key) && (p.sort.dir === "asc" || p.sort.dir === "desc")) d.sort = { key: p.sort.key, dir: p.sort.dir };
  } catch {
    // corrupt prefs: fall back to defaults
  }
  return d;
}

export class Prefs {
  layout = $state<Layout>("classic");
  highContrast = $state(false);
  widths = $state<Record<ColumnId, number>>({ ...DEFAULT_WIDTHS });
  detailOpen = $state(true);
  readonly #storage: Storage | null;

  constructor(storage: Storage | null) {
    this.#storage = storage;
    let raw: string | null = null;
    try {
      raw = storage?.getItem(KEY) ?? null;
    } catch {
      // storage blocked (private mode, sandboxed iframe)
    }
    const p = parsePrefs(raw);
    this.layout = p.layout;
    this.highContrast = p.highContrast;
    this.widths = p.widths;
    this.detailOpen = p.detailOpen;
    this.initialSort = p.sort;
  }

  /** Sort read at startup; the live sort lives in DownloadStore. */
  readonly initialSort: Sort;

  setWidth(col: ColumnId, w: number): void {
    this.widths[col] = clampWidth(w, DEFAULT_WIDTHS[col]);
  }

  save(sort: Sort): void {
    const data: PrefsData = {
      layout: this.layout,
      highContrast: this.highContrast,
      widths: { ...this.widths },
      sort,
      detailOpen: this.detailOpen,
    };
    try {
      this.#storage?.setItem(KEY, JSON.stringify(data));
    } catch {
      // quota or blocked: preferences just won't persist
    }
  }
}
