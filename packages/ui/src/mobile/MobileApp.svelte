<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  The download manager laid out for a phone (the Android app). Same state,
  client and dialogs as the desktop `App`; a different shell:

  - a top bar with the live total speed, search and a menu (History, Settings);
  - filter chips (All, Downloading, Queued…) with their counts;
  - a list of cards, each opening a details sheet;
  - an Add button at the thumb's reach;
  - every dialog a bottom sheet (Dialog does that on narrow touch screens).

  Android's back gesture closes the topmost layer: each open layer (a page,
  search, the menu, a sheet) holds one browser-history entry, and the
  shell's back press pops it. Content keeps clear of the system bars and the
  keyboard through `--wdm-inset-*`, which the Android shell sets.
-->
<script lang="ts">
  import type { WdmClient } from "@wdm/api-client";
  import { onDestroy, untrack } from "svelte";
  import { AppState, provideApp, type AppOptions } from "../lib/state/app.svelte.ts";
  import type { StateGroup } from "../lib/state/downloads.svelte.ts";
  import type { Host } from "../lib/host.ts";
  import AddDownloadDialog from "../components/AddDownloadDialog.svelte";
  import ClearHistoryDialog from "../components/ClearHistoryDialog.svelte";
  import HistoryView from "../components/HistoryView.svelte";
  import Icon from "../components/Icon.svelte";
  import RefreshLinkDialog from "../components/RefreshLinkDialog.svelte";
  import RemoveDialog from "../components/RemoveDialog.svelte";
  import SettingsView from "../components/SettingsView.svelte";
  import Toasts from "../components/Toasts.svelte";
  import DetailSheet from "./DetailSheet.svelte";
  import MobileCard from "./MobileCard.svelte";

  interface Props {
    client: WdmClient;
    mode?: "mock" | "live";
    storage?: Storage | null;
    locale?: string;
    onready?: (app: AppState) => void;
    host?: Host;
  }
  let { client, mode = "live", storage, locale, onready, host }: Props = $props();

  const app = untrack(() => {
    const opts: AppOptions = { mode };
    if (storage !== undefined) opts.storage = storage;
    if (locale !== undefined) opts.locale = locale;
    if (host !== undefined) opts.host = host;
    return provideApp(new AppState(client, opts));
  });
  untrack(() => onready?.(app));
  onDestroy(() => app.destroy());

  const { store, i18n } = app;
  const { t } = i18n;

  let searching = $state(false);
  let menuOpen = $state(false);
  let detailId = $state<string | undefined>(undefined);
  const detail = $derived(detailId === undefined ? undefined : store.get(detailId));
  let scrolled = $state(false);

  const CHIPS: readonly StateGroup[] = ["all", "active", "queued", "paused", "completed", "attention"];
  const group = $derived(store.view.kind === "group" ? store.view.group : "all");
  /** "All" and the current chip always; the others only while they hold something. */
  const chips = $derived(CHIPS.filter((g) => g === "all" || g === group || store.counts.groups[g] > 0));

  const modal = $derived(app.addOpen || app.removeIds !== undefined || app.refreshId !== undefined || app.clearIds !== undefined || detail !== undefined || menuOpen);

  /** The line under the title: live speed while something moves, else the connection when it is not up. */
  const subtitle = $derived.by(() => {
    if (app.connection !== "open" && app.connection !== "demo") return { text: t(`conn.${app.connection}`), tone: "warn" as const };
    const s = app.stats;
    if (s.active > 0) return { text: `${i18n.speed(s.downloadBps)} · ${t("status.active", { count: s.active })}`, tone: "live" as const };
    if (s.queued > 0) return { text: t("status.queued", { count: s.queued }), tone: "muted" as const };
    return undefined;
  });

  const emptyKind = $derived(
    !store.loaded ? (store.loadError ? "error" : "loading") : store.visible.length > 0 ? undefined : store.query.trim() ? "noMatch" : "empty",
  );

  $effect(() => {
    const el = document.documentElement;
    el.lang = i18n.locale;
    el.dir = i18n.dir;
    if (app.theme === "system") delete el.dataset.theme;
    else el.dataset.theme = app.theme;
  });

  // The Android shell colours the system bars' icons to match the app (light icons on a dark app).
  $effect(() => {
    void app.theme;
    const bg = getComputedStyle(document.documentElement).getPropertyValue("--vy-color-bg").trim();
    (window as { WdmAndroid?: { setBars?: (bg: string) => void } }).WdmAndroid?.setBars?.(bg);
  });

  // ---- Android back: one history entry per open layer --------------------------------------
  type Layer = { open: boolean; close: () => void };
  const layers = $derived<Layer[]>([
    { open: app.view !== "downloads", close: () => (app.view = "downloads") },
    { open: searching, close: closeSearch },
    { open: detail !== undefined, close: () => (detailId = undefined) },
    { open: menuOpen, close: () => (menuOpen = false) },
    { open: app.addOpen, close: () => (app.addOpen = false) },
    { open: app.removeIds !== undefined, close: () => (app.removeIds = undefined) },
    { open: app.refreshId !== undefined, close: () => (app.refreshId = undefined) },
    { open: app.clearIds !== undefined, close: () => (app.clearIds = undefined) },
  ]);
  const openLayers = $derived(layers.filter((l) => l.open));
  let pushed = 0;
  let skipPop = 0;
  $effect(() => {
    const depth = openLayers.length;
    untrack(() => {
      while (pushed < depth) {
        history.pushState({ wdmLayer: pushed + 1 }, "");
        pushed += 1;
      }
      if (pushed > depth) {
        // Closed from the UI: drop the entries it held, without closing anything else.
        skipPop += 1;
        history.go(depth - pushed);
        pushed = depth;
      }
    });
  });
  function onpopstate(): void {
    if (skipPop > 0) {
      skipPop -= 1;
      return;
    }
    if (pushed === 0) return;
    pushed -= 1;
    openLayers.at(-1)?.close();
  }
  // ------------------------------------------------------------------------------------------

  function setGroup(g: StateGroup): void {
    store.view = { kind: "group", group: g };
    document.getElementById(`wdm-chip-${g}`)?.scrollIntoView({ inline: "nearest", block: "nearest", behavior: "smooth" });
  }
  function openSearch(): void {
    searching = true;
    queueMicrotask(() => document.getElementById("wdm-search")?.focus());
  }
  function closeSearch(): void {
    searching = false;
    store.query = "";
  }
  function go(view: "history" | "settings"): void {
    menuOpen = false;
    app.view = view;
  }
  function openDetail(id: string): void {
    store.selectOnly(id);
    detailId = id;
  }
  // The details panel's own close button turns the desktop panel off; here it closes the sheet.
  $effect(() => {
    if (app.prefs.detailOpen) return;
    untrack(() => {
      app.prefs.detailOpen = true;
      if (detailId !== undefined) history.back();
    });
  });
  // The detail sheet goes when its download does (removed from the sheet itself).
  $effect(() => {
    if (detailId !== undefined && !detail) detailId = undefined;
  });
