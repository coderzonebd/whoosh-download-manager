// SPDX-License-Identifier: GPL-3.0-or-later

// Self-healing links (design: docs/self-healing.md).
//
// When a download the extension handed over stops in `needs_refresh` (its
// link expired), the healer looks for the fresh address in the user's own
// browsing and gives it to the daemon with `download.refreshUrl`, together
// with the browser's current cookies and referrer. Strategies, cheapest first:
//
//   1. cookies  — the same link again with fresh cookies (stable "/download/123"
//                 links, session-bound links);
//   2. page     — read the links on the page the download came from (an open
//                 tab, else a muted background tab) and pick the same link,
//                 a re-signed copy of it, or the same file name;
//   3. browser  — a new browser download of the same file (the user clicked
//                 again, or used "Open page") is taken over for the refresh;
//   4. ask      — anything unsure, or nothing found: the popup asks the user.
//
// The extension only proposes; the daemon checks that the new address serves
// the same file before it continues. Nothing here leaves the machine.

import type { DownloadId, DownloadState, DownloadSummary, RefreshUrlParams, RequestContext, StateEvent } from "@whoosh/api-client";
import { ErrorCode } from "@whoosh/api-client";

import type { LinkInfo } from "../shared/messages";
import type { KeyValueStore, Settings } from "../shared/settings";
import { RpcError } from "../transport/rpc";
import { buildRequestContext, cookiesFor, type CookieSource } from "./capture/context";
import type { DaemonCalls } from "./capture/handoff";
import { isSameFile, matchLink, type BrowserFile } from "./healing/match";

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
  /** Firefox container of the download, so the page is read with the same cookies. */
  cookieStoreId?: string;
  capturedAt: number;
}

export type HealStrategy = "cookies" | "page";

/** A question for the user, shown in the popup. */
export interface HealAsk {
  id: DownloadId;
  filename: string;
  pageUrl?: string;
  /**
   * `open-page`: no fresh address was found; opening the page once lets the
   * next matching download refresh it. `confirm`: a guess that needs a yes.
   */
  kind: "open-page" | "confirm";
  candidateUrl?: string;
  askedAt: number;
}

export type HealAction = "open-page" | "use-candidate" | "dismiss";

export type RefreshOutcome =
  | { status: "refreshed"; url: string; via: HealStrategy | "browser-download" | "user"; request?: RequestContext }
  | { status: "asked"; ask: HealAsk }
  | { status: "failed"; reason: "daemon-refused" | "user-declined" | "busy" }
  | { status: "unsupported"; reason: "disabled" | "unknown" | "policy" };

export interface SelfHealer {
  /** Called after a successful hand-off. */
  remember(provenance: Provenance): Promise<void>;
  /** Called for `event.download.state` with `state: "needs_refresh"`. */
  heal(event: StateEvent): Promise<RefreshOutcome>;
  /** Called when the daemon reports the download completed or was removed. */
  forget(id: DownloadId): Promise<void>;
}

/** Reads the links of a page the way the user would see it. */
export type PageLinks = (pageUrl: string, cookieStoreId: string | undefined) => Promise<LinkInfo[]>;

export interface HealerDeps {
  /** `storage.local`. */
  store: KeyValueStore;
  daemon: DaemonCalls;
  cookies: CookieSource;
  pageLinks: PageLinks;
  /** Opens the page in a foreground tab (the "Open page" answer). */
  openPage(pageUrl: string, cookieStoreId: string | undefined): Promise<void>;
  settings(): Promise<Settings>;
  /** True when this build must leave these URLs alone (store policy). */
  blocked(urls: (string | undefined)[]): boolean;
  userAgent: string;
  now(): number;
  /** Told whenever the list of questions for the user changes (badge). */
  onAsksChanged?(asks: HealAsk[]): void;
}

interface Round {
  startedAt: number;
  tried: HealStrategy[];
}

