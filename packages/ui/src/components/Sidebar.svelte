<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import { CATEGORIES, type Category } from "@whoosh/api-client/categories";
  import { getApp } from "../lib/state/app.svelte.ts";
  import type { StateGroup, View } from "../lib/state/downloads.svelte.ts";
  import Icon from "./Icon.svelte";
  import type { IconName } from "./icons.ts";

  const app = getApp();
  const { store, i18n } = app;
  const { t } = i18n;

  const GROUPS: { group: StateGroup; icon: IconName }[] = [
    { group: "all", icon: "inbox" },
    { group: "active", icon: "arrowCircle" },
    { group: "queued", icon: "clock" },
    { group: "paused", icon: "pauseCircle" },
    { group: "completed", icon: "checkCircle" },
    { group: "attention", icon: "alertCircle" },
  ];
  const CATEGORY_ICON: Record<Category, IconName> = {
    video: "video",
    music: "music",
    documents: "document",
    compressed: "archive",
    programs: "app",
    images: "image",
    other: "file",
  };

  function same(a: View, b: View): boolean {
    if (a.kind === "group" && b.kind === "group") return a.group === b.group;
    if (a.kind === "category" && b.kind === "category") return a.category === b.category;
    if (a.kind === "queue" && b.kind === "queue") return a.queue === b.queue;
    return false;
  }

  function current(v: View): "page" | undefined {
    return app.view === "downloads" && same(store.view, v) ? "page" : undefined;
  }

  function go(v: View): void {
    store.view = v;
    app.view = "downloads";
    app.navOpen = false;
  }

  const counts = $derived(store.counts);
  const queues = app.queues;
  const queueNames = $derived(["main", ...(queues.enabled ? queues.named.map((q) => q.name) : [])]);

  function foot(view: "history" | "settings"): void {
    app.view = view;
    app.navOpen = false;
  }
</script>

