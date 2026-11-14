// SPDX-License-Identifier: Apache-2.0
// Everything the shell needs, built from one WhooshClient. Components get it
// from context (`getApp()`), so the same tree runs on the mock, the real
// WebSocket client or (in the extension) a native-messaging client.

import { MAIN_QUEUE, type AddParams, type DownloadSummary, type MediaAddParams, type Queue, type QueueConfig, type Settings, type Stats, type WhooshClient } from "@whoosh/api-client";
import { isWhooshConnectionError, isWhooshRpcError } from "@whoosh/api-client/errors";
import { getContext, setContext, untrack } from "svelte";
import { I18n } from "../i18n/i18n.svelte.ts";
import type { Host } from "../host.ts";
import { DownloadStore, PAUSABLE, RESUMABLE, type Download } from "./downloads.svelte.ts";
import { Prefs } from "./prefs.svelte.ts";
import { SegmentCache } from "./segments.svelte.ts";
import { QUEUES_CAPABILITY, QueueStore, isUnsupported } from "./queues.svelte.ts";
import { MEDIA_CAPABILITY, MediaStore, isMediaUnsupported } from "./media.svelte.ts";
import { ToastStore } from "./toasts.svelte.ts";

export type Connection = "demo" | "connecting" | "open" | "reconnecting" | "closed";
export type MainView = "downloads" | "history" | "settings" | "browsers";
export type Theme = "system" | "light" | "dark" | "high-contrast";

export interface AppOptions {
  /** "mock" labels the status bar "Demo data". */
  mode?: "mock" | "live";
  /** Where UI preferences persist; null disables. Defaults to localStorage. */
  storage?: Storage | null;
  /** Language before settings load (defaults to the browser language). */
  locale?: string;
  /** What the host shell adds (folder picker, desktop settings…). */
  host?: Host;
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
  readonly client: WhooshClient;
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
  /** Capabilities from the last hello, when the client reports them. */
  #capabilities: string[] | undefined;

  readonly #off: (() => void)[] = [];

  constructor(client: WhooshClient, opts: AppOptions = {}) {
    this.client = client;
    this.host = opts.host ?? {};
    this.host.crashReports
      ?.get()
      .then((s) => (this.crashNotice = s !== undefined && !s.noticeShown))
      .catch(() => undefined);
    this.prefs = new Prefs(opts.storage === undefined ? defaultStorage() : opts.storage);
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
          const resync = st === "open" && this.connection === "reconnecting";
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
    if (list.status === "fulfilled") this.store.replaceAll(list.value);
    else this.store.loadError = this.describe(list.reason);
    if (settings.status === "fulfilled") {
      this.settings = settings.value;
      this.i18n.setLocale(settings.value.language);
    }
    if (stats.status === "fulfilled") this.stats = stats.value;
    await Promise.all([this.loadQueues(), this.loadMedia()]);
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
      c.on("event.download.state", (e) => {
        const prev = this.store.applyState(e);
        const d = this.store.get(e.id);
        if (!d || prev === undefined || prev === e.state) return;
        if (e.state === "completed") this.segments.forget([e.id]);
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
    if (isWhooshRpcError(err) || isWhooshConnectionError(err)) return err.message;
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

const KEY = Symbol("whoosh.app");

export function provideApp(app: AppState): AppState {
  return setContext(KEY, app);
}

export function getApp(): AppState {
  const app = getContext<AppState | undefined>(KEY);
  if (!app) throw new Error("AppState missing: render inside <App> or provide it with provideApp()");
  return app;
}

export const APP_CONTEXT_KEY = KEY;
