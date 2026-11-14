<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  The whole download manager UI. Hand it any WhooshClient (mock, WebSocket,
  native messaging) and it runs. Hosts (desktop, extension, mobile, web
  remote) only decide which client to pass and where to mount it.
-->
<script lang="ts">
  import type { WhooshClient } from "@whoosh/api-client";
  import { onDestroy, untrack } from "svelte";
  import { AppState, provideApp, type AppOptions } from "../lib/state/app.svelte.ts";
  import type { Host } from "../lib/host.ts";
  import AddDownloadDialog from "./AddDownloadDialog.svelte";
  import AfterDoneBanner from "./AfterDoneBanner.svelte";
  import ClearHistoryDialog from "./ClearHistoryDialog.svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import DetailPanel from "./DetailPanel.svelte";
  import DownloadList from "./DownloadList.svelte";
  import HistoryView from "./HistoryView.svelte";
  import MoveQueueDialog from "./MoveQueueDialog.svelte";
  import QueueBar from "./QueueBar.svelte";
  import QueueDialog from "./QueueDialog.svelte";
  import RefreshLinkDialog from "./RefreshLinkDialog.svelte";
  import RemoveDialog from "./RemoveDialog.svelte";
  import RemoveQueueDialog from "./RemoveQueueDialog.svelte";
  import SettingsView from "./SettingsView.svelte";
  import ShortcutsDialog from "./ShortcutsDialog.svelte";
  import Sidebar from "./Sidebar.svelte";
  import StatusBar from "./StatusBar.svelte";
  import Toasts from "./Toasts.svelte";
  import Toolbar from "./Toolbar.svelte";

  interface Props {
    client: WhooshClient;
    /** "mock" shows "Demo data" in the status bar. */
    mode?: "mock" | "live";
    /** Where device preferences persist (default localStorage; null = nowhere). */
    storage?: Storage | null;
    /** Language until the daemon's settings arrive. */
    locale?: string;
    /** Set lang/dir/data-theme on <html> too (default true; turn off when embedded). */
    manageDocument?: boolean;
    /** Receive the AppState, e.g. for host integrations and tests. */
    onready?: (app: AppState) => void;
    /** What the host shell adds: folder picker, desktop settings (see lib/host.ts). */
    host?: Host;
  }
  let { client, mode = "live", storage, locale, manageDocument = true, onready, host }: Props = $props();

  const app = untrack(() => {
    const opts: AppOptions = { mode };
    if (storage !== undefined) opts.storage = storage;
    if (locale !== undefined) opts.locale = locale;
    if (host !== undefined) opts.host = host;
    return provideApp(new AppState(client, opts));
  });
  untrack(() => onready?.(app));
  onDestroy(() => app.destroy());

  const { store, prefs, i18n } = app;

  const detail = $derived(app.view === "downloads" && prefs.detailOpen ? store.get(store.activeId) : undefined);
  const modal = $derived(
    app.addOpen ||
      app.shortcutsOpen ||
      app.removeIds !== undefined ||
      app.refreshId !== undefined ||
      app.queueDialog !== undefined ||
      app.removeQueueName !== undefined ||
      app.moveIds !== undefined ||
      app.clearIds !== undefined,
  );
  const queueView = $derived(app.view === "downloads" && store.view.kind === "queue" && app.queues.enabled ? store.view.queue : undefined);

  $effect(() => {
    if (!manageDocument) return;
    const el = document.documentElement;
    el.lang = i18n.locale;
    el.dir = i18n.dir;
    if (app.theme === "system") delete el.dataset.theme;
    else el.dataset.theme = app.theme;
  });

  function inField(e: Event): boolean {
    const el = e.target;
    return el instanceof Element && el.closest("input, textarea, select, [contenteditable='true']") !== null;
  }

  function onkeydown(e: KeyboardEvent): void {
    if (modal || e.defaultPrevented) return;
    const mod = e.ctrlKey || e.metaKey;
    const key = e.key.toLowerCase();
    if (mod && key === "n") {
      e.preventDefault();
      app.openAdd();
    } else if ((mod && key === "f") || (key === "/" && !inField(e))) {
      e.preventDefault();
      app.view = "downloads";
      queueMicrotask(() => document.getElementById("vy-search")?.focus());
    } else if (mod && key === ",") {
      e.preventDefault();
      app.view = "settings";
    } else if ((e.key === "?" || e.key === "F1") && !inField(e)) {
      e.preventDefault();
      app.shortcutsOpen = true;
    } else if (e.key === "Escape" && !inField(e)) {
      if (app.navOpen) app.navOpen = false;
      else if (prefs.detailOpen && detail) prefs.detailOpen = false;
    }
  }

  /** Paste a link anywhere outside a text field → the add dialog opens with it. */
  function onpaste(e: ClipboardEvent): void {
    if (modal || inField(e)) return;
    const text = e.clipboardData?.getData("text/plain")?.trim() ?? "";
    if (!/^(https?|ftp):\/\/\S+$/i.test(text)) return;
    e.preventDefault();
    app.openAdd(text);
  }
