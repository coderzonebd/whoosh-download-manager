// SPDX-License-Identifier: Apache-2.0
// Everything the shell needs, built from one WdmClient. Components get it
// from context (`getApp()`), so the same tree runs on the mock, the real
// WebSocket client or (in the extension) a native-messaging client.

import { MAIN_QUEUE, type AddParams, type ConfirmParams, type DownloadSummary, type MediaAddParams, type MediaOffer, type Queue, type QueueConfig, type Settings, type Stats, type WdmClient } from "@wdm/api-client";
import { isWdmConnectionError, isWdmRpcError } from "@wdm/api-client/errors";
import { getContext, setContext, untrack } from "svelte";
import { I18n } from "../i18n/i18n.svelte.ts";
import type { Host } from "../host.ts";
import { DownloadStore, PAUSABLE, RESUMABLE, type Download } from "./downloads.svelte.ts";
import { readFollowed, writeFollowed, type Followed } from "./followed.ts";
import { Prefs } from "./prefs.svelte.ts";
import { SegmentCache } from "./segments.svelte.ts";
import { QUEUES_CAPABILITY, QueueStore, isUnsupported } from "./queues.svelte.ts";
import { MEDIA_CAPABILITY, MediaStore, isMediaUnsupported } from "./media.svelte.ts";
import { ToastStore } from "./toasts.svelte.ts";

export type Connection = "demo" | "connecting" | "open" | "reconnecting" | "closed";
export type MainView = "downloads" | "history" | "settings" | "browsers";
export type Theme = "system" | "light" | "dark" | "high-contrast";

/** How a followed download ended: the prompt window says so ("Download complete"). */
export const ENDED: ReadonlySet<DownloadSummary["state"]> = new Set(["completed", "failed"]);

export interface AppOptions {
  /** "mock" labels the status bar "Demo data". */
  mode?: "mock" | "live";
  /** Where UI preferences persist; null disables. Defaults to localStorage. */
  storage?: Storage | null;
  /** Language before settings load (defaults to the browser language). */
  locale?: string;
  /** What the host shell adds (folder picker, desktop settings…). */
  host?: Host;
  /**
   * The prompt window (`PromptApp`): it takes the pages the extension offers and asks about
   * them and the browser downloads that wait, and nothing else (no first-run pages, notices).
   */
  prompt?: boolean;
}

/** The queue dialog: create a queue, or edit one (name, limit, schedule). */
export type QueueDialog = { mode: "create" } | { mode: "edit"; name: string };

interface StatusSource {
  onStatus(h: (s: { state: string; hello?: { capabilities?: string[] } }) => void): () => void;
}
function hasStatus(c: unknown): c is StatusSource {
  return typeof (c as Partial<StatusSource>)?.onStatus === "function";
}

function defaultStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

export class AppState {
  readonly client: WdmClient;
  readonly i18n: I18n;
  readonly store = new DownloadStore();
  readonly toasts = new ToastStore();
  readonly prefs: Prefs;
  readonly segments: SegmentCache;
  readonly queues = new QueueStore();
  readonly media = new MediaStore();
  readonly host: Host;

  settings = $state.raw<Settings | undefined>(undefined);
  stats = $state.raw<Stats>({ downloadBps: 0, uploadBps: 0, active: 0, queued: 0 });
  connection = $state<Connection>("connecting");

