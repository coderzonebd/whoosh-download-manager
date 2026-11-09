// SPDX-License-Identifier: GPL-3.0-or-later

// Self-healing links — typed stub. Design: docs/self-healing.md.
// Implementation is Phase 2; until then a `needs_refresh` download waits for
// the user to supply a new address in the app.

import type { DownloadId, RequestContext, StateEvent } from "@vayu/api-client";

/** What the extension remembers about a download it handed over, to find a fresh URL later. */
export interface Provenance {
  daemonId: DownloadId;
  /** The page the user was on when the download started. */
  originPageUrl?: string;
  /** The URL that was handed over (often signed and short-lived). */
  url: string;
  /** The link the user clicked, when it differs from `url` (redirect chains). */
  linkUrl?: string;
  linkText?: string;
  filename?: string;
  totalBytes?: number;
  capturedAt: number;
}

export type RefreshOutcome =
  | { status: "refreshed"; url: string; request?: RequestContext }
  | { status: "failed"; reason: "origin-gone" | "no-match" | "size-mismatch" | "timeout" | "user-declined" }
  | { status: "unsupported" };

export interface SelfHealer {
  /** Called after a successful hand-off. */
  remember(provenance: Provenance): Promise<void>;
  /** Called for `event.download.state` with `state: "needs_refresh"`. */
  heal(event: StateEvent): Promise<RefreshOutcome>;
  /** Called when the daemon reports the download completed or was removed. */
  forget(id: DownloadId): Promise<void>;
}

/** Phase 0/1: does nothing; the app shows "link expired" to the user. */
export class SelfHealingStub implements SelfHealer {
  async remember(_provenance: Provenance): Promise<void> {}

  async heal(_event: StateEvent): Promise<RefreshOutcome> {
    return { status: "unsupported" };
  }

  async forget(_id: DownloadId): Promise<void> {}
}