</script>

<svelte:window {onkeydown} {onpaste} />

<div class="vy-app" data-theme={app.theme} lang={i18n.locale} dir={i18n.dir}>
  <div class="shell" class:nav-open={app.navOpen} inert={modal}>
    <div class="side"><Sidebar /></div>
    {#if app.navOpen}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div class="nav-scrim" aria-hidden="true" onclick={() => (app.navOpen = false)}></div>
    {/if}
    <main class="main">
      <Toolbar />
      <AfterDoneBanner />
      <div class="content">
        {#if app.view === "settings"}
          <SettingsView />
        {:else if app.view === "history"}
          <HistoryView />
        {:else}
          <div class="list-col">
            {#if queueView !== undefined}<QueueBar name={queueView} />{/if}
            <DownloadList />
          </div>
          {#if detail}
            <DetailPanel download={detail} />
          {/if}
        {/if}
      </div>
    </main>
    <div class="status"><StatusBar /></div>
  </div>

  <Toasts />

  {#if app.menu}
    {#key app.menu}
      <ContextMenu {...app.menu} onclose={() => (app.menu = undefined)} />
    {/key}
  {/if}
  {#if app.addOpen}
    <AddDownloadDialog initialUrl={app.addUrl} onclose={() => (app.addOpen = false)} />
  {/if}
  {#if app.removeIds}
    <RemoveDialog ids={app.removeIds} onclose={() => (app.removeIds = undefined)} />
  {/if}
  {#if app.refreshId}
    <RefreshLinkDialog id={app.refreshId} onclose={() => (app.refreshId = undefined)} />
  {/if}
  {#if app.queueDialog}
    <QueueDialog dialog={app.queueDialog} onclose={() => (app.queueDialog = undefined)} />
  {/if}
  {#if app.removeQueueName !== undefined}
    <RemoveQueueDialog name={app.removeQueueName} onclose={() => (app.removeQueueName = undefined)} />
  {/if}
  {#if app.moveIds}
    <MoveQueueDialog ids={app.moveIds} onclose={() => (app.moveIds = undefined)} />
  {/if}
  {#if app.clearIds}
    <ClearHistoryDialog ids={app.clearIds} onclose={() => (app.clearIds = undefined)} />
  {/if}
  {#if app.shortcutsOpen}
    <ShortcutsDialog onclose={() => (app.shortcutsOpen = false)} />
  {/if}
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr) auto;
    grid-template-areas:
      "side main"
      "status status";
    block-size: 100%;
  }
  .side {
    grid-area: side;
    display: flex;
    min-block-size: 0;
  }
  .main {
    grid-area: main;
    display: flex;
    flex-direction: column;
    min-inline-size: 0;
    min-block-size: 0;
  }
  .content {
    position: relative;
    flex: 1;
    display: flex;
    min-block-size: 0;
  }
  .status {
    grid-area: status;
  }
  .list-col {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-inline-size: 0;
    min-block-size: 0;
  }
  .nav-scrim {
    display: none;
  }

  @container vy-app (max-width: 760px) {
    .shell {
      grid-template-columns: minmax(0, 1fr);
      grid-template-areas:
        "main"
        "status";
    }
    .side {
      position: absolute;
      inset-block: 0;
      inset-inline-start: 0;
      z-index: var(--vy-z-overlay);
      box-shadow: var(--vy-shadow-lg);
      transform: translateX(-105%);
      visibility: hidden;
      transition:
        transform var(--vy-duration-normal) var(--vy-ease-out),
        visibility 0s linear var(--vy-duration-normal);
    }
    :global([dir="rtl"]) .side {
      transform: translateX(105%);
    }
    .nav-open .side {
      transform: none;
      visibility: visible;
      transition: transform var(--vy-duration-normal) var(--vy-ease-out);
    }
    .nav-scrim {
      display: block;
      position: absolute;
      inset: 0;
      z-index: calc(var(--vy-z-overlay) - 1);
      background: var(--vy-color-scrim);
    }
  }
</style>
