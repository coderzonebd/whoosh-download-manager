// SPDX-License-Identifier: Apache-2.0
//
// The download list as reactive state, built to stay fast at 10 000+ rows:
// - each download is one `$state.raw` object, so a progress event re-renders
//   only that row, and only if the row is on screen;
// - the filtered + sorted order is recomputed on *structural* changes
//   (add / remove / state / filter / sort), never on progress ticks. Rows do
//   not jump under the pointer while you aim at them; a re-sort happens on
//   the next structural change or when you click a column header.

import type { DownloadState, DownloadSummary, ProgressItem, StateEvent } from "@whoosh/api-client";
import { categoryForFilename, isCategory, type Category, CATEGORIES } from "@whoosh/api-client/categories";
import { untrack } from "svelte";
import { SvelteSet } from "svelte/reactivity";
import { fraction } from "../format.ts";

export type StateGroup = "all" | "active" | "queued" | "paused" | "completed" | "attention";
export const STATE_GROUPS: readonly StateGroup[] = ["all", "active", "queued", "paused", "completed", "attention"];

export type View =
  | { kind: "group"; group: StateGroup }
  | { kind: "category"; category: Category }
  /** A queue's unfinished downloads ("main" or a named queue). */
  | { kind: "queue"; queue: string };

export const GROUP_STATES: Record<Exclude<StateGroup, "all">, readonly DownloadState[]> = {
  active: ["probing", "downloading", "verifying"],
  queued: ["queued"],
  paused: ["paused"],
  completed: ["completed"],
  attention: ["failed", "needs_refresh"],
};
/** The queue a download belongs to; absent means the main queue. */
export function queueOf(d: DownloadSummary): string {
  return d.queue || "main";
}

export const ACTIVE_STATES: ReadonlySet<DownloadState> = new Set(GROUP_STATES.active);
export const PAUSABLE: ReadonlySet<DownloadState> = new Set(["queued", "probing", "downloading"]);
export const RESUMABLE: ReadonlySet<DownloadState> = new Set(["paused", "failed"]);

export type SortKey = "name" | "size" | "progress" | "status" | "speed" | "eta" | "connections" | "added";
export interface Sort {
  key: SortKey;
  dir: "asc" | "desc";
}

/** Status sort order: what needs you first, then what is moving, then what is done. */
const STATUS_RANK: Record<DownloadState, number> = {
  needs_refresh: 0,
  failed: 1,
  downloading: 2,
  probing: 3,
  verifying: 4,
  queued: 5,
  paused: 6,
  completed: 7,
};

/** Speed samples kept per download: 30 s at the protocol's 4 Hz. */
export const HISTORY_LENGTH = 120;

export class Download {
  readonly id: string;
  data: DownloadSummary = $state.raw() as unknown as DownloadSummary;
  /** Recent speed samples (bytes/s), oldest first. Not reactive: read it next to `data`. */
  readonly history: number[] = [];
  #nameFor = "";
  #nameKey = "";

  constructor(d: DownloadSummary) {
    this.id = d.id;
    this.data = d;
  }

  get category(): Category {
    const c = this.data.category;
    return isCategory(c) ? c : categoryForFilename(this.data.filename);
  }

  get fraction(): number | undefined {
    if (this.data.state === "completed") return 1;
    return fraction(this.data.downloadedBytes, this.data.totalBytes);
  }

  /** Lower-cased file name, cached until the name changes. */
  get searchKey(): string {
    if (this.#nameFor !== this.data.filename) {
      this.#nameFor = this.data.filename;
      this.#nameKey = this.data.filename.toLocaleLowerCase();
    }
    return this.#nameKey;
  }
}

export interface Counts {
  groups: Record<StateGroup, number>;
  categories: Record<Category, number>;
  /** Unfinished downloads in the main queue. */
  mainQueue: number;
  /** Unfinished downloads per queue name (including "main"). */
  queues: Record<string, number>;
}

function matches(d: Download, view: View): boolean {
  const s = d.data.state;
  switch (view.kind) {
    case "group":
      return view.group === "all" || GROUP_STATES[view.group].includes(s);
    case "category":
      return d.category === view.category;
    case "queue":
      return s !== "completed" && queueOf(d.data) === view.queue;
  }
}

