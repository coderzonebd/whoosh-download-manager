<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Finished and failed downloads, newest first, with search. Clearing an
  entry removes it from the list (`download.remove`); the file stays on disk.
-->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import type { Download } from "../lib/state/downloads.svelte.ts";
  import FileIcon from "./FileIcon.svelte";
  import Icon from "./Icon.svelte";
  import StatusBadge from "./StatusBadge.svelte";

  type Filter = "all" | "completed" | "failed";
  const PAGE = 200;

  const app = getApp();
  const { store, i18n } = app;
  const { t } = i18n;

  let query = $state("");
  let filter = $state<Filter>("all");
  let limit = $state(PAGE);

  const when = (d: Download) => d.data.completedAt ?? d.data.addedAt;

  const entries = $derived.by(() => {
    const q = query.trim().toLocaleLowerCase();
    return store
      .all()
      .filter((d) => {
        const s = d.data.state;
        if (s !== "completed" && s !== "failed") return false;
        if (filter !== "all" && s !== filter) return false;
        return !q || d.searchKey.includes(q) || d.data.url.toLocaleLowerCase().includes(q);
      })
      .sort((a, b) => when(b) - when(a) || (a.id < b.id ? -1 : 1));
  });
  const shown = $derived(entries.slice(0, limit));
  const total = $derived(store.counts.groups.completed + store.counts.groups.attention);

  // A new search starts from the top again.
  $effect(() => {
    void query;
    void filter;
    limit = PAGE;
  });

  const FILTERS: Filter[] = ["all", "completed", "failed"];
</script>

