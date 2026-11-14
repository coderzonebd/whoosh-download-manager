// SPDX-License-Identifier: GPL-3.0-or-later

// Glue between the browser's download events and the pure pieces: rules,
// request context, hand-off.

import type { Settings } from "../../shared/settings";
import type { BrowserFile } from "../healing/match";
import { buildAddParams, cookiesFor, privateCookieStore, type CookieSource } from "./context";
import { handOff, type DaemonCalls, type DownloadControl, type HandoffResult, type KeptReason } from "./handoff";
import { evaluateCapture, safetySkip, type CandidateDownload, type SkipReason } from "./rules";
import type { BypassTracker, RequestMethodTracker } from "./trackers";

/** The part of the self-healer the capture path uses. */
export interface ExpiredDownloads {
  /** The daemon id of an expired download this browser download can refresh, if any. */
  match(file: BrowserFile, settings: Settings): Promise<string | undefined>;
  /** Gives the daemon this download's address for `id`; throws when it refuses. */
  refreshFromBrowser(id: string, file: BrowserFile & { cookieStoreId?: string }): Promise<void>;
}

export interface CaptureDeps {
  daemon: DaemonCalls;
  downloads: DownloadControl;
  cookies: CookieSource;
  settings(): Promise<Settings>;
  bypass: BypassTracker;
  /** Armed by Alt+click when the setting says "send to Whoosh". */
  force?: BypassTracker;
  methods: RequestMethodTracker;
  /** Store build: true for URLs the store extension must leave alone. */
  blocked?(urls: (string | undefined)[]): boolean;
  /** Self-healing: a new browser download may be the fresh copy of an expired one. */
  expired?: ExpiredDownloads;
  userAgent: string;
  isFirefox: boolean;
  now(): number;
  /** Told about every decision, for the popup and debugging. */
  report?(candidate: CandidateDownload, result: CaptureResult): void;
}

export type CaptureResult =
  | { outcome: "skipped"; reason: SkipReason }
  | HandoffResult
  /** The download was the fresh copy of an expired whoosh download, which now continues. */
  | { outcome: "healed"; daemonId: string }
  | { outcome: "kept"; reason: KeptReason; error?: unknown; healing: string };

const TIMEOUT = Symbol("timeout");

export class CaptureController {
  constructor(private readonly deps: CaptureDeps) {}

  /**
   * Called for every new browser download. `release` lets the browser go on
   * with it (Chromium's `suggest`; a no-op on Firefox) and is called exactly
   * once.
   */
  async onBrowserDownload(candidate: CandidateDownload, release: () => void): Promise<CaptureResult> {
    let released = false;
    const releaseOnce = () => {
      if (released) return;
      released = true;
      release();
    };
    let result: CaptureResult;
    try {
      result = await this.decideAndHandOff(candidate, releaseOnce);
    } catch (error) {
      // Defensive: nothing above should throw, but the browser must never be
      // left waiting.
      releaseOnce();
      result = { outcome: "kept", reason: "error", error };
    }
    this.deps.report?.(candidate, result);
    return result;
  }

  private async decideAndHandOff(candidate: CandidateDownload, release: () => void): Promise<CaptureResult> {
    const { deps } = this;
    const settings = await deps.settings();
    const now = deps.now();
    const bypass = settings.captureEnabled && deps.bypass.consume(now);
    const force = !bypass && deps.force?.consume(now) === true;
    const ctx = {
      bypass,
      force,
      blocked: deps.blocked?.([candidate.url, candidate.originalUrl, candidate.referrer]) ?? false,
      nonGetRequest: deps.methods.wasNonGet([candidate.url, candidate.originalUrl], now),
    };

    // A fresh copy of an expired whoosh download continues that download.
    if (!bypass && deps.expired && safetySkip(candidate, settings, ctx) === undefined) {
      const expiredId = await deps.expired.match(candidate, settings);
      if (expiredId) return this.refreshExpired(candidate, expiredId, settings, release);
    }

    const decision = evaluateCapture(candidate, settings, ctx);
    if (!decision.capture) {
      release();
      return { outcome: "skipped", reason: decision.reason };
    }

    const storeId = this.storeOf(candidate);
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

  private storeOf(candidate: CandidateDownload): string | undefined {
    return candidate.cookieStoreId ?? (candidate.incognito ? privateCookieStore(this.deps.isFirefox) : undefined);
  }

  /**
   * Same never-lose rule as a hand-off: the browser copy is cancelled only
   * after the daemon accepted the new address. A late or failed answer leaves
   * the browser its download (at worst the file exists twice).
   */
  private async refreshExpired(
    candidate: CandidateDownload,
    daemonId: string,
    settings: Settings,
    release: () => void,
  ): Promise<CaptureResult> {
    const { deps } = this;
    const file: BrowserFile & { cookieStoreId?: string } = { url: candidate.url };
    if (candidate.originalUrl) file.originalUrl = candidate.originalUrl;
    if (candidate.referrer) file.referrer = candidate.referrer;
    if (candidate.filename) file.filename = candidate.filename;
    if (candidate.totalBytes) file.totalBytes = candidate.totalBytes;
    const storeId = this.storeOf(candidate);
    if (storeId) file.cookieStoreId = storeId;

    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = new Promise<typeof TIMEOUT>((resolve) => {
      timer = setTimeout(() => resolve(TIMEOUT), settings.handoffTimeoutMs);
    });
    try {
      const answer = await Promise.race([deps.expired!.refreshFromBrowser(daemonId, file), deadline]);
      if (answer === TIMEOUT) {
        release();
        return { outcome: "kept", reason: "timeout", healing: daemonId };
      }
    } catch (error) {
      release();
      return { outcome: "kept", reason: "error", error, healing: daemonId };
    } finally {
      clearTimeout(timer);
    }

    try {
      await deps.downloads.cancel(candidate.id);
    } catch {
      // It finished or went away; the browser may keep a copy.
    }
    release();
    try {
      const state = await deps.downloads.state(candidate.id);
      if (state !== "complete" && state !== "in_progress") await deps.downloads.erase(candidate.id);
    } catch {
      // Cosmetic.
    }
    return { outcome: "healed", daemonId };
  }
}