  view = $state<MainView>("downloads");
  navOpen = $state(false);
  addOpen = $state(false);
  /** The address the add dialog opens with (a paste or a whoosh:// link). */
  addUrl = $state("");
  shortcutsOpen = $state(false);
  /** Ids awaiting the remove confirmation. */
  removeIds = $state.raw<string[] | undefined>(undefined);
  /** Id awaiting a fresh URL. */
  refreshId = $state<string | undefined>(undefined);
  menu = $state.raw<{ id: string; x: number; y: number } | undefined>(undefined);
  /** Bumped to ask the download list to take keyboard focus. */
  listFocus = $state(0);
  queueDialog = $state.raw<QueueDialog | undefined>(undefined);
  /** Queue awaiting the delete confirmation. */
  removeQueueName = $state<string | undefined>(undefined);
  /** Ids awaiting "Move to queue". */
  moveIds = $state.raw<string[] | undefined>(undefined);
  /** History entries awaiting the clear confirmation. */
  clearIds = $state.raw<string[] | undefined>(undefined);
  /** Show the one-time crash-report notice (first run, builds that send them). */
  crashNotice = $state(false);
  /**
   * Downloads caught in a browser that wait for the user ("Ask before downloading from the
   * browser"), oldest first. The confirmation dialog asks about the first one.
   */
  readonly waiting: Download[] = $derived.by(() =>
    this.store
      .all()
      .filter((d) => d.data.awaitingConfirmation === true && d.data.state === "paused")
      .sort((a, b) => a.data.addedAt - b.data.addedAt || a.id.localeCompare(b.id)),
  );
  /**
   * Pages the browser extension offered ("Download with WDM" on a video) that this UI took
   * (`media.takeOffer`) and shows in the add dialog, one at a time, oldest first. Only the prompt
   * window and a UI without one (no `Host.openPrompt`) take them.
   */
  offers = $state.raw<MediaOffer[]>([]);
  /**
   * Downloads started in the prompt window, which it follows: their progress while they run
   * (unless hidden), then how they ended. Shared with the main window through storage.
   */
  followed = $state.raw<Followed[]>([]);
  /**
   * The followed download the prompt window shows: one that ended first (so "Download complete"
   * is never kept waiting behind a running one), else the first running one not hidden.
   */
  readonly following: Download | undefined = $derived.by(() => {
    if (!this.prompt) return undefined;
    const known = this.followed.map((f) => ({ f, d: this.store.get(f.id) })).filter((x) => x.d !== undefined);
    const ended = known.find((x) => ENDED.has(x.d!.data.state));
    return (ended ?? known.find((x) => !x.f.hidden))?.d;
  });
  /** The download list (and the offered pages) loaded at least once; the prompt window closes only then. */
  loaded = $state(false);
  readonly prompt: boolean;
  /**
   * The main window of a host with a prompt window: what the browser hands over is asked about
   * there (`Host.openPrompt`), never here.
   */
  readonly delegates: boolean;
  /** Capabilities from the last hello, when the client reports them. */
  #capabilities: string[] | undefined;

  readonly #off: (() => void)[] = [];
  readonly #storage: Storage | null;

