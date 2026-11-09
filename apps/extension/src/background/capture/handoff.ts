// SPDX-License-Identifier: GPL-3.0-or-later

// Handing a browser download to the daemon without ever losing it.
//
// The browser's own download keeps running (Firefox) or waits for us
// (Chromium's onDeterminingFilename) while we ask the daemon to take over.
// Exactly one of them must end up owning the file:
//
//   daemon says yes in time  → cancel the browser copy. If the browser had
//                              already finished, or refuses to cancel, the
//                              browser keeps it and the daemon's copy is
//                              removed again.
//   daemon says no / errors  → the browser keeps it.
//   no answer before deadline → the browser keeps it. If a yes arrives
//                              later, the daemon's copy is removed.
//
// The browser copy is cancelled only after the daemon has confirmed, so a
// missing host, a stopped daemon or a slow one can never cost the user the
// download. The worst case is a duplicate (when a compensating removal
// fails), never a loss.

import type { AddParams, AddResult, MethodMap } from "@vayu/api-client";

export type BrowserDownloadState = "in_progress" | "interrupted" | "complete" | "missing";

/** The browser's download controls, as the hand-off needs them. */
export interface DownloadControl {
  cancel(id: number): Promise<void>;
  erase(id: number): Promise<void>;
  state(id: number): Promise<BrowserDownloadState>;
}

export interface DaemonCalls {
  call<M extends keyof MethodMap>(method: M, params: MethodMap[M]["params"]): Promise<MethodMap[M]["result"]>;
}

export type KeptReason =
  /** The daemon (or the host) answered with an error, or was unreachable. */
  | "error"
  /** No answer before the deadline. */
  | "timeout"
  /** The browser finished the file before it could be cancelled. */
  | "browser-finished"
  /** The browser would not cancel its copy. */
  | "cancel-failed";

export type LateOutcome =
  /** The daemon accepted after the deadline; its copy was removed again. */
  | { outcome: "compensated"; daemonId: string }
  /** It accepted late and the removal failed: the file downloads twice. */
  | { outcome: "duplicate"; daemonId: string; error: unknown }
  /** It failed or never answered; nothing to undo. */
  | { outcome: "nothing" };

export type HandoffResult =
  | { outcome: "sent"; daemonId: string }
  | {
      outcome: "kept";
      reason: KeptReason;
      error?: unknown;
      /** For "timeout": settles when the late answer has been dealt with. */
      late?: Promise<LateOutcome>;
      /** For "browser-finished" / "cancel-failed": the daemon copy that was removed (or not). */
      compensation?: LateOutcome;
    };

export interface HandoffOptions {
  /** Deadline for building the request and the daemon's answer. */
  timeoutMs: number;
  /**
   * Lets the browser continue with its own download (Chromium: `suggest()`).
   * Called exactly once on every path; on the "sent" path only after the
   * browser copy has been cancelled.
   */
  release(): void;
}

export interface HandoffDeps {
  daemon: DaemonCalls;
  downloads: DownloadControl;
}

const TIMEOUT = Symbol("timeout");

async function removeFromDaemon(daemon: DaemonCalls, daemonId: string): Promise<LateOutcome> {
  try {
    await daemon.call("download.remove", { ids: [daemonId], deleteFiles: true });
    return { outcome: "compensated", daemonId };
  } catch (error) {
    return { outcome: "duplicate", daemonId, error };
  }
}

/**
 * Runs one hand-off. `prepare` builds the `download.add` params (reads
 * cookies); it runs inside the deadline.
 */
export async function handOff(
  browserId: number,
  prepare: () => Promise<AddParams>,
  deps: HandoffDeps,
  opts: HandoffOptions,
): Promise<HandoffResult> {
  let released = false;
  const release = () => {
    if (released) return;
    released = true;
    try {
      opts.release();
    } catch {
      // The browser may already have dropped the download; nothing to keep.
    }
  };

  const added: Promise<AddResult> = prepare().then((params) => deps.daemon.call("download.add", params));
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<typeof TIMEOUT>((resolve) => {
    timer = setTimeout(() => resolve(TIMEOUT), opts.timeoutMs);
  });

  let answer: AddResult | typeof TIMEOUT;
  try {
    answer = await Promise.race([added, deadline]);
  } catch (error) {
    clearTimeout(timer);
    release();
    return { outcome: "kept", reason: "error", error };
  }
  clearTimeout(timer);

  if (answer === TIMEOUT) {
    release();
    const late = added.then(
      (result) => removeFromDaemon(deps.daemon, result.id),
      (): LateOutcome => ({ outcome: "nothing" }),
    );
    return { outcome: "kept", reason: "timeout", late };
  }

  // The daemon owns a copy now. Take the browser's away.
  const daemonId = answer.id;
  let cancelError: unknown;
  try {
    await deps.downloads.cancel(browserId);
  } catch (error) {
    cancelError = error;
  }
  release();

  let state: BrowserDownloadState;
  try {
    state = await deps.downloads.state(browserId);
  } catch {
    state = "missing";
  }

  if (state === "complete" || state === "in_progress") {
    // Too late (it finished) or the cancel did not take: the browser keeps it.
    const compensation = await removeFromDaemon(deps.daemon, daemonId);
    return {
      outcome: "kept",
      reason: state === "complete" ? "browser-finished" : "cancel-failed",
      error: cancelError,
      compensation,
    };
  }

  try {
    await deps.downloads.erase(browserId);
  } catch {
    // Cosmetic: the cancelled entry stays in the browser's list.
  }
  return { outcome: "sent", daemonId };
}
