// SPDX-License-Identifier: Apache-2.0
// Polls `download.get` for the downloads that are on screen (and the one in
// the detail panel) so their bars can show per-segment progress. Only rows
// that are moving, or that we have never fetched, are polled — a page of
// 30 visible rows costs at most 30 small local calls per second.
// Proposed contract change (see packages/ui/README.md): put segments in the
// progress event and this poller goes away.

import type { DownloadDetail, VayuClient } from "@vayu/api-client";
import { SvelteMap } from "svelte/reactivity";
import type { DownloadStore } from "./downloads.svelte.ts";

export interface SegmentSnapshot {
  detail: DownloadDetail;
  at: number;
}

const POLL_MS = 1000;
const MAX_PER_TICK = 40;

export class SegmentCache {
  readonly #client: VayuClient;
  readonly #store: DownloadStore;
  readonly #snapshots = new SvelteMap<string, SegmentSnapshot>();
  #visible = new Set<string>();
  #focus: string | undefined;
  #timer: ReturnType<typeof setInterval> | undefined;
  #kick: ReturnType<typeof setTimeout> | undefined;
  #inflight = new Set<string>();
  #stopped = false;

  constructor(client: VayuClient, store: DownloadStore) {
    this.#client = client;
    this.#store = store;
  }

  start(): void {
    this.#stopped = false;
    this.#timer ??= setInterval(() => void this.#tick(), POLL_MS);
  }

  stop(): void {
    this.#stopped = true;
    if (this.#timer !== undefined) clearInterval(this.#timer);
    if (this.#kick !== undefined) clearTimeout(this.#kick);
    this.#timer = undefined;
    this.#kick = undefined;
  }

  get(id: string): SegmentSnapshot | undefined {
    return this.#snapshots.get(id);
  }

  forget(ids: readonly string[]): void {
    for (const id of ids) this.#snapshots.delete(id);
  }

  /** The rows currently rendered. */
  setVisible(ids: Iterable<string>): void {
    this.#visible = new Set(ids);
    this.#soon();
  }

  /** The download in the detail panel: always polled while it is shown. */
  setFocus(id: string | undefined): void {
    if (this.#focus === id) return;
    this.#focus = id;
    this.#soon();
  }

  #soon(): void {
    if (this.#stopped || this.#kick !== undefined) return;
    this.#kick = setTimeout(() => {
      this.#kick = undefined;
      void this.#tick();
    }, 60);
  }

  #wants(id: string): boolean {
    const d = this.#store.get(id)?.data;
    if (!d) return false;
    if (d.state === "downloading" || d.state === "probing") return true;
    if (d.state === "completed") return id === this.#focus && !this.#snapshots.has(id);
    // Paused / failed / queued with partial data: fetch once.
    return d.downloadedBytes > 0 && !this.#snapshots.has(id);
  }

  async #tick(): Promise<void> {
    if (this.#stopped) return;
    const ids = new Set<string>();
    if (this.#focus !== undefined && this.#wants(this.#focus)) ids.add(this.#focus);
    for (const id of this.#visible) {
      if (ids.size >= MAX_PER_TICK) break;
      if (this.#wants(id)) ids.add(id);
    }
    await Promise.all(
      [...ids].filter((id) => !this.#inflight.has(id)).map(async (id) => {
        this.#inflight.add(id);
        try {
          const detail = await this.#client.call("download.get", { id });
          if (!this.#stopped) this.#snapshots.set(id, { detail, at: Date.now() });
        } catch {
          this.#snapshots.delete(id);
        } finally {
          this.#inflight.delete(id);
        }
      }),
    );
  }
}