  constructor(client: WdmClient, opts: AppOptions = {}) {
    this.client = client;
    this.host = opts.host ?? {};
    this.prompt = opts.prompt === true;
    this.delegates = !this.prompt && typeof this.host.openPrompt === "function";
    if (!this.prompt) {
      this.host.crashReports
        ?.get()
        .then((s) => (this.crashNotice = s !== undefined && !s.noticeShown))
        .catch(() => undefined);
    }
    this.#storage = opts.storage === undefined ? defaultStorage() : opts.storage;
    this.prefs = new Prefs(this.#storage);
    if (this.prompt) {
      this.followed = readFollowed(this.#storage);
      this.#tellShell();
    }
    this.i18n = new I18n(opts.locale ?? (typeof navigator === "undefined" ? "en" : navigator.language));
    this.store.sort = this.prefs.initialSort;
    this.store.setCollationLocale(this.i18n.intlLocale);
    this.segments = new SegmentCache(client, this.store);

    if (opts.mode === "mock") this.connection = "demo";
    else if (hasStatus(client)) {
      this.#off.push(
        client.onStatus((s) => {
          const st = s.state as Connection;
          if (s.hello) this.#capabilities = s.hello.capabilities ?? [];
          // The prompt window also loads again when its first load failed (the daemon was starting).
          const resync = st === "open" && (this.connection === "reconnecting" || (this.prompt && !this.loaded));
          this.connection = st;
          if (resync) void this.reload(); // events were missed while away
        }),
      );
    } else this.connection = "open";

    this.#subscribe();
    this.#off.push(
      $effect.root(() => {
        $effect(() => {
          this.prefs.save(this.store.sort);
        });
        $effect(() => {
          const locale = this.i18n.intlLocale;
          untrack(() => this.store.setCollationLocale(locale));
        });
        // A browser download waits for the user: open the prompt window (the shell's, which asks
        // about it), or, without one, bring this window to the front (also from the tray).
        let shown: string | undefined;
        $effect(() => {
          const id = this.waiting[0]?.id;
          if (id === undefined || id === shown) return;
          shown = id;
          if (this.prompt) return;
          untrack(() => this.#bringUp());
        });
        // A queue view whose queue went away (removed elsewhere) falls back to the main queue.
        $effect(() => {
          const v = this.store.view;
          if (v.kind !== "queue" || v.queue === MAIN_QUEUE) return;
          if (this.queues.supported === false || (this.queues.supported && !this.queues.has(v.queue))) {
            untrack(() => (this.store.view = { kind: "queue", queue: MAIN_QUEUE }));
          }
        });
      }),
    );
    this.segments.start();
    void this.reload();
    if (this.prompt) return;
    // First run of the desktop app: "Connect your browser".
    void this.host.browsers
      ?.firstRun()
      .then((first) => {
        if (first && this.view === "downloads") this.view = "browsers";
      })
      .catch(() => {});
  }

  get theme(): Theme {
    return this.prefs.highContrast ? "high-contrast" : (this.settings?.theme ?? "system");
  }

  destroy(): void {
    for (const off of this.#off.splice(0)) off();
    this.segments.stop();
    this.toasts.clear();
  }

  /** (Re)load the list, settings and stats — at start and after a reconnect. */
  async reload(): Promise<void> {
    const [list, settings, stats] = await Promise.allSettled([
      this.client.call("download.list", {}),
      this.client.call("settings.get", undefined),
      this.client.call("stats.get", undefined),
    ]);
    if (list.status === "fulfilled") {
      this.store.replaceAll(list.value);
      // Followed downloads removed while this window was away are forgotten.
      if (this.prompt) this.#setFollowed(this.followed.filter((f) => list.value.some((d) => d.id === f.id)));
    }
    else this.store.loadError = this.describe(list.reason);
    if (settings.status === "fulfilled") {
      this.settings = settings.value;
      this.i18n.setLocale(settings.value.language);
    }
    if (stats.status === "fulfilled") this.stats = stats.value;
    await Promise.all([this.loadQueues(), this.loadMedia(), this.loadOffers()]);
    // Only a list that loaded counts: the prompt window must not close on a daemon still starting.
    if (list.status === "fulfilled") this.loaded = true;
  }

  /** The prompt window, or this window when there is none. */
  #bringUp(): void {
    const h = this.host;
    if (this.delegates) h.openPrompt?.().catch(() => undefined);
    else h.showWindow?.().catch(() => undefined);
  }

  /**
   * Pages the extension offered while this UI was away. The main window of a host with a
   * prompt window only opens it; otherwise they are taken and shown here.
   */
  async loadOffers(): Promise<void> {
    let list: MediaOffer[];
    try {
      list = await this.client.call("media.offers", undefined);
    } catch {
      return; // an older daemon, or not allowed: nothing to show
    }
    if (list.length === 0) return;
    if (this.delegates) this.#bringUp();
    else for (const o of list) await this.#takeOffer(o.id);
  }

  async #takeOffer(id: string): Promise<void> {
    if (this.offers.some((o) => o.id === id)) return;
    try {
      const offer = await this.client.call("media.takeOffer", { id });
      if (this.offers.some((o) => o.id === offer.id)) return;
      this.offers = [...this.offers, offer];
      if (!this.prompt) this.#bringUp();
    } catch {
      // Taken by another window, or expired.
    }
  }

  #setFollowed(list: Followed[]): void {
    this.followed = list;
    writeFollowed(this.#storage, list);
    this.#tellShell();
  }

