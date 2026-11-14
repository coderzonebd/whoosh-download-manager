// SPDX-License-Identifier: Apache-2.0
// Queues and the scheduler (PROTOCOL.md "Queues and the scheduler"). A daemon
// without the `queues` capability answers every queue method with -32006:
// then `supported` is false and the UI shows only the main queue, as before
// queues existed, and never surfaces that error.

import { ErrorCode, MAIN_QUEUE, type AfterDone, type Queue, type QueueFinishedEvent } from "@whoosh/api-client";
import { isWhooshRpcError } from "@whoosh/api-client/errors";

export const QUEUES_CAPABILITY = "queues";

/** A pending after-done action announced by `event.queue.finished`. */
export interface PendingAction {
  name: string;
  action: Exclude<AfterDone, "none">;
  atMs: number;
}

export function isUnsupported(err: unknown): boolean {
  return isWhooshRpcError(err) && err.code === ErrorCode.Unsupported;
}

/** Queue names: 1–64 characters without control characters (PROTOCOL.md). */
export function queueNameProblem(name: string): "empty" | "long" | "control" | undefined {
  if (name.length === 0) return "empty";
  if ([...name].length > 64) return "long";
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f-\u009f]/.test(name)) return "control";
  return undefined;
}

export class QueueStore {
  /** undefined until known; false when the daemon has no queues. */
  supported = $state<boolean | undefined>(undefined);
  /** The main queue first, then named queues by name (as the daemon sends them). */
  list = $state.raw<Queue[]>([]);
  /** After-done actions waiting to run, by queue name. */
  pending = $state.raw<PendingAction[]>([]);

  get enabled(): boolean {
    return this.supported === true;
  }

  /** Named queues only (not "main"). */
  get named(): Queue[] {
    return this.list.filter((q) => q.name !== MAIN_QUEUE);
  }

  get(name: string): Queue | undefined {
    return this.list.find((q) => q.name === name);
  }

  has(name: string): boolean {
    return name === MAIN_QUEUE || this.list.some((q) => q.name === name);
  }

  replace(list: Queue[]): void {
    this.list = list;
    // A pending action for a queue that no longer exists can't be cancelled.
    if (this.pending.some((p) => !this.has(p.name))) this.pending = this.pending.filter((p) => this.has(p.name));
  }

  /** `event.queue.finished`: show a countdown unless there is nothing to do. */
  finished(e: QueueFinishedEvent): void {
    const rest = this.pending.filter((p) => p.name !== e.name);
    this.pending = e.action === "none" ? rest : [...rest, { name: e.name, action: e.action, atMs: e.atMs }];
  }

  dismiss(name: string): void {
    this.pending = this.pending.filter((p) => p.name !== name);
  }

  /** Drop countdowns that ran out more than `graceMs` ago. */
  expire(now: number, graceMs = 5_000): void {
    if (this.pending.some((p) => p.atMs + graceMs < now)) this.pending = this.pending.filter((p) => p.atMs + graceMs >= now);
  }

  disable(): void {
    this.supported = false;
    this.list = [];
    this.pending = [];
  }
}
