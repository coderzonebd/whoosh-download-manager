// SPDX-License-Identifier: GPL-3.0-or-later

// Glue between the browser's download events and the pure pieces: rules,
// request context, hand-off.

import type { Settings } from "../../shared/settings";
import { buildAddParams, cookiesFor, privateCookieStore, type CookieSource } from "./context";
import { handOff, type DaemonCalls, type DownloadControl, type HandoffResult } from "./handoff";
import { evaluateCapture, type CandidateDownload, type SkipReason } from "./rules";
import type { BypassTracker, RequestMethodTracker } from "./trackers";

export interface CaptureDeps {
  daemon: DaemonCalls;
  downloads: DownloadControl;
  cookies: CookieSource;
  settings(): Promise<Settings>;
  bypass: BypassTracker;
  methods: RequestMethodTracker;
  userAgent: string;
  isFirefox: boolean;
  now(): number;
  /** Told about every decision, for the popup and debugging. */
  report?(candidate: CandidateDownload, result: CaptureResult): void;
}

export type CaptureResult = { outcome: "skipped"; reason: SkipReason } | HandoffResult;

export class CaptureController {
  constructor(private readonly deps: CaptureDeps) {}

  /**
   * Called for every new browser download. `release` lets the browser go on
   * with it (Chromium's `suggest`; a no-op on Firefox) and is called exactly
   * once.
   */
  async onBrowserDownload(candidate: CandidateDownload, release: () => void): Promise<CaptureResult> {
    let result: CaptureResult;
    try {
      result = await this.decideAndHandOff(candidate, release);
    } catch (error) {
      // Defensive: nothing above should throw, but the browser must never be
      // left waiting.
      release();
      result = { outcome: "kept", reason: "error", error };
    }
    this.deps.report?.(candidate, result);
    return result;
  }

  private async decideAndHandOff(candidate: CandidateDownload, release: () => void): Promise<CaptureResult> {
    const { deps } = this;
    const settings = await deps.settings();
    const now = deps.now();
    const decision = evaluateCapture(candidate, settings, {
      bypass: settings.captureEnabled && deps.bypass.consume(now),
      nonGetRequest: deps.methods.wasNonGet([candidate.url, candidate.originalUrl], now),
    });
    if (!decision.capture) {
      release();
      return { outcome: "skipped", reason: decision.reason };
    }

    const storeId =
      candidate.cookieStoreId ?? (candidate.incognito ? privateCookieStore(deps.isFirefox) : undefined);
    const prepare = async () =>
      buildAddParams({
        url: candidate.url,
        filename: candidate.filename,
        cookies: await cookiesFor(deps.cookies, candidate.url, storeId),
        referrer: candidate.referrer,
        userAgent: deps.userAgent,
      });

    return handOff(candidate.id, prepare, deps, { timeoutMs: settings.handoffTimeoutMs, release });
  }
}
