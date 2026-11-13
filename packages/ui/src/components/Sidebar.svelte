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
    if (a.kind !== b.kind) return false;
    if (a.kind === "group" && b.kind === "group") return a.group === b.group;
    if (a.kind === "category" && b.kind === "category") return a.category === b.category;
    return a.kind === "queue";
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
      <h2 id="vy-nav-queues">{t("nav.queues")}</h2>
      <ul>
        <li>
          <button class="item" aria-current={current({ kind: "queue", queue: "main" })} onclick={() => go({ kind: "queue", queue: "main" })}>
            <Icon name="layers" size={16} />
            <span class="label vy-truncate">{t("queue.main")}</span>
            {#if counts.mainQueue > 0}<span class="count vy-num">{i18n.number(counts.mainQueue)}</span>{/if}
          </button>
        </li>
      </ul>
    </section>
  </div>

  <div class="foot">
    <button
      class="item"
      aria-current={app.view === "settings" ? "page" : undefined}
      onclick={() => {
        app.view = "settings";
        app.navOpen = false;
      }}
    >
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
    font-size: var(--vy-font-size-lg);
    font-weight: var(--vy-font-weight-bold);
    letter-spacing: var(--vy-letter-spacing-tight);
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
    padding-inline: var(--vy-space-2-5);
    border: 0;
    border-radius: var(--vy-radius-sm);
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
    background: var(--vy-color-surface);
    color: var(--vy-color-text);
    font-weight: var(--vy-font-weight-medium);
    box-shadow: var(--vy-shadow-sm), inset 0 0 0 1px var(--vy-color-border);
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
    padding: var(--vy-space-2);
    border-block-start: 1px solid var(--vy-color-border);
  }
</style>
