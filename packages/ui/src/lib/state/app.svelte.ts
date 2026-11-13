// SPDX-License-Identifier: Apache-2.0
// Everything the shell needs, built from one WhooshClient. Components get it
// from context (`getApp()`), so the same tree runs on the mock, the real
// WebSocket client or (in the extension) a native-messaging client.

import type { AddParams, DownloadSummary, Settings, Stats, WhooshClient } from "@whoosh/api-client";
import { isWhooshConnectionError, isWhooshRpcError } from "@whoosh/api-client/errors";
import { getContext, setContext, untrack } from "svelte";
import { I18n } from "../i18n/i18n.svelte.ts";
import { DownloadStore, PAUSABLE, RESUMABLE, type Download } from "./downloads.svelte.ts";
import { Prefs } from "./prefs.svelte.ts";
import { SegmentCache } from "./segments.svelte.ts";
import { ToastStore } from "./toasts.svelte.ts";

export type Connection = "demo" | "connecting" | "open" | "reconnecting" | "closed";
export type MainView = "downloads" | "settings";
export type Theme = "system" | "light" | "dark" | "high-contrast";

export interface AppOptions {
  /** "mock" labels the status bar "Demo data". */
  mode?: "mock" | "live";
  /** Where UI preferences persist; null disables. Defaults to localStorage. */
  storage?: Storage | null;
  /** Language before settings load (defaults to the browser language). */
  locale?: string;
}

interface StatusSource {
  onStatus(h: (s: { state: string }) => void): () => void;
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

  settings = $state.raw<Settings | undefined>(undefined);
  stats = $state.raw<Stats>({ downloadBps: 0, uploadBps: 0, active: 0, queued: 0 });
  connection = $state<Connection>("connecting");

  view = $state<MainView>("downloads");
  navOpen = $state(false);
  addOpen = $state(false);
  shortcutsOpen = $state(false);
  /** Ids awaiting the remove confirmation. */
  removeIds = $state.raw<string[] | undefined>(undefined);
  /** Id awaiting a fresh URL. */
  refreshId = $state<string | undefined>(undefined);
  menu = $state.raw<{ id: string; x: number; y: number } | undefined>(undefined);
  /** Bumped to ask the download list to take keyboard focus. */
  listFocus = $state(0);


  readonly #off: (() => void)[] = [];

  constructor(client: WhooshClient, opts: AppOptions = {}) {
    this.client = client;
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
      }),
    );
    this.segments.start();
    void this.reload();
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
        action: { label: t("action.refreshLink"), run: () => (this.refreshId = d.id) },
      });
    }
  }

  // ---------------------------------------------------------------- actions

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
      this.refreshId = needsRefresh.id;
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
      this.toasts.push({ tone: "info", message: t("remove.done", { count: ids.length, name }) });
    } catch (e) {
      this.report(e);
    }
  }

  async refreshUrl(id: string, url: string): Promise<void> {
    await this.client.call("download.refreshUrl", { id, url });
  }

  async add(params: AddParams): Promise<string> {
    const { id } = await this.client.call("download.add", params);
    // The added event usually arrives first; make sure the row exists either way.
    if (!this.store.get(id)) {
      try {
        this.store.upsert(await this.client.call("download.get", { id }));
      } catch {
        // the event will bring it
      }
    }
    return id;
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
