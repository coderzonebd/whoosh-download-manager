// SPDX-License-Identifier: Apache-2.0
// Toast queue. Timers pause while the pointer or focus is inside the toast
// region and while the window is hidden, so nothing vanishes unread.

export type ToastTone = "info" | "success" | "warning" | "error";

export interface ToastAction {
  label: string;
  run: () => void;
}

export interface Toast {
  id: number;
  tone: ToastTone;
  message: string;
  action?: ToastAction;
}

interface Timer {
  remaining: number;
  started: number;
  handle: ReturnType<typeof setTimeout> | undefined;
}

const DEFAULT_MS: Record<ToastTone, number> = { info: 4500, success: 4500, warning: 8000, error: 9000 };
const MAX_VISIBLE = 4;

export class ToastStore {
  items = $state<Toast[]>([]);
  #next = 1;
  #timers = new Map<number, Timer>();
  #held = false;

  push(t: Omit<Toast, "id">, durationMs?: number): number {
    const id = this.#next++;
    const next = [...this.items, { ...t, id }];
    // Over the limit: drop the oldest routine toast first, so a warning or
    // error that still needs a decision is not pushed out by "finished" news.
    while (next.length > MAX_VISIBLE) {
      const routine = next.findIndex((x) => x.tone === "info" || x.tone === "success");
      next.splice(routine >= 0 ? routine : 0, 1);
    }
    this.items = next;
    for (const old of [...this.#timers.keys()]) {
      if (!this.items.some((x) => x.id === old)) this.#clear(old);
    }
    const ms = durationMs ?? DEFAULT_MS[t.tone] + (t.action ? 3000 : 0);
    const timer: Timer = { remaining: ms, started: Date.now(), handle: undefined };
    this.#timers.set(id, timer);
    if (!this.#held) this.#arm(id, timer);
    return id;
  }

  dismiss(id: number): void {
    this.#clear(id);
    this.items = this.items.filter((t) => t.id !== id);
  }

  /** Pause every timer (pointer/focus inside the region, or window hidden). */
  hold(): void {
    if (this.#held) return;
    this.#held = true;
    const now = Date.now();
    for (const t of this.#timers.values()) {
      if (t.handle !== undefined) clearTimeout(t.handle);
      t.handle = undefined;
      t.remaining -= now - t.started;
    }
  }

  release(): void {
    if (!this.#held) return;
    this.#held = false;
    for (const [id, t] of this.#timers) this.#arm(id, t);
  }

  clear(): void {
    for (const id of [...this.#timers.keys()]) this.#clear(id);
    this.items = [];
  }

  #arm(id: number, t: Timer): void {
    t.started = Date.now();
    t.handle = setTimeout(() => this.dismiss(id), Math.max(800, t.remaining));
  }

  #clear(id: number): void {
    const t = this.#timers.get(id);
    if (t?.handle !== undefined) clearTimeout(t.handle);
    this.#timers.delete(id);
  }
}