interface HealRecord extends Provenance {
  /** The last state the daemon reported. */
  lastState?: DownloadState;
  /** The strategies tried since the link last expired. */
  round?: Round;
  /** Rounds in a row that ended without a working address. */
  failedRounds: number;
  lastRoundEndedAt?: number;
  ask?: HealAsk;
  /** Until then, the next browser download of the same file refreshes this one. */
  armedUntil?: number;
  /** When an address was last proposed (to recognise a wrong-file failure). */
  proposedAt?: number;
  /** The user dismissed the question, or the daemon said the file changed: no more automatic attempts. */
  stopped?: boolean;
}

export const STORAGE_KEY = "healing";
export const MAX_ENTRIES = 500;
export const MAX_AGE_MS = 30 * 24 * 60 * 60_000;
/** Automatic rounds in a row before only the user can help. */
export const MAX_FAILED_ROUNDS = 3;
/** A new automatic round waits this long after a failed one. */
export const ROUND_GAP_MS = 10 * 60_000;
/** How long "Open page" waits for the matching download. */
export const ARM_MS = 10 * 60_000;
/** How long an event for a download not remembered yet is kept for it. */
export const EARLY_EVENT_MS = 60_000;

const STRATEGIES: readonly HealStrategy[] = ["cookies", "page"];
const ACTIVE_STATES: ReadonlySet<DownloadState> = new Set(["queued", "probing", "downloading", "verifying"]);

function displayName(rec: Provenance): string {
  if (rec.filename) return rec.filename;
  try {
    const path = new URL(rec.url).pathname;
    return decodeURIComponent(path.slice(path.lastIndexOf("/") + 1)) || rec.url;
  } catch {
    return rec.url;
  }
}

export class LinkHealer implements SelfHealer {
  private records: Map<DownloadId, HealRecord> | undefined;
  private loading: Promise<Map<DownloadId, HealRecord>> | undefined;
  private saving: Promise<void> = Promise.resolve();
  /** Downloads being healed right now (one heal at a time per download). */
  private readonly busy = new Set<DownloadId>();
  /**
   * Events for downloads not remembered yet: the daemon may report a state
   * before the hand-off has finished (the browser copy is still being
   * cancelled). `remember` applies them.
   */
  private readonly early = new Map<DownloadId, { event: StateEvent | "removed"; at: number }>();

  constructor(private readonly deps: HealerDeps) {}

  // ------------------------------------------------------------ bookkeeping

  async remember(provenance: Provenance): Promise<void> {
    if (this.deps.blocked([provenance.url, provenance.linkUrl, provenance.originPageUrl])) return;
    const records = await this.load();
    const existing = records.get(provenance.daemonId);
    records.set(provenance.daemonId, { ...existing, ...provenance, failedRounds: existing?.failedRounds ?? 0 });
    this.prune(records);
    await this.save();
    const early = this.early.get(provenance.daemonId);
    if (early) {
      this.early.delete(provenance.daemonId);
      if (early.event === "removed") await this.forget(provenance.daemonId);
      else await this.onState(early.event);
    }
  }

  private keepEarly(id: DownloadId, event: StateEvent | "removed"): void {
    const now = this.deps.now();
    for (const [key, value] of this.early) if (now - value.at > EARLY_EVENT_MS) this.early.delete(key);
    this.early.delete(id);
    this.early.set(id, { event, at: now });
    if (this.early.size > 64) {
      const oldest = this.early.keys().next().value;
      if (oldest !== undefined) this.early.delete(oldest);
    }
  }

  async forget(id: DownloadId): Promise<void> {
    const records = await this.load();
    const had = records.get(id);
    if (!records.delete(id)) return;
    await this.save();
    if (had?.ask) this.asksChanged();
  }

  /** Daemon ids the extension remembers. */
  async ids(): Promise<DownloadId[]> {
    return [...(await this.load()).keys()];
  }

  /**
   * True while a remembered download may still be running or being healed:
   * the native port is then kept open so its events arrive. Synchronous; false
   * until the records have been loaded once.
   */
  hasActive(): boolean {
    for (const rec of this.records?.values() ?? []) {
      if (rec.lastState === undefined || ACTIVE_STATES.has(rec.lastState) || rec.round) return true;
    }
    return false;
  }