<div class="history">
  <div class="head">
    <h1>{t("history.title")}</h1>
    <div class="search">
      <Icon name="search" size={15} />
      <input
        class="vy-input"
        type="search"
        aria-label={t("history.search")}
        placeholder={t("toolbar.searchPlaceholder")}
        bind:value={query}
        onkeydown={(e) => {
          if (e.key === "Escape" && query) {
            e.preventDefault();
            query = "";
          }
        }}
      />
    </div>
    <div class="choice" role="radiogroup" aria-label={t("history.filter.label")}>
      {#each FILTERS as f (f)}
        <label class:on={filter === f}>
          <input type="radio" name="vy-history-filter" value={f} bind:group={filter} />
          {t(`history.filter.${f}`)}
        </label>
      {/each}
    </div>
    <button class="vy-btn" disabled={entries.length === 0} onclick={() => (app.clearIds = entries.map((d) => d.id))}>
      <Icon name="trash" size={15} />
      {query.trim() || filter !== "all" ? t("history.clearShown") : t("history.clear")}
    </button>
  </div>

  <p class="vy-sr-only" role="status">{t("history.count", { count: entries.length })}</p>

  {#if total === 0}
    <div class="empty">
      <Icon name="history" size={32} />
      <h2>{t("history.empty.title")}</h2>
      <p>{t("history.empty.body")}</p>
    </div>
  {:else if entries.length === 0}
    <div class="empty">
      <Icon name="search" size={32} />
      <h2>{t("history.noMatch")}</h2>
    </div>
  {:else}
    <ul class="list" aria-label={t("history.title")}>
      {#each shown as d (d.id)}
        {@const failed = d.data.state === "failed"}
        <li class="entry">
          <FileIcon category={d.category} />
          <div class="main">
            <span class="name vy-truncate" dir="auto" title={d.data.filename}>{d.data.filename}</span>
            <span class="facts">
              <StatusBadge state={d.data.state} />
              {#if !failed}<span class="vy-num">{i18n.bytes(d.data.totalBytes ?? d.data.downloadedBytes)}</span>{/if}
              <span class="vy-num"><time datetime={new Date(when(d)).toISOString()}>{i18n.date(when(d))}</time></span>
              <span class="dir vy-truncate" dir="auto" title={d.data.directory}>{d.data.directory}</span>
            </span>
            {#if failed && d.data.error}<span class="err vy-truncate">{d.data.error.message}</span>{/if}
          </div>
          <div class="actions">
            {#if failed}
              <button class="vy-btn vy-btn--sm" onclick={() => app.resume([d.id])}>
                <Icon name="refresh" size={13} />{t("action.retry")}
              </button>
            {/if}
            <button class="vy-btn vy-btn--sm" onclick={() => app.reveal(d.id)}>{t("history.showInList")}</button>
            <button
              class="vy-btn vy-btn--sm vy-btn--icon"
              aria-label={t("history.removeOne", { name: d.data.filename })}
              title={t("history.removeOne", { name: d.data.filename })}
              onclick={() => app.clearHistory([d.id])}
            >
              <Icon name="x" size={14} />
            </button>
          </div>
        </li>
      {/each}
    </ul>
    {#if entries.length > shown.length}
      <div class="more">
        <button class="vy-btn" onclick={() => (limit += PAGE)}>{t("history.more", { count: entries.length - shown.length })}</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .history {
    flex: 1;
    overflow-y: auto;
    background: var(--vy-color-bg);
  }
  .head {
    position: sticky;
    inset-block-start: 0;
    z-index: 1;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--vy-space-2) var(--vy-space-3);
    padding-block: var(--vy-space-4) var(--vy-space-3);
    padding-inline: var(--vy-space-6);
    background: color-mix(in srgb, var(--vy-color-bg) 88%, transparent);
    backdrop-filter: blur(8px);
    border-block-end: 1px solid var(--vy-color-border);
  }
  h1 {
    margin-inline-end: auto;
    font-size: var(--vy-font-size-2xl);
    font-weight: var(--vy-font-weight-semibold);
    letter-spacing: var(--vy-letter-spacing-tight);
  }
  .search {
    position: relative;
    display: flex;
    align-items: center;
    color: var(--vy-color-text-subtle);
  }
  .search > :global(.icon) {
    position: absolute;
    inset-inline-start: var(--vy-space-2-5);
    pointer-events: none;
  }
  .search input {
    inline-size: 220px;
    padding-inline-start: var(--vy-space-8);
    color: var(--vy-color-text);
  }
  .choice {
    display: flex;
    padding: 2px;
    border-radius: var(--vy-radius-sm);
    background: var(--vy-color-bg-sunken);
    box-shadow: inset 0 0 0 1px var(--vy-color-border-strong);
  }
  .choice label {
    position: relative;
    display: inline-flex;
    align-items: center;
    min-block-size: 26px;
    padding-inline: var(--vy-space-2-5);
    border-radius: calc(var(--vy-radius-sm) - 1px);
    color: var(--vy-color-text-muted);
    cursor: pointer;
    white-space: nowrap;
  }
  .choice label.on {
    background: var(--vy-color-surface);
    color: var(--vy-color-text);
    font-weight: var(--vy-font-weight-medium);
    box-shadow: var(--vy-shadow-sm), inset 0 0 0 1px var(--vy-color-border);
  }
  .choice input {
    position: absolute;
    inset: 0;
    margin: 0;
    opacity: 0;
    cursor: pointer;
  }
  .choice label:has(input:focus-visible) {
    outline: var(--vy-size-focus-width) solid var(--vy-color-focus);
    outline-offset: 1px;
  }
  .list {
    display: grid;
    margin: 0;
    padding-block: var(--vy-space-2);
    padding-inline: var(--vy-space-4);
    list-style: none;
  }
  .entry {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    padding-block: var(--vy-space-2-5);
    padding-inline: var(--vy-space-2);
    border-block-end: 1px solid var(--vy-color-border);
  }
  .main {
    display: grid;
    flex: 1;
    min-inline-size: 0;
    gap: 2px;
  }
  .name {
    font-weight: var(--vy-font-weight-medium);
  }
  .facts {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    min-inline-size: 0;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .dir {
    min-inline-size: 0;
    color: var(--vy-color-text-subtle);
  }
  .err {
    color: var(--vy-color-danger-text);
    font-size: var(--vy-font-size-sm);
  }
  .actions {
    display: flex;
    flex: none;
    gap: var(--vy-space-1-5);
  }
  .empty {
    display: grid;
    justify-items: center;
    gap: var(--vy-space-2);
    padding: var(--vy-space-12, 48px) var(--vy-space-6);
    color: var(--vy-color-text-muted);
    text-align: center;
  }
  .empty h2 {
    color: var(--vy-color-text);
    font-size: var(--vy-font-size-lg);
    font-weight: var(--vy-font-weight-semibold);
  }
  .more {
    display: flex;
    justify-content: center;
    padding: var(--vy-space-4);
  }
  @container vy-app (max-width: 560px) {
    .search input {
      inline-size: 100%;
    }
    .facts .dir {
      display: none;
    }
  }
</style>