<nav class="sidebar" aria-label={t("nav.label")}>
  <div class="brand">
    <span class="logo" aria-hidden="true"><Icon name="wind" size={18} /></span>
    <span class="wordmark">{t("app.name")}</span>
  </div>

  <div class="scroll">
    <section aria-labelledby="vy-nav-downloads">
      <h2 id="vy-nav-downloads">{t("nav.downloads")}</h2>
      <ul>
        {#each GROUPS as g (g.group)}
          {@const v: View = { kind: "group", group: g.group }}
          {@const n = counts.groups[g.group]}
          <li>
            <button class="item" aria-current={current(v)} onclick={() => go(v)}>
              <Icon name={g.icon} size={16} />
              <span class="label vy-truncate">{t(`group.${g.group}`)}</span>
              {#if n > 0}
                <span class="count vy-num" class:alert={g.group === "attention"}>{i18n.number(n)}</span>
              {/if}
            </button>
          </li>
        {/each}
      </ul>
    </section>

    <section aria-labelledby="vy-nav-categories">
      <h2 id="vy-nav-categories">{t("nav.categories")}</h2>
      <ul>
        {#each CATEGORIES as c (c)}
          {@const v: View = { kind: "category", category: c }}
          {@const n = counts.categories[c]}
          <li>
            <button class="item" class:empty={n === 0} aria-current={current(v)} onclick={() => go(v)}>
              <span class="cat" style:--tint="var(--vy-cat-{c})"><Icon name={CATEGORY_ICON[c]} size={16} /></span>
              <span class="label vy-truncate">{t(`category.${c}`)}</span>
              {#if n > 0}<span class="count vy-num">{i18n.number(n)}</span>{/if}
            </button>
          </li>
        {/each}
      </ul>
    </section>

    <section aria-labelledby="vy-nav-queues">
      <div class="section-head">
        <h2 id="vy-nav-queues">{t("nav.queues")}</h2>
        {#if queues.enabled}
          <button class="vy-btn vy-btn--icon vy-btn--sm add" aria-label={t("queue.new")} title={t("queue.new")} onclick={() => (app.queueDialog = { mode: "create" })}>
            <Icon name="plus" size={14} />
          </button>
        {/if}
      </div>
      <ul>
        {#each queueNames as name (name)}
          {@const v: View = { kind: "queue", queue: name }}
          {@const q = queues.get(name)}
          {@const n = counts.queues[name] ?? 0}
          <li>
            <button class="item" aria-current={current(v)} onclick={() => go(v)}>
              <Icon name="layers" size={16} />
              <span class="label vy-truncate" dir="auto">{app.queueLabel(name)}</span>
              {#if q && q.name !== "main" && !q.running}
                <span class="state" title={t("queue.stopped")}><Icon name="pause" size={12} /><span class="vy-sr-only">{t("queue.stopped")}</span></span>
              {:else if q?.schedule}
                <span class="state" title={t("queue.scheduled")}><Icon name="clock" size={12} /><span class="vy-sr-only">{t("queue.scheduled")}</span></span>
              {/if}
              {#if n > 0}<span class="count vy-num">{i18n.number(n)}</span>{/if}
            </button>
          </li>
        {/each}
      </ul>
    </section>
  </div>

  <div class="foot">
    <button class="item" aria-current={app.view === "history" ? "page" : undefined} onclick={() => foot("history")}>
      <Icon name="history" size={16} />
      <span class="label">{t("nav.history")}</span>
    </button>
    <button class="item" aria-current={app.view === "settings" ? "page" : undefined} onclick={() => foot("settings")}>
      <Icon name="settings" size={16} />
      <span class="label">{t("nav.settings")}</span>
    </button>
  </div>
</nav>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    min-block-size: 0;
    inline-size: var(--vy-size-sidebar);
    background: var(--vy-color-bg-sunken);
    border-inline-end: 1px solid var(--vy-color-border);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2);
    block-size: var(--vy-size-toolbar);
    padding-inline: var(--vy-space-4);
    flex: none;
  }
  .logo {
    display: grid;
    place-items: center;
    inline-size: 28px;
    block-size: 28px;
    border-radius: var(--vy-radius-md);
    color: var(--vy-color-text-on-accent);
    background: linear-gradient(145deg, var(--vy-color-accent), color-mix(in srgb, var(--vy-color-accent) 55%, var(--vy-color-info)));
    box-shadow: var(--vy-shadow-sm);
  }
  .wordmark {
    font-family: var(--vy-font-display);
    font-size: var(--vy-font-size-2xl);
    font-weight: 800;
    line-height: 1;
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    padding-block-end: var(--vy-space-4);
  }
  section + section {
    margin-block-start: var(--vy-space-4);
  }
  h2 {
    padding-block: var(--vy-space-1);
    padding-inline: var(--vy-space-4);
    color: var(--vy-color-text-subtle);
    font-size: var(--vy-font-size-xs);
    font-weight: var(--vy-font-weight-semibold);
    letter-spacing: var(--vy-letter-spacing-wide);
    text-transform: uppercase;
  }
  :global(:lang(bn)) h2 {
    font-size: var(--vy-font-size-sm);
    letter-spacing: 0;
  }
  ul {
    display: grid;
    gap: 1px;
    margin: 0;
    padding-block: 0;
    padding-inline: var(--vy-space-2);
    list-style: none;
  }
  .item {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2-5);
    inline-size: 100%;
    min-block-size: 30px;
    padding-block: 0;
    padding-inline: var(--vy-space-3);
    border: 0;
    border-radius: var(--vy-radius-full);
    background: transparent;
    color: var(--vy-color-text-muted);
    text-align: start;
    cursor: pointer;
    transition:
      background-color var(--vy-duration-fast) var(--vy-ease-standard),
      color var(--vy-duration-fast) var(--vy-ease-standard);
  }
  @media (hover: hover) and (pointer: fine) {
    .item:hover {
      background: var(--vy-color-surface-hover);
      color: var(--vy-color-text);
    }
  }
  .item[aria-current="page"] {
    background: var(--vy-color-selection-strong);
    color: var(--vy-color-accent-text);
    font-weight: var(--vy-font-weight-semibold);
  }
  .item[aria-current="page"] > :global(.icon) {
    color: var(--vy-color-accent-text);
  }
  :global([data-theme="high-contrast"]) .item[aria-current="page"] {
    box-shadow: inset 0 0 0 2px var(--vy-color-accent);
  }
  .item.empty:not([aria-current]) {
    color: var(--vy-color-text-subtle);
  }
  .label {
    flex: 1;
  }
  .cat {
    display: inline-flex;
    color: var(--tint);
  }
  .count {
    flex: none;
    min-inline-size: 1.6em;
    padding-inline: 6px;
    border-radius: var(--vy-radius-full);
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-xs);
    font-weight: var(--vy-font-weight-medium);
    text-align: center;
  }
  .count.alert {
    color: var(--vy-color-warning-text);
    background: var(--vy-color-warning-subtle);
  }
  .foot {
    flex: none;
    display: grid;
    gap: 1px;
    padding: var(--vy-space-2);
    border-block-start: 1px solid var(--vy-color-border);
  }
  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-inline-end: var(--vy-space-2);
  }
  .section-head .add {
    color: var(--vy-color-text-subtle);
  }
  .state {
    display: inline-flex;
    flex: none;
    color: var(--vy-color-text-subtle);
  }
</style>