</script>

<svelte:window {onpopstate} />

<div class="vy-app wdm-mobile" data-theme={app.theme} lang={i18n.locale} dir={i18n.dir}>
  <div class="screen" inert={modal}>
    {#if app.view === "downloads"}
      <header class="bar" class:scrolled>
        <div class="row">
          {#if searching}
            <button class="icon-btn" aria-label={t("mobile.back")} onclick={() => history.back()}><Icon name="back" size={22} /></button>
            <input
              id="wdm-search"
              class="search"
              type="search"
              enterkeyhint="search"
              autocomplete="off"
              spellcheck="false"
              placeholder={t("toolbar.searchPlaceholder")}
              aria-label={t("toolbar.search")}
              bind:value={store.query}
            />
            {#if store.query}
              <button class="icon-btn" aria-label={t("list.noMatch.clear")} onclick={() => (store.query = "")}><Icon name="x" size={20} /></button>
            {/if}
          {:else}
            <div class="titles">
              <h1>{t("nav.downloads")}</h1>
              {#if subtitle}
                <p class="sub vy-num {subtitle.tone}" aria-live="polite">
                  {#if subtitle.tone === "live"}<Icon name="download" size={13} />{/if}{subtitle.text}
                </p>
              {/if}
            </div>
            <button class="icon-btn" aria-label={t("toolbar.search")} onclick={openSearch}><Icon name="search" size={21} /></button>
            <button class="icon-btn" aria-label={t("mobile.menu")} aria-haspopup="menu" aria-expanded={menuOpen} onclick={() => (menuOpen = true)}>
              <Icon name="more" size={21} />
            </button>
          {/if}
        </div>
        <div class="chips" role="tablist" aria-label={t("mobile.filters")}>
          {#each chips as g (g)}
            <button
              id="wdm-chip-{g}"
              class="chip"
              role="tab"
              aria-selected={group === g}
              onclick={() => setGroup(g)}
            >
              {t(g === "all" ? "mobile.all" : `group.${g}`)}
              <span class="count vy-num">{i18n.number(store.counts.groups[g])}</span>
            </button>
          {/each}
        </div>
      </header>

      <main class="list" onscroll={(e) => (scrolled = (e.currentTarget as HTMLElement).scrollTop > 4)}>
        <h2 class="vy-sr-only">{t(group === "all" ? "mobile.all" : `group.${group}`)}</h2>
        {#if emptyKind === "loading"}
          <p class="state" role="status">{t("list.loading")}</p>
        {:else if emptyKind === "error"}
          <p class="state" role="alert">{t("list.loadError", { message: store.loadError ?? "" })}</p>
        {:else if emptyKind === "noMatch"}
          <div class="state">
            <h2>{t("list.noMatch.title", { query: store.query.trim() })}</h2>
            <p>{t("list.noMatch.body")}</p>
          </div>
        {:else if emptyKind === "empty"}
          <div class="state empty">
            <span class="art" aria-hidden="true"><Icon name="download" size={30} /></span>
            <h2>{group === "all" ? t("list.empty.title") : t("mobile.emptyGroup")}</h2>
            {#if group === "all"}<p>{t("mobile.emptyBody")}</p>{/if}
          </div>
        {:else}
          <ul class="cards">
            {#each store.visible as d (d.id)}
              <li><MobileCard download={d} onopen={openDetail} /></li>
            {/each}
          </ul>
        {/if}
      </main>

      <button class="fab" class:compact={scrolled} onclick={() => app.openAdd()} aria-label={t("toolbar.add")}>
        <Icon name="plus" size={22} />
        <span class="fab-label">{t("mobile.add")}</span>
      </button>
    {:else}
      <header class="bar page">
        <div class="row">
          <button class="icon-btn" aria-label={t("mobile.back")} onclick={() => history.back()}><Icon name="back" size={22} /></button>
          <div class="titles"><h1>{t(app.view === "history" ? "nav.history" : "nav.settings")}</h1></div>
        </div>
      </header>
      <main class="page-body">
        {#if app.view === "history"}<HistoryView />{:else}<SettingsView />{/if}
      </main>
    {/if}
  </div>

  <Toasts />

  {#if menuOpen}
    <div class="menu-layer">
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div class="menu-scrim" onclick={() => history.back()}></div>
      <div class="menu" role="menu" aria-label={t("mobile.menu")}>
        <button role="menuitem" onclick={() => go("history")}><Icon name="history" size={20} />{t("nav.history")}</button>
        <button role="menuitem" onclick={() => go("settings")}><Icon name="settings" size={20} />{t("nav.settings")}</button>
      </div>
    </div>
  {/if}
  {#if detail}
    <DetailSheet download={detail} label={detail.data.filename} onclose={() => history.back()} />
  {/if}
  {#if app.addOpen}
    <!-- A link shared or handed over while the dialog is open starts it again with that link. -->
    {#key app.addUrl}
      <AddDownloadDialog initialUrl={app.addUrl} onclose={() => (app.addOpen = false)} />
    {/key}
  {/if}
  {#if app.removeIds}
    <RemoveDialog ids={app.removeIds} onclose={() => (app.removeIds = undefined)} />
  {/if}
  {#if app.refreshId}
    <RefreshLinkDialog id={app.refreshId} onclose={() => (app.refreshId = undefined)} />
  {/if}
  {#if app.clearIds}
    <ClearHistoryDialog ids={app.clearIds} onclose={() => (app.clearIds = undefined)} />
  {/if}
</div>

<style>
  /* Touch sizes: the shared tokens are tuned for a mouse and a 13px desktop body. */
  .wdm-mobile {
    --vy-font-size-xs: 0.75rem;
    --vy-font-size-sm: 0.8125rem;
    --vy-font-size-md: 0.9375rem;
    --vy-font-size-lg: 1.0625rem;
    --vy-font-size-xl: 1.25rem;
    --vy-font-size-2xl: 1.5rem;
    --vy-font-size-3xl: 1.875rem;
    --vy-size-control-sm: 2.25rem;
    --vy-size-control-md: 2.75rem;
    --vy-size-control-lg: 3.25rem;
    --inset-top: var(--wdm-inset-top, env(safe-area-inset-top, 0px));
    --inset-bottom: var(--wdm-inset-bottom, env(safe-area-inset-bottom, 0px));
    --inset-start: var(--wdm-inset-left, env(safe-area-inset-left, 0px));
    --inset-end: var(--wdm-inset-right, env(safe-area-inset-right, 0px));
    background: var(--vy-color-bg);
    -webkit-tap-highlight-color: transparent;
    overscroll-behavior: none;
  }
  .screen {
    display: flex;
    flex-direction: column;
    block-size: 100%;
    padding-inline: var(--inset-start) var(--inset-end);
  }

  /* ---- top bar ---- */
  .bar {
    position: relative;
    z-index: var(--vy-z-sticky);
    flex: none;
    padding-block-start: var(--inset-top);
    background: var(--vy-color-bg);
    transition: box-shadow var(--vy-duration-normal) var(--vy-ease-out);
  }
  /* Content scrolled under the bar: a hairline appears instead of a hard divider. */
  .bar.scrolled {
    box-shadow: 0 1px 0 var(--vy-color-border);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--vy-space-1);
    min-block-size: 60px;
    padding-inline: var(--vy-space-4) var(--vy-space-2);
  }
  .page .row {
    padding-inline-start: var(--vy-space-1);
  }
  .titles {
    flex: 1;
    min-inline-size: 0;
  }
  h1 {
    font-family: var(--vy-font-display);
    font-size: var(--vy-font-size-2xl);
    font-weight: var(--vy-font-weight-bold);
    letter-spacing: var(--vy-letter-spacing-tight);
    line-height: 1.15;
  }
  .sub {
    display: flex;
    align-items: center;
    gap: var(--vy-space-1);
    margin-block-start: 1px;
    font-size: var(--vy-font-size-sm);
    color: var(--vy-color-text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sub.live {
    color: var(--vy-color-accent-text);
    font-weight: var(--vy-font-weight-medium);
  }
  .sub.warn {
    color: var(--vy-color-warning-text);
  }
  .icon-btn {
    flex: none;
    display: grid;
    place-items: center;
    inline-size: 48px;
    block-size: 48px;
    border: 0;
    border-radius: var(--vy-radius-full);
    background: none;
    color: var(--vy-color-text);
    transition:
      background-color var(--vy-duration-fast) var(--vy-ease-out),
      scale 120ms var(--vy-ease-out);
  }
  .icon-btn:active {
    background: var(--vy-color-surface-hover);
    scale: 0.92;
  }
  .search {
    flex: 1;
    min-inline-size: 0;
    block-size: 44px;
    padding-inline: var(--vy-space-3);
    border: 0;
    border-radius: var(--vy-radius-full);
    background: var(--vy-color-surface-hover);
    color: var(--vy-color-text);
    font: inherit;
    font-size: var(--vy-font-size-lg);
    outline: none;
  }
  .search::-webkit-search-cancel-button {
    display: none;
  }

  /* ---- chips ---- */
  .chips {
    display: flex;
    gap: var(--vy-space-2);
    padding-block: var(--vy-space-1) var(--vy-space-3);
    padding-inline: var(--vy-space-4);
    overflow-x: auto;
    scrollbar-width: none;
    scroll-padding-inline: var(--vy-space-4);
  }
  .chips::-webkit-scrollbar {
    display: none;
  }
  .chip {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1-5);
    block-size: 36px;
    padding-inline: var(--vy-space-3-5, 14px);
    border: 1px solid var(--vy-color-border);
    border-radius: var(--vy-radius-full);
    background: transparent;
    color: var(--vy-color-text);
    font: inherit;
    font-size: var(--vy-font-size-sm);
    font-weight: var(--vy-font-weight-medium);
    white-space: nowrap;
    transition:
      background-color var(--vy-duration-fast) var(--vy-ease-out),
      border-color var(--vy-duration-fast) var(--vy-ease-out),
      scale 120ms var(--vy-ease-out);
  }
  .chip:active {
    scale: 0.95;
  }
  .chip[aria-selected="true"] {
    border-color: transparent;
    background: var(--vy-color-text);
    color: var(--vy-color-bg);
  }
  .count {
    color: var(--vy-color-text-muted);
    font-weight: var(--vy-font-weight-regular);
  }
  .chip[aria-selected="true"] .count {
    color: inherit;
    opacity: 0.7;
  }

  /* ---- list ---- */
  .list {
    flex: 1;
    min-block-size: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    padding-block: var(--vy-space-1) calc(var(--inset-bottom) + 96px);
    padding-inline: var(--vy-space-3);
  }
  .cards {
    display: grid;
    gap: var(--vy-space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .state {
    display: grid;
    justify-items: center;
    gap: var(--vy-space-2);
    padding-block: 18vh var(--vy-space-6);
    padding-inline: var(--vy-space-6);
    text-align: center;
    color: var(--vy-color-text-muted);
  }
  .state h2 {
    font-size: var(--vy-font-size-xl);
    font-weight: var(--vy-font-weight-semibold);
    color: var(--vy-color-text);
  }
  .art {
    display: grid;
    place-items: center;
    inline-size: 72px;
    block-size: 72px;
    margin-block-end: var(--vy-space-2);
    border-radius: var(--vy-radius-xl);
    background: var(--vy-color-accent-subtle);
    color: var(--vy-color-accent-text);
  }

  /* ---- add button ---- */
  .fab {
    position: fixed;
    inset-block-end: calc(var(--inset-bottom) + var(--vy-space-5));
    inset-inline-end: calc(var(--inset-end) + var(--vy-space-4));
    z-index: var(--vy-z-sticky);
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-2);
    block-size: 56px;
    padding-inline: var(--vy-space-5) var(--vy-space-6);
    border: 0;
    border-radius: var(--vy-radius-full);
    background: var(--vy-color-primary);
    color: var(--vy-color-primary-text);
    font: inherit;
    font-family: var(--vy-font-display);
    font-size: var(--vy-font-size-lg);
    font-weight: var(--vy-font-weight-bold);
    box-shadow:
      0 10px 24px -8px color-mix(in srgb, var(--vy-color-primary) 60%, transparent),
      0 2px 6px rgb(0 0 0 / 0.18);
    transition:
      padding 240ms var(--vy-ease-out),
      scale 120ms var(--vy-ease-out);
  }
  .fab:active {
    scale: 0.94;
  }
  .fab-label {
    max-inline-size: 12em;
    overflow: hidden;
    white-space: nowrap;
    transition:
      max-inline-size 240ms var(--vy-ease-out),
      opacity 160ms var(--vy-ease-out);
  }
  /* Scrolled: just the plus, so it covers less of the list. */
  .fab.compact {
    padding-inline: 17px;
    gap: 0;
  }
  .fab.compact .fab-label {
    max-inline-size: 0;
    opacity: 0;
  }

  /* ---- pages (History, Settings) ---- */
  .page-body {
    flex: 1;
    min-block-size: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    padding-block-end: var(--inset-bottom);
  }
  .page-body > :global(*) {
    flex: 1 0 auto;
  }
  /* The page's own big heading repeats the bar's title; its buttons (Save, Revert, search) stay. */
  .page-body :global(.settings .bar h1),
  .page-body :global(.history h1) {
    display: none;
  }

  /* ---- menu ---- */
  .menu-layer {
    position: fixed;
    inset: 0;
    z-index: var(--vy-z-dropdown);
  }
  .menu-scrim {
    position: absolute;
    inset: 0;
  }
  .menu {
    position: absolute;
    inset-block-start: calc(var(--inset-top) + 52px);
    inset-inline-end: calc(var(--inset-end) + var(--vy-space-2));
    display: grid;
    min-inline-size: 200px;
    padding-block: var(--vy-space-1-5);
    border-radius: var(--vy-radius-lg);
    background: var(--vy-color-surface-raised);
    box-shadow: var(--vy-shadow-lg);
    transform-origin: top right;
    transition:
      opacity 160ms var(--vy-ease-out),
      scale 200ms var(--vy-ease-out);
  }
  :global([dir="rtl"]) .menu {
    transform-origin: top left;
  }
  @starting-style {
    .menu {
      opacity: 0;
      scale: 0.92;
    }
  }
  .menu button {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    min-block-size: 48px;
    padding-inline: var(--vy-space-4) var(--vy-space-6);
    border: 0;
    background: none;
    color: var(--vy-color-text);
    font: inherit;
    text-align: start;
  }
  .menu button:active {
    background: var(--vy-color-surface-hover);
  }

  @media (prefers-reduced-motion: reduce) {
    .chip:active,
    .fab:active,
    .icon-btn:active {
      scale: none;
    }
    .fab,
    .fab-label {
      transition: none;
    }
    @starting-style {
      .menu {
        scale: none;
      }
    }
  }
</style>
