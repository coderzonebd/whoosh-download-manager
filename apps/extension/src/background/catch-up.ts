// SPDX-License-Identifier: GPL-3.0-or-later

// Events from the daemon only arrive while the native port is open, and the
// port closes after 5 idle minutes (or with the browser). Whenever it opens
// again — or the host reports that the daemon came up — the extension asks
// for the current state of the downloads it cares about instead of relying
// on events it may have missed.

import type { DownloadState, DownloadSummary } from "@whoosh/api-client";

import type { DaemonCalls } from "./capture/handoff";

/** Every state except `completed`: a remembered download missing from this list is finished or gone. */
export const UNFINISHED_STATES: readonly DownloadState[] = [
  "queued",
  "probing",
  "downloading",
  "paused",
  "needs_refresh",
  "verifying",
  "failed",
];

/**
 * All unfinished downloads, newest first, page by page (native messaging
 * answers are limited to 1 MiB, PROTOCOL.md §1).
 */
export async function listUnfinished(daemon: DaemonCalls, pageSize = 100, maxPages = 100): Promise<DownloadSummary[]> {
  const all: DownloadSummary[] = [];
  for (let page = 0; page < maxPages; page++) {
    const batch = await daemon.call("download.list", {
      states: [...UNFINISHED_STATES],
      offset: page * pageSize,
      limit: pageSize,
    });
    all.push(...batch);
    if (batch.length < pageSize) return all;
  }
  throw new Error(`more than ${pageSize * maxPages} unfinished downloads; catch-up incomplete`);
}

/**
 * Runs `task` when triggered, never twice at once; a trigger during a run
 * schedules exactly one more run afterwards.
 */
export class Coalescer {
  private running: Promise<void> | undefined;
  private again = false;

  constructor(
    private readonly task: () => Promise<void>,
    private readonly onError: (error: unknown) => void = () => {},
  ) {}

  trigger(): Promise<void> {
    if (this.running) {
      this.again = true;
      return this.running;
    }
    this.running = (async () => {
      do {
        this.again = false;
        try {
          await this.task();
        } catch (error) {
          this.onError(error);
        }
      } while (this.again);
      this.running = undefined;
    })();
    return this.running;
  }
}

/** Remembers whether the daemon was up, to notice it coming up. */
export class RisingEdge {
  private last = false;

  /** True when `value` is true and the previous one was not. */
  update(value: boolean): boolean {
    const rose = value && !this.last;
    this.last = value;
    return rose;
  }
}