  #tellShell(): void {
    this.host.setFollowed?.(this.followed.map((f) => f.id)).catch(() => undefined);
  }

  /** The prompt window started a download: follow it (its progress, then how it ended). */
  follow(id: string): void {
    if (!this.prompt) return;
    this.#setFollowed([...this.followed.filter((f) => f.id !== id), { id, hidden: false }]);
  }

  /** "Hide": its progress goes away; the prompt window comes back when it ends. */
  hideFollowed(id: string): void {
    this.#setFollowed(this.followed.map((f) => (f.id === id ? { ...f, hidden: true } : f)));
  }

  /** Done with a followed download ("Close" on how it ended). */
  unfollow(id: string): void {
    this.#setFollowed(this.followed.filter((f) => f.id !== id));
  }

  /** Open a completed download's file, or show it in its folder (desktop app). */
  openFile(id: string, reveal: boolean): void {
    this.host.openDownload?.(id, reveal).catch((e: unknown) => this.report(e));
  }

  /** The add dialog for the first offered page closed (added, or not wanted). */
  doneWithOffer(id: string): void {
    this.offers = this.offers.filter((o) => o.id !== id);
  }

  /** Learn whether the daemon offers video and audio sites (yt-dlp). Never reports -32006. */
  async loadMedia(): Promise<void> {
    if (this.#capabilities && !this.#capabilities.includes(MEDIA_CAPABILITY)) {
      this.media.disable();
      return;
    }
    try {
      this.media.replace(await this.client.call("media.tools", undefined));
    } catch (e) {
      if (isMediaUnsupported(e)) this.media.disable();
    }
  }

  /** Learn whether the daemon has queues, and load them if it does. Never reports -32006. */
  async loadQueues(): Promise<void> {
    if (this.#capabilities && !this.#capabilities.includes(QUEUES_CAPABILITY)) {
      this.queues.disable();
      return;
    }
    try {
      const list = await this.client.call("queue.list", undefined);
      this.queues.supported = true;
      this.queues.replace(list);
    } catch (e) {
      if (isUnsupported(e)) this.queues.disable();
      // Anything else (a dropped connection): keep what we know; the next reload asks again.
    }
  }

  #subscribe(): void {
    const c = this.client;
    this.#off.push(
      c.on("event.download.added", (d) => this.store.upsert(d)),
      c.on("event.download.updated", (d) => this.store.upsert(d)),
      c.on("event.download.progress", (e) => this.store.applyProgress(e.items)),
      c.on("event.download.removed", (e) => {
        this.store.remove(e.ids);
        if (this.prompt && this.followed.some((f) => e.ids.includes(f.id))) {
          this.#setFollowed(this.followed.filter((f) => !e.ids.includes(f.id)));
        }
        this.segments.forget(e.ids);
      }),
      c.on("event.stats", (s) => (this.stats = s)),
      c.on("event.settings", (s) => {
        this.settings = s;
        this.i18n.setLocale(s.language);
      }),
      c.on("event.queues", (list) => {
        this.queues.supported = true;
        this.queues.replace(list);
      }),
      c.on("event.queue.finished", (e) => this.queues.finished(e)),
      c.on("event.media.tools", (s) => this.media.replace(s)),
      c.on("event.media.offered", (o) => {
        if (this.delegates) this.#bringUp();
        else void this.#takeOffer(o.id);
      }),
      c.on("event.download.state", (e) => {
        const prev = this.store.applyState(e);
        const d = this.store.get(e.id);
        if (!d || prev === undefined || prev === e.state) return;
        if (e.state === "completed") this.segments.forget([e.id]);
        // A download the prompt window follows ended: bring it back to say so.
        if (this.delegates && ENDED.has(e.state) && readFollowed(this.#storage).some((f) => f.id === e.id)) this.#bringUp();
        this.#announce(d, e.state);
      }),
    );
  }

  #announce(d: Download, state: DownloadSummary["state"]): void {
    const { t } = this.i18n;
    const name = d.data.filename;
    const show = { label: t("action.show"), run: () => this.reveal(d.id) };
    if (state === "completed") this.toasts.push({ tone: "success", message: t("toast.completed", { name }), action: show });
    else if (state === "failed") this.toasts.push({ tone: "error", message: t("toast.failed", { name }), action: show });
    else if (state === "needs_refresh") {
      this.toasts.push({
        tone: "warning",
        message: t("toast.expired", { name }),
        action: { label: t("action.refreshLink"), run: () => this.refreshLink(d.id) },
      });
    }
  }

  // ---------------------------------------------------------------- actions

  /**
   * A link expired. A media download asks the daemon to get fresh links from
   * its page (yt-dlp) by itself; anything else needs a new address from the
   * user.
   */
  refreshLink(id: string): void {
    const d = this.store.get(id);
    if (d?.data.media) {
      this.client.call("download.refreshUrl", { id, url: d.data.url }).catch((e: unknown) => this.report(e));
      return;
    }
    this.refreshId = id;
  }

  /** Open the add dialog, optionally with an address filled in. */
  /** Hide the crash-report notice for good. */
  dismissCrashNotice(): void {
    if (!this.crashNotice) return;
    this.crashNotice = false;
    this.host.crashReports?.dismissNotice().catch((e: unknown) => this.report(e));
  }

  /** The notice's link: Settings, with focus on the crash-report switch. */
  openCrashSetting(): void {
    this.dismissCrashNotice();
    this.view = "settings";
    const focus = (tries: number): void => {
      const el = document.getElementById("vy-set-crash");
      if (el) el.focus();
      else if (tries > 0) setTimeout(() => focus(tries - 1), 50);
    };
    setTimeout(() => focus(40), 0);
  }

  openAdd(url = ""): void {
    this.addUrl = url;
    this.view = this.view === "settings" || this.view === "browsers" ? "downloads" : this.view;
    this.addOpen = true;
  }

  /** Show a download: switch to the list, clear filters that hide it, select it. */
  reveal(id: string): void {
    this.view = "downloads";
    if (this.store.indexOf(id) < 0) {
      this.store.view = { kind: "group", group: "all" };
      this.store.query = "";
    }
    this.store.selectOnly(id);
    this.prefs.detailOpen = true;
    this.listFocus++;
  }

  describe(err: unknown): string {
    if (isWdmRpcError(err) || isWdmConnectionError(err)) return err.message;
    return err instanceof Error ? err.message : String(err);
  }

  report(err: unknown): void {
    this.toasts.push({ tone: "error", message: this.i18n.t("error.generic", { message: this.describe(err) }) });
  }

  pausableIds(items = this.store.selected()): string[] {
    return items.filter((d) => PAUSABLE.has(d.data.state)).map((d) => d.id);
  }

  resumableIds(items = this.store.selected()): string[] {
    return items.filter((d) => RESUMABLE.has(d.data.state)).map((d) => d.id);
  }

  async pause(ids = this.pausableIds()): Promise<void> {
    if (ids.length === 0) return;
    try {
      await this.client.call("download.pause", { ids });
    } catch (e) {
      this.report(e);
    }
  }

  async resume(ids = this.resumableIds()): Promise<void> {
    if (ids.length === 0) return;
    try {
      await this.client.call("download.resume", { ids });
    } catch (e) {
      this.report(e);
    }
  }

  /** Space: pause what is moving in the selection, otherwise resume what is stopped. */
  async togglePause(): Promise<void> {
    const sel = this.store.selected();
    const pausable = this.pausableIds(sel);
    if (pausable.length > 0) return this.pause(pausable);
    const needsRefresh = sel.find((d) => d.data.state === "needs_refresh");
    if (needsRefresh && sel.length === 1) {
      this.refreshLink(needsRefresh.id);
      return;
    }
    return this.resume(this.resumableIds(sel));
  }

  requestRemove(ids = this.store.selected().map((d) => d.id)): void {
    if (ids.length > 0) this.removeIds = ids;
  }

  async remove(ids: string[], deleteFiles: boolean): Promise<void> {
    const { t } = this.i18n;
    const name = this.store.get(ids[0])?.data.filename ?? "";
    try {
      await this.client.call("download.remove", { ids, deleteFiles });
      this.toasts.push({ tone: "info", message: ids.length === 1 ? t("remove.doneOne", { name }) : t("remove.done", { count: ids.length }) });
    } catch (e) {
      this.report(e);
    }
  }

  /**
   * Answer a download that waits for confirmation: start it now, or keep it for later
   * (`download.confirm`). Throws, so the dialog can show what went wrong.
   */
  async confirmDownload(params: ConfirmParams): Promise<void> {
    await this.client.call("download.confirm", params);
    // The daemon's `updated` event follows; leave the dialog right away.
    const d = this.store.get(params.id);
    if (d?.data.awaitingConfirmation) {
      const next: DownloadSummary = { ...d.data };
      delete next.awaitingConfirmation;
      this.store.upsert(next);
    }
  }

  /**
   * "Cancel" in the confirmation dialog (nothing was fetched yet) and in the prompt window's
   * progress: the download goes away with its partial file.
   */
  async cancelWaiting(id: string): Promise<void> {
    await this.client.call("download.remove", { ids: [id], deleteFiles: true });
    this.store.remove([id]);
  }

  async refreshUrl(id: string, url: string): Promise<void> {
    await this.client.call("download.refreshUrl", { id, url });
  }

  async add(params: AddParams): Promise<string> {
    const { id } = await this.client.call("download.add", params);
    await this.#ensureRow(id);
    return id;
  }

  /** A video or audio download from a page (`media.add`). */
  async addMedia(params: MediaAddParams): Promise<string> {
    const { id } = await this.client.call("media.add", params);
    await this.#ensureRow(id);
    return id;
  }

  async #ensureRow(id: string): Promise<void> {
    // The added event usually arrives first; make sure the row exists either way.
    if (!this.store.get(id)) {
      try {
        this.store.upsert(await this.client.call("download.get", { id }));
      } catch {
        // the event will bring it
      }
    }
  }

  async copyLink(id: string): Promise<void> {
    const d = this.store.get(id);
    if (!d) return;
    try {
      await navigator.clipboard.writeText(d.data.url);
      this.toasts.push({ tone: "success", message: this.i18n.t("detail.copied") }, 2500);
    } catch (e) {
      this.report(e);
    }
  }

  // ---------------------------------------------------------------- queues

  /** Run a queue method; errors become a toast. Returns whether it worked. */
  async #queueCall(run: () => Promise<unknown>): Promise<boolean> {
    try {
      await run();
      return true;
    } catch (e) {
      if (isUnsupported(e)) this.queues.disable();
      else this.report(e);
      return false;
    }
  }

  async createQueue(config: QueueConfig): Promise<void> {
    const q = await this.client.call("queue.create", config);
    this.#mergeQueue(q);
  }

  /** Rename (when `newName` differs) and then apply `config` under the new name. */
  async editQueue(name: string, config: QueueConfig): Promise<void> {
    let current = name;
    if (config.name !== name) {
      // Read before the call: `event.queues` may arrive first and move a view
      // whose queue disappeared back to the main queue.
      const v = this.store.view;
      const following = v.kind === "queue" && v.queue === name;
      this.#mergeQueue(await this.client.call("queue.rename", { name, newName: config.name }), name);
      current = config.name;
      if (following) this.store.view = { kind: "queue", queue: current };
    }
    this.#mergeQueue(await this.client.call("queue.update", { ...config, name: current }));
  }

  /** Apply a queue the daemon returned, before `event.queues` arrives. */
  #mergeQueue(q: Queue, oldName = q.name): void {
    const rest = this.queues.list.filter((x) => x.name !== oldName && x.name !== q.name);
    const main = rest.filter((x) => x.name === MAIN_QUEUE);
    const named = [...rest.filter((x) => x.name !== MAIN_QUEUE), q].sort((a, b) => a.name.localeCompare(b.name));
    this.queues.replace(q.name === MAIN_QUEUE ? [q, ...rest.filter((x) => x.name !== MAIN_QUEUE)] : [...main, ...named]);
  }

  async removeQueue(name: string): Promise<void> {
    const ok = await this.#queueCall(() => this.client.call("queue.remove", { name }));
    if (!ok) return;
    this.queues.replace(this.queues.list.filter((q) => q.name !== name));
    for (const d of this.store.all()) {
      if (d.data.queue !== name) continue;
      const next: DownloadSummary = { ...d.data };
      delete next.queue;
      this.store.upsert(next);
    }
    this.toasts.push({ tone: "info", message: this.i18n.t("queue.removed", { name }) }, 3000);
  }

  startQueue(name: string): Promise<boolean> {
    return this.#queueCall(() => this.client.call("queue.start", { name }));
  }

  stopQueue(name: string): Promise<boolean> {
    return this.#queueCall(() => this.client.call("queue.stop", { name }));
  }

  async cancelAction(name: string): Promise<void> {
    const ok = await this.#queueCall(() => this.client.call("queue.cancelAction", { name }));
    if (ok) {
      this.queues.dismiss(name);
      this.toasts.push({ tone: "info", message: this.i18n.t("afterDone.cancelled") }, 3000);
    }
  }

  /** Move downloads to `queue` ("main" = the main queue). */
  async moveToQueue(ids: string[], queue: string): Promise<boolean> {
    if (ids.length === 0) return true;
    const params = queue === MAIN_QUEUE ? { ids } : { ids, queue };
    const ok = await this.#queueCall(() => this.client.call("download.setQueue", params));
    if (!ok) return false;
    // The daemon's `updated` events follow; show the move right away.
    for (const id of ids) {
      const d = this.store.get(id);
      if (!d) continue;
      const next: DownloadSummary = { ...d.data };
      if (queue === MAIN_QUEUE) delete next.queue;
      else next.queue = queue;
      this.store.upsert(next);
    }
    const name = queue === MAIN_QUEUE ? this.i18n.t("queue.main") : queue;
    this.toasts.push({ tone: "success", message: ids.length === 1 ? this.i18n.t("move.doneOne", { queue: name }) : this.i18n.t("move.done", { count: ids.length, queue: name }) }, 3000);
    return true;
  }

  /** Queue display name ("main" is translated). */
  queueLabel(name: string): string {
    return name === MAIN_QUEUE ? this.i18n.t("queue.main") : name;
  }

  // ---------------------------------------------------------------- history

  /** Remove finished or failed entries from the list; their files stay on disk. */
  async clearHistory(ids: string[]): Promise<void> {
    if (ids.length === 0) return;
    try {
      await this.client.call("download.remove", { ids, deleteFiles: false });
      this.store.remove(ids);
      this.toasts.push({ tone: "info", message: this.i18n.t("history.cleared", { count: ids.length }) }, 3000);
    } catch (e) {
      this.report(e);
    }
  }

  /** "Not now" on the sort-into-folders question: asked again at the next start. */
  organizeDismissed = $state(false);

  /**
   * Ask once whether to sort downloads into folders by type: the daemon has not been told
   * (`organizeByCategory` absent), and sorting creates folders, so it waits for the user.
   */
  get askOrganize(): boolean {
    return this.connection === "open" && this.settings !== undefined && this.settings.organizeByCategory === undefined && !this.organizeDismissed;
  }

  /** The answer to that question, kept by the daemon (`settings.set`). */
  async setOrganize(on: boolean): Promise<boolean> {
    if (!this.settings) return false;
    return this.saveSettings({ ...this.settings, organizeByCategory: on });
  }

  async saveSettings(next: Settings): Promise<boolean> {
    try {
      const saved = await this.client.call("settings.set", next);
      this.settings = saved;
      this.i18n.setLocale(saved.language);
      this.toasts.push({ tone: "success", message: this.i18n.t("settings.saved") }, 3000);
      return true;
    } catch (e) {
      this.toasts.push({ tone: "error", message: this.i18n.t("settings.saveFailed", { message: this.describe(e) }) });
      return false;
    }
  }
}

const KEY = Symbol("wdm.app");

export function provideApp(app: AppState): AppState {
  return setContext(KEY, app);
}

export function getApp(): AppState {
  const app = getContext<AppState | undefined>(KEY);
  if (!app) throw new Error("AppState missing: render inside <App> or provide it with provideApp()");
  return app;
}

export const APP_CONTEXT_KEY = KEY;