  /** Open questions for the user, newest first. */
  async asks(): Promise<HealAsk[]> {
    return this.collectAsks(await this.load());
  }

  // ------------------------------------------------------------ daemon events

  /** `event.download.state` for any download. */
  async onState(event: StateEvent): Promise<RefreshOutcome | undefined> {
    const records = await this.load();
    const rec = records.get(event.id);
    if (!rec) {
      this.keepEarly(event.id, event);
      return undefined;
    }
    if (event.state === "completed") {
      await this.forget(event.id);
      return undefined;
    }
    if (event.state === "needs_refresh") return this.heal(event);

    rec.lastState = event.state;
    if (event.state === "downloading" && (rec.round || rec.ask || rec.stopped)) {
      // The new address worked (the daemon checked it before downloading).
      const hadAsk = rec.ask !== undefined;
      this.endRound(rec, true);
      delete rec.ask;
      delete rec.armedUntil;
      delete rec.stopped;
      if (hadAsk) this.asksChanged();
    } else if (event.state === "failed" && event.error?.code === "remote_changed" && rec.proposedAt !== undefined) {
      // The proposed address served another file; the daemon refused it.
      // Never guess again for this download.
      delete rec.round;
      rec.stopped = true;
    }
    await this.save();
    return undefined;
  }

  /** `event.download.removed`. */
  async onRemoved(ids: readonly DownloadId[]): Promise<void> {
    const records = await this.load();
    for (const id of ids) {
      if (records.has(id)) await this.forget(id);
      else this.keepEarly(id, "removed");
    }
  }

  /**
   * Catch-up after the native port (re)opened: `summaries` is every download
   * the daemon has that is not completed. Remembered downloads missing from
   * it finished or were removed while no events could arrive; those in
   * `needs_refresh` are healed now.
   */
  async sync(summaries: readonly DownloadSummary[]): Promise<RefreshOutcome[]> {
    const live = new Map(summaries.map((s) => [s.id, s]));
    const records = await this.load();
    const outcomes: RefreshOutcome[] = [];
    let changed = false;
    for (const id of [...records.keys()]) {
      const summary = live.get(id);
      if (!summary || summary.state === "completed") {
        records.delete(id);
        changed = true;
        continue;
      }
      const event: StateEvent = { id, state: summary.state };
      if (summary.error) event.error = summary.error;
      const outcome = await this.onState(event);
      if (outcome) outcomes.push(outcome);
    }
    if (changed) {
      await this.save();
      this.asksChanged();
    }
    return outcomes;
  }

  // ------------------------------------------------------------ healing

  async heal(event: StateEvent): Promise<RefreshOutcome> {
    const settings = await this.deps.settings();
    const records = await this.load();
    const rec = records.get(event.id);
    if (!rec) return { status: "unsupported", reason: "unknown" };
    rec.lastState = "needs_refresh";
    if (!settings.selfHealing) {
      await this.save();
      return { status: "unsupported", reason: "disabled" };
    }
    if (this.deps.blocked([rec.url, rec.linkUrl, rec.originPageUrl])) {
      return { status: "unsupported", reason: "policy" };
    }
    if (this.busy.has(rec.daemonId)) return { status: "failed", reason: "busy" };
    this.busy.add(rec.daemonId);
    try {
      return await this.runStrategies(rec);
    } finally {
      this.busy.delete(rec.daemonId);
    }
  }

