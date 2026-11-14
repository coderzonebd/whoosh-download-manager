<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import Icon from "./Icon.svelte";

  const app = getApp();
  const { store, prefs, i18n } = app;
  const { t } = i18n;

  const selected = $derived(store.selected());
  const canPause = $derived(app.pausableIds(selected).length > 0);
  const canResume = $derived(app.resumableIds(selected).length > 0);
  const canRemove = $derived(selected.length > 0);
  /** Selection actions, search and layout belong to the download list only. */
  const onSettings = $derived(app.view !== "downloads");
</script>

<div class="toolbar">
  <button class="vy-btn vy-btn--icon nav-toggle" aria-label={t(app.navOpen ? "nav.close" : "nav.open")} aria-expanded={app.navOpen} onclick={() => (app.navOpen = !app.navOpen)}>
    <Icon name="menu" size={18} />
  </button>

  <button class="vy-btn vy-btn--primary add" onclick={() => app.openAdd()}>
    <Icon name="plus" size={16} />
    <span class="text">{t("toolbar.add")}</span>
  </button>

  {#if !onSettings}
    <div class="group" role="group" aria-label={t("toolbar.label")}>
      <button class="vy-btn" disabled={!canResume} onclick={() => app.resume()} title={t("toolbar.resume")}>
        <Icon name="play" size={15} />
        <span class="text">{t("toolbar.resume")}</span>
      </button>
      <button class="vy-btn" disabled={!canPause} onclick={() => app.pause()} title={t("toolbar.pause")}>
        <Icon name="pause" size={15} />
        <span class="text">{t("toolbar.pause")}</span>
      </button>
      <button class="vy-btn" disabled={!canRemove} onclick={() => app.requestRemove()} title={t("toolbar.remove")}>
        <Icon name="trash" size={15} />
        <span class="text">{t("toolbar.remove")}</span>
      </button>
    </div>

    <div class="spacer"></div>

    <div class="search">
      <Icon name="search" size={15} />
      <input
        id="vy-search"
        class="vy-input"
        type="search"
        aria-label={t("toolbar.search")}
        placeholder={t("toolbar.searchPlaceholder")}
        bind:value={store.query}
        onkeydown={(e) => {
          if (e.key === "Escape" && store.query) {
            e.preventDefault();
            store.query = "";
          }
        }}
      />
    </div>

    <div class="segmented" role="group" aria-label={t("toolbar.layout")}>
      <button class="vy-btn vy-btn--icon" aria-pressed={prefs.layout === "classic"} aria-label={t("layout.classic")} title={t("layout.classic")} onclick={() => (prefs.layout = "classic")}>
        <Icon name="rows" size={16} />
      </button>
      <button class="vy-btn vy-btn--icon" aria-pressed={prefs.layout === "modern"} aria-label={t("layout.modern")} title={t("layout.modern")} onclick={() => (prefs.layout = "modern")}>
        <Icon name="cards" size={16} />
      </button>
    </div>

    <button class="vy-btn vy-btn--icon toggle" aria-pressed={prefs.detailOpen} aria-label={t("toolbar.details")} title={t("toolbar.details")} onclick={() => (prefs.detailOpen = !prefs.detailOpen)}>
      <Icon name="panel" size={16} />
    </button>
  {:else}
    <div class="spacer"></div>
  {/if}

  <button class="vy-btn vy-btn--icon" aria-label={t("toolbar.shortcuts")} title={t("toolbar.shortcuts")} onclick={() => (app.shortcutsOpen = true)}>
    <Icon name="keyboard" size={17} />
  </button>
</div>

<style>
  .toolbar {
    display: flex;
    align-items: center;
    gap: var(--vy-space-1);
    flex: none;
    block-size: var(--vy-size-toolbar);
    padding-inline: var(--vy-space-3);
    border-block-end: 1px solid var(--vy-color-border);
    background: var(--vy-color-surface);
  }
  .add {
    margin-inline-end: var(--vy-space-2);
  }
  .group {
    display: flex;
    gap: 2px;
  }
  .group .vy-btn {
    padding-inline: var(--vy-space-2-5);
    color: var(--vy-color-text-muted);
  }
  .group .vy-btn:not(:disabled) {
    color: var(--vy-color-text);
  }
  .spacer {
    flex: 1;
  }
  .search {
    position: relative;
    display: flex;
    align-items: center;
    inline-size: min(260px, 28cqi);
    margin-inline-end: var(--vy-space-2);
    color: var(--vy-color-text-subtle);
  }
  .search > :global(.icon) {
    position: absolute;
    inset-inline-start: var(--vy-space-2-5);
    pointer-events: none;
  }
  .search input {
    padding-inline-start: var(--vy-space-8);
    background: var(--vy-color-bg);
    border-color: var(--vy-color-border-strong);
  }
  .search input::-webkit-search-cancel-button {
    cursor: pointer;
  }
  .segmented {
    display: flex;
    padding: 2px;
    border-radius: var(--vy-radius-sm);
    background: var(--vy-color-bg);
    box-shadow: inset 0 0 0 1px var(--vy-color-border);
  }
  .segmented .vy-btn {
    min-block-size: 28px;
    inline-size: 30px;
    color: var(--vy-color-text-muted);
  }
  .segmented .vy-btn[aria-pressed="true"] {
    background: var(--vy-color-surface);
    color: var(--vy-color-text);
    box-shadow: var(--vy-shadow-sm), inset 0 0 0 1px var(--vy-color-border);
  }
  .toggle[aria-pressed="true"] {
    color: var(--vy-color-accent-text);
    background: var(--vy-color-accent-subtle);
  }
  .nav-toggle {
    display: none;
  }

  @container vy-app (max-width: 960px) {
    .group .text {
      display: none;
    }
  }
  @container vy-app (max-width: 600px) {
    /* the list switches to cards on its own at this width */
    .segmented {
      display: none;
    }
  }
  @container vy-app (max-width: 760px) {
    .nav-toggle {
      display: inline-flex;
    }
    .add .text {
      display: none;
    }
    .add {
      inline-size: var(--vy-size-control-md);
      padding-inline: 0;
    }
    .search {
      inline-size: auto;
      flex: 1;
      min-inline-size: 80px;
    }
    .spacer {
      display: none;
    }
  }
</style>