export class DownloadStore {
  readonly #items = new Map<string, Download>();
  #collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

  /** Bumped on every change that can affect filtering, grouping or order. */
  version = $state(0);
  loaded = $state(false);
  loadError = $state<string | undefined>(undefined);

  view = $state.raw<View>({ kind: "group", group: "all" });
  query = $state("");
  sort = $state.raw<Sort>({ key: "added", dir: "desc" });

  readonly selection = new SvelteSet<string>();
  /** The row with keyboard focus (and the one the detail panel shows). */
  activeId = $state<string | undefined>(undefined);
  #anchorId: string | undefined;

  readonly visible: Download[] = $derived.by(() => {
    void this.version;
    const view = this.view;
    const query = this.query.trim().toLocaleLowerCase();
    const sort = this.sort;
    return untrack(() => this.#compute(view, query, sort));
  });

  readonly #indexById: Map<string, number> = $derived(new Map(this.visible.map((d, i) => [d.id, i])));

  readonly counts: Counts = $derived.by(() => {
    void this.version;
    return untrack(() => this.#count());
  });

  get size(): number {
    void this.version;
    return this.#items.size;
  }

  /** Every download, in no particular order (reactive on structural changes). */
  all(): Download[] {
    void this.version;
    return [...this.#items.values()];
  }

  get(id: string | undefined): Download | undefined {
    return id === undefined ? undefined : this.#items.get(id);
  }

  indexOf(id: string | undefined): number {
    return id === undefined ? -1 : (this.#indexById.get(id) ?? -1);
  }

  /** Locale-aware name sorting (call when the UI language changes). */
  setCollationLocale(locale: string): void {
    this.#collator = new Intl.Collator(locale, { numeric: true, sensitivity: "base" });
    if (this.sort.key === "name") this.version++;
  }

  // ---------------------------------------------------------------- updates from the daemon

  replaceAll(list: DownloadSummary[]): void {
    const seen = new Set<string>();
    for (const d of list) {
      seen.add(d.id);
      const existing = this.#items.get(d.id);
      if (existing) existing.data = d;
      else this.#items.set(d.id, new Download(d));
    }
    for (const id of [...this.#items.keys()]) if (!seen.has(id)) this.#items.delete(id);
    this.#prune();
    this.loaded = true;
    this.loadError = undefined;
    this.version++;
  }

  upsert(d: DownloadSummary): void {
    const existing = this.#items.get(d.id);
    if (existing) existing.data = d;
    else this.#items.set(d.id, new Download(d));
    this.version++;
  }

  applyProgress(items: readonly ProgressItem[]): void {
    for (const p of items) {
      const d = this.#items.get(p.id);
      if (!d) continue;
      const next: DownloadSummary = {
        ...d.data,
        downloadedBytes: p.downloadedBytes,
        speedBps: p.speedBps,
        connections: p.connections,
      };
      if (p.totalBytes !== undefined) next.totalBytes = p.totalBytes;
      if (p.etaSecs !== undefined) next.etaSecs = p.etaSecs;
      else delete next.etaSecs;
      d.data = next;
      if (ACTIVE_STATES.has(next.state)) {
        d.history.push(p.speedBps);
        if (d.history.length > HISTORY_LENGTH) d.history.splice(0, d.history.length - HISTORY_LENGTH);
      }
    }
  }

  /** Returns the previous state, or undefined for an unknown download. */
  applyState(ev: StateEvent): DownloadState | undefined {
    const d = this.#items.get(ev.id);
    if (!d) return undefined;
    const prev = d.data.state;
    const next: DownloadSummary = { ...d.data, state: ev.state };
    if (ev.error) next.error = ev.error;
    else delete next.error;
    if (ev.state !== "downloading") {
      next.speedBps = 0;
      delete next.etaSecs;
      next.connections = 0;
    }
    if (ev.state === "completed") {
      next.completedAt ??= Date.now();
      if (next.totalBytes !== undefined) next.downloadedBytes = next.totalBytes;
    }
    d.data = next;
    if (prev !== ev.state) this.version++;
    return prev;
  }

  remove(ids: readonly string[]): void {
    let changed = false;
    for (const id of ids) changed = this.#items.delete(id) || changed;
    if (!changed) return;
    this.#prune();
    this.version++;
  }

  // ---------------------------------------------------------------- selection

  selectOnly(id: string | undefined): void {
    this.selection.clear();
    if (id !== undefined) this.selection.add(id);
    this.activeId = id;
    this.#anchorId = id;
  }

  toggle(id: string): void {
    if (this.selection.has(id)) this.selection.delete(id);
    else this.selection.add(id);
    this.activeId = id;
    this.#anchorId = id;
  }

  /** Shift+click / Shift+arrow: select from the anchor to `id` in visible order. */
  selectRange(id: string): void {
    const anchor = this.#anchorId ?? this.activeId ?? id;
    const a = this.indexOf(anchor);
    const b = this.indexOf(id);
    if (a < 0 || b < 0) return this.selectOnly(id);
    const [lo, hi] = a < b ? [a, b] : [b, a];
    this.selection.clear();
    for (let i = lo; i <= hi; i++) {
      const d = this.visible[i];
      if (d) this.selection.add(d.id);
    }
    this.activeId = id;
  }

  selectAll(): void {
    for (const d of this.visible) this.selection.add(d.id);
  }

  clearSelection(): void {
    this.selection.clear();
  }

  /** Selected downloads in visible order; falls back to the active row. */
  selected(): Download[] {
    const out: Download[] = [];
    for (const id of this.selection) {
      const d = this.#items.get(id);
      if (d) out.push(d);
    }
    if (out.length === 0) {
      const a = this.get(this.activeId);
      if (a) out.push(a);
    }
    return out;
  }

  // ---------------------------------------------------------------- internals

  #prune(): void {
    for (const id of [...this.selection]) if (!this.#items.has(id)) this.selection.delete(id);
    if (this.activeId !== undefined && !this.#items.has(this.activeId)) this.activeId = undefined;
    if (this.#anchorId !== undefined && !this.#items.has(this.#anchorId)) this.#anchorId = undefined;
  }

  #compute(view: View, query: string, sort: Sort): Download[] {
    const out: Download[] = [];
    for (const d of this.#items.values()) {
      if (!matches(d, view)) continue;
      if (query && !d.searchKey.includes(query)) continue;
      out.push(d);
    }
    const dir = sort.dir === "asc" ? 1 : -1;
    const key = sort.key;
    const col = this.#collator;
    const num = (d: Download): number => {
      const x = d.data;
      switch (key) {
        case "size":
          return x.totalBytes ?? -1;
        case "progress":
          return d.fraction ?? -1;
        case "status":
          return STATUS_RANK[x.state];
        case "speed":
          return x.speedBps;
        case "eta":
          return x.etaSecs ?? Number.MAX_SAFE_INTEGER;
        case "connections":
          return x.connections;
        default:
          return x.addedAt;
      }
    };
    out.sort((a, b) => {
      const c = key === "name" ? col.compare(a.data.filename, b.data.filename) : num(a) - num(b);
      if (c !== 0) return c * dir;
      return b.data.addedAt - a.data.addedAt || (a.id < b.id ? -1 : 1);
    });
    return out;
  }

  #count(): Counts {
    const groups: Record<StateGroup, number> = { all: 0, active: 0, queued: 0, paused: 0, completed: 0, attention: 0 };
    const categories = Object.fromEntries(CATEGORIES.map((c) => [c, 0])) as Record<Category, number>;
    const queues: Record<string, number> = { main: 0 };
    for (const d of this.#items.values()) {
      const s = d.data.state;
      groups.all++;
      for (const g of ["active", "queued", "paused", "completed", "attention"] as const) {
        if (GROUP_STATES[g].includes(s)) groups[g]++;
      }
      categories[d.category]++;
      if (s !== "completed") {
        const q = queueOf(d.data);
        queues[q] = (queues[q] ?? 0) + 1;
      }
    }
    return { groups, categories, mainQueue: queues.main ?? 0, queues };
  }
}