  private async runStrategies(rec: HealRecord): Promise<RefreshOutcome> {
    const now = this.deps.now();
    if (rec.ask) return { status: "asked", ask: rec.ask };
    if (rec.stopped) {
      await this.save();
      return { status: "failed", reason: "user-declined" };
    }
    if (!rec.round) {
      const tooMany = rec.failedRounds >= MAX_FAILED_ROUNDS;
      const tooSoon =
        rec.failedRounds > 0 && rec.lastRoundEndedAt !== undefined && now - rec.lastRoundEndedAt < ROUND_GAP_MS;
      if (tooMany || tooSoon) return this.askUser(rec, "open-page");
      rec.round = { startedAt: now, tried: [] };
    }
    const round = rec.round;

    for (const strategy of STRATEGIES) {
      if (round.tried.includes(strategy)) continue;
      round.tried.push(strategy);
      await this.save();

      if (strategy === "cookies") {
        const outcome = await this.propose(rec, rec.linkUrl ?? rec.url, rec.originPageUrl, "cookies");
        if (outcome.status === "refreshed") return outcome;
        continue;
      }

      // strategy === "page"
      if (!rec.originPageUrl) continue;
      let links: LinkInfo[];
      try {
        links = await this.deps.pageLinks(rec.originPageUrl, rec.cookieStoreId);
      } catch {
        continue; // The page is gone or would not load.
      }
      const match = matchLink(links, rec);
      if (!match) continue;
      if (match.confidence === "unsure") return this.askUser(rec, "confirm", match.url);
      const outcome = await this.propose(rec, match.url, rec.originPageUrl, "page");
      if (outcome.status === "refreshed") return outcome;
    }

    // Every strategy of this round was tried and the link is still expired.
    this.endRound(rec, false);
    return this.askUser(rec, "open-page");
  }

  /** Hands a candidate address to the daemon, with the browser's current cookies. */
  private async propose(
    rec: HealRecord,
    url: string,
    referrer: string | undefined,
    via: HealStrategy | "browser-download" | "user",
    cookieStoreId = rec.cookieStoreId,
  ): Promise<RefreshOutcome> {
    const request = buildRequestContext({
      cookies: await cookiesFor(this.deps.cookies, url, cookieStoreId),
      referrer,
      userAgent: this.deps.userAgent,
    });
    const params: RefreshUrlParams = { id: rec.daemonId, url };
    if (request) params.request = request;
    try {
      await this.deps.daemon.call("download.refreshUrl", params);
    } catch (error) {
      if (error instanceof RpcError && error.code === ErrorCode.NotFound) await this.forget(rec.daemonId);
      return { status: "failed", reason: "daemon-refused" };
    }
    rec.proposedAt = this.deps.now();
    rec.lastState = "queued";
    await this.save();
    const outcome: RefreshOutcome = { status: "refreshed", url, via };
    if (request) outcome.request = request;
    return outcome;
  }

  private async askUser(rec: HealRecord, kind: HealAsk["kind"], candidateUrl?: string): Promise<RefreshOutcome> {
    const ask: HealAsk = { id: rec.daemonId, filename: displayName(rec), kind, askedAt: this.deps.now() };
    if (rec.originPageUrl) ask.pageUrl = rec.originPageUrl;
    if (candidateUrl) ask.candidateUrl = candidateUrl;
    rec.ask = ask;
    await this.save();
    this.asksChanged();
    return { status: "asked", ask };
  }

  private endRound(rec: HealRecord, succeeded: boolean): void {
    if (!rec.round && succeeded) return;
    delete rec.round;
    rec.lastRoundEndedAt = this.deps.now();
    rec.failedRounds = succeeded ? 0 : rec.failedRounds + 1;
  }

  // ------------------------------------------------------------ the user

  /** The popup's answer to a question. */
  async act(id: DownloadId, action: HealAction): Promise<RefreshOutcome | undefined> {
    const records = await this.load();
    const rec = records.get(id);
    if (!rec) return undefined;
    const ask = rec.ask;
    delete rec.ask;
    let outcome: RefreshOutcome | undefined;
    switch (action) {
      case "dismiss":
        rec.stopped = true;
        delete rec.round;
        outcome = { status: "failed", reason: "user-declined" };
        break;
      case "use-candidate":
        if (ask?.candidateUrl) outcome = await this.propose(rec, ask.candidateUrl, rec.originPageUrl, "user");
        break;
      case "open-page":
        if (rec.originPageUrl) {
          rec.armedUntil = this.deps.now() + ARM_MS;
          await this.deps.openPage(rec.originPageUrl, rec.cookieStoreId);
        }
        break;
    }
    await this.save();
    this.asksChanged();
    return outcome;
  }

  /**
   * A new browser download: is it the same file as an expired one? Returns
   * the daemon id it can refresh. Only downloads the user opened the page for
   * (armed), or — while capture is on — ones from the same site, qualify.
   */
  async match(file: BrowserFile, settings: Settings): Promise<DownloadId | undefined> {
    if (!settings.selfHealing) return undefined;
    const now = this.deps.now();
    for (const rec of (await this.load()).values()) {
      if (rec.lastState !== "needs_refresh") continue;
      const armed = rec.armedUntil !== undefined && rec.armedUntil > now;
      if (!armed && (!settings.captureEnabled || rec.stopped)) continue;
      if (isSameFile(file, rec, armed)) return rec.daemonId;
    }
    return undefined;
  }

  /** Refreshes `id` from a browser download of the same file. Throws when the daemon refuses. */
  async refreshFromBrowser(id: DownloadId, file: BrowserFile & { cookieStoreId?: string }): Promise<void> {
    const rec = (await this.load()).get(id);
    if (!rec) throw new Error(`unknown download ${id}`);
    const referrer = file.referrer ?? rec.originPageUrl;
    const store = file.cookieStoreId ?? rec.cookieStoreId;
    const outcome = await this.propose(rec, file.url, referrer, "browser-download", store);
    if (outcome.status !== "refreshed") throw new Error("the daemon refused the new address");
    const hadAsk = rec.ask !== undefined;
    delete rec.ask;
    delete rec.armedUntil;
    await this.save();
    if (hadAsk) this.asksChanged();
  }

  // ------------------------------------------------------------ storage

  private load(): Promise<Map<DownloadId, HealRecord>> {
    if (this.records) return Promise.resolve(this.records);
    this.loading ??= this.deps.store
      .get(STORAGE_KEY)
      .then((items) => {
        const map = new Map<DownloadId, HealRecord>();
        const raw = items[STORAGE_KEY];
        if (typeof raw === "object" && raw !== null) {
          for (const [id, value] of Object.entries(raw as Record<string, unknown>)) {
            const rec = value as Partial<HealRecord> | null;
            if (rec && typeof rec.url === "string" && typeof rec.capturedAt === "number") {
              map.set(id, { ...(rec as HealRecord), daemonId: id, failedRounds: Number(rec.failedRounds) || 0 });
            }
          }
        }
        this.prune(map);
        this.records ??= map;
        return this.records;
      })
      .catch(() => {
        this.records ??= new Map();
        return this.records;
      });
    return this.loading;
  }

  /** At most {@link MAX_ENTRIES}, none older than {@link MAX_AGE_MS}. */
  private prune(records: Map<DownloadId, HealRecord>): void {
    const cutoff = this.deps.now() - MAX_AGE_MS;
    for (const [id, rec] of records) if (rec.capturedAt < cutoff) records.delete(id);
    if (records.size <= MAX_ENTRIES) return;
    const oldest = [...records.values()].sort((a, b) => a.capturedAt - b.capturedAt);
    for (const rec of oldest.slice(0, records.size - MAX_ENTRIES)) records.delete(rec.daemonId);
  }

  private save(): Promise<void> {
    const records = this.records;
    if (!records) return Promise.resolve();
    // Writes are chained so an older snapshot never lands after a newer one.
    this.saving = this.saving
      .catch(() => {})
      .then(() => this.deps.store.set({ [STORAGE_KEY]: Object.fromEntries(records) }));
    return this.saving.catch(() => {});
  }

  private collectAsks(records: Map<DownloadId, HealRecord>): HealAsk[] {
    return [...records.values()]
      .map((r) => r.ask)
      .filter((a): a is HealAsk => a !== undefined)
      .sort((a, b) => b.askedAt - a.askedAt);
  }

  private asksChanged(): void {
    if (!this.deps.onAsksChanged || !this.records) return;
    this.deps.onAsksChanged(this.collectAsks(this.records));
  }
}
