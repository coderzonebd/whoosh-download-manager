<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  The download list: one virtualised ARIA grid with two layouts.
  - Classic: dense sortable/resizable table (what IDM users know).
  - Modern: one card per row with richer status.
  Only the rows in view (plus a small overscan) exist in the DOM, so 10 000
  downloads cost the same as 30. The grid is a single tab stop; the active
  row is exposed with aria-activedescendant and kept scrolled into view.
-->
<script lang="ts">
  import { untrack } from "svelte";
  import { COLUMNS, HEADER_HEIGHT, ROW_HEIGHT } from "../lib/columns.ts";
  import { getApp } from "../lib/state/app.svelte.ts";
  import { pageSize, rowDomId, scrollTopFor, visibleRange } from "../lib/virtual.ts";
  import ClassicHeader from "./ClassicHeader.svelte";
  import ClassicRow from "./ClassicRow.svelte";
  import Icon from "./Icon.svelte";
  import ModernCard from "./ModernCard.svelte";

  const app = getApp();
  const { store, prefs, i18n } = app;
  const { t } = i18n;

  /** Below this width the table cannot breathe (extension popup, phone): cards it is. */
  const NARROW = 560;
  let width = $state(1024);
  const layout = $derived(width < NARROW ? "modern" : prefs.layout);
  const rowH = $derived(ROW_HEIGHT[layout]);
  const headerH = $derived(layout === "classic" ? HEADER_HEIGHT : 0);
  const items = $derived(store.visible);

  let scroller: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  /** jsdom has no layout; 600px keeps tests meaningful. */
  let viewport = $state(600);

  const range = $derived(visibleRange(scrollTop, viewport - headerH, rowH, items.length));
  const slice = $derived(items.slice(range.start, range.end));

  const template = $derived(COLUMNS.map((c) => (c.id === "name" ? `minmax(${prefs.widths.name}px, 1fr)` : `${prefs.widths[c.id]}px`)).join(" "));
  const minWidth = $derived(COLUMNS.reduce((a, c) => a + prefs.widths[c.id], 0));

  const activeIndex = $derived(store.indexOf(store.activeId));
  const activeRendered = $derived(activeIndex >= range.start && activeIndex < range.end);

  // Track the viewport size.
  $effect(() => {
    const el = scroller;
    if (!el) return;
    if (el.clientHeight > 0) viewport = el.clientHeight;
    if (el.clientWidth > 0) width = el.clientWidth;
    const ro = new ResizeObserver((entries) => {
      const r = entries[0]?.contentRect;
      if (r?.height) viewport = r.height;
      if (r?.width) width = r.width;
    });
    ro.observe(el);
    return () => ro.disconnect();
  });

  // Tell the segment poller which rows are on screen.
  $effect(() => {
    const ids = slice.map((d) => d.id);
    untrack(() => app.segments.setVisible(ids));
  });

  // A new view or search starts at the top.
  $effect(() => {
    void store.view;
    void store.query;
    untrack(() => {
      if (scroller) scroller.scrollTop = 0;
      scrollTop = 0;
    });
  });

  // Someone (a toast's "Show", the add dialog) asked for focus: take it after
  // any closing dialog has handed focus back to its trigger.
  $effect(() => {
    if (app.listFocus === 0) return;
    const t = setTimeout(() => scroller?.focus(), 0);
    return () => clearTimeout(t);
  });

  // Keep the active row in view when it changes (keyboard, reveal from a toast).
  $effect(() => {
    const i = activeIndex;
    if (i < 0) return;
    untrack(() => ensureVisible(i));
  });

  function ensureVisible(i: number): void {
    if (!scroller) return;
    const next = scrollTopFor(i, scroller.scrollTop, viewport, rowH, headerH);
    if (next !== undefined) {
      scroller.scrollTop = next;
      scrollTop = next;
    }
  }

  function isMod(e: KeyboardEvent | MouseEvent): boolean {
    return e.ctrlKey || e.metaKey;
  }

  function moveTo(index: number, e: KeyboardEvent): void {
    if (items.length === 0) return;
    const i = Math.max(0, Math.min(items.length - 1, index));
    const id = items[i]?.id;
    if (id === undefined) return;
    if (e.shiftKey) store.selectRange(id);
    else if (isMod(e)) store.activeId = id;
    else store.selectOnly(id);
  }

  function openMenuForActive(): void {
    const id = store.activeId;
    if (id === undefined || !scroller) return;
    const row = scroller.querySelector<HTMLElement>(`#${rowDomId(id)}`);
    const r = (row ?? scroller).getBoundingClientRect();
    if (!store.selection.has(id)) store.selectOnly(id);
    app.menu = { id, x: r.left + 24, y: r.top + r.height / 2 };
  }

  function onkeydown(e: KeyboardEvent): void {
    // Keys typed on a column header button (sort, resize) are theirs, not the grid's.
    if (e.target !== scroller) return;
    const cur = activeIndex;
    const page = pageSize(viewport, rowH, headerH);
    switch (e.key) {
      case "ArrowDown":
        moveTo(cur < 0 ? range.start : cur + 1, e);
        break;
      case "ArrowUp":
        moveTo(cur < 0 ? range.start : cur - 1, e);
        break;
      case "PageDown":
        moveTo(cur + page, e);
        break;
      case "PageUp":
        moveTo(cur - page, e);
        break;
      case "Home":
        moveTo(0, e);
        break;
      case "End":
        moveTo(items.length - 1, e);
        break;
      case "a":
      case "A":
        if (!isMod(e)) return;
        store.selectAll();
        break;
      case " ":
        if (e.ctrlKey && store.activeId !== undefined) store.toggle(store.activeId);
        else void app.togglePause();
        break;
      case "Enter":
        if (store.activeId === undefined) return;
        if (store.selection.size === 0) store.selectOnly(store.activeId);
        prefs.detailOpen = true;
        break;
      case "Delete":
        app.requestRemove();
        break;
      case "Backspace":
        if (!e.metaKey) return;
        app.requestRemove();
        break;
      case "Escape":
        if (store.selection.size > 0) store.clearSelection();
        else return;
        break;
      case "ContextMenu":
        openMenuForActive();
        break;
      case "F10":
        if (!e.shiftKey) return;
        openMenuForActive();
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  function rowIdAt(e: Event): string | undefined {
    const el = (e.target as Element | null)?.closest<HTMLElement>("[data-row-id]");
    return el?.dataset.rowId;
  }

  function onclick(e: MouseEvent): void {
    const id = rowIdAt(e);
    if (id === undefined) return;
    if (e.shiftKey) store.selectRange(id);
    else if (isMod(e)) store.toggle(id);
    else store.selectOnly(id);
  }

  function ondblclick(e: MouseEvent): void {
    const id = rowIdAt(e);
    if (id === undefined) return;
    store.selectOnly(id);
    prefs.detailOpen = true;
  }

  function oncontextmenu(e: MouseEvent): void {
    const id = rowIdAt(e);
    if (id === undefined) return;
    e.preventDefault();
    if (!store.selection.has(id)) store.selectOnly(id);
    else store.activeId = id;
    app.menu = { id, x: e.clientX, y: e.clientY };
  }

  function onfocus(): void {
    if (store.activeId === undefined && items.length > 0) {
      const first = items[Math.min(items.length - 1, Math.ceil(scrollTop / rowH))];
      if (first) store.activeId = first.id;
    }
  }

  const emptyKind = $derived(
    !store.loaded ? (store.loadError ? "error" : "loading") : items.length > 0 ? undefined : store.query.trim() ? "noMatch" : "empty",
  );
  const modKey = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘" : "Ctrl";
</script>

<div class="list">
  <div
    bind:this={scroller}
    class="scroller {layout}"
    role="grid"
    tabindex="0"
    aria-label={t("list.label")}
    aria-rowcount={items.length + (layout === "classic" ? 1 : 0)}
    aria-colcount={layout === "classic" ? COLUMNS.length : 1}
    aria-multiselectable="true"
    aria-activedescendant={activeRendered && store.activeId !== undefined ? rowDomId(store.activeId) : undefined}
    aria-busy={!store.loaded}
    style:--cols={template}
    onscroll={() => (scrollTop = scroller?.scrollTop ?? 0)}
    {onkeydown}
    {onclick}
    {ondblclick}
    {oncontextmenu}
    {onfocus}
  >
    <div class="table" style:min-inline-size={layout === "classic" ? `${minWidth}px` : undefined}>
      {#if layout === "classic"}
        <ClassicHeader />
      {/if}
      <div class="body" role="rowgroup" style:block-size="{items.length * rowH}px">
        <div class="window" style:transform="translateY({range.start * rowH}px)">
          {#each slice as download, i (download.id)}
            {@const index = range.start + i}
            {#if layout === "classic"}
              <ClassicRow
                {download}
                rowIndex={index + 2}
                domId={rowDomId(download.id)}
                selected={store.selection.has(download.id)}
                active={store.activeId === download.id}
              />
            {:else}
              <ModernCard
                {download}
                rowIndex={index + 1}
                domId={rowDomId(download.id)}
                selected={store.selection.has(download.id)}
                active={store.activeId === download.id}
              />
            {/if}
          {/each}
        </div>
      </div>
    </div>
  </div>

  {#if emptyKind}
    <div class="empty" style:inset-block-start="{headerH}px">
      {#if emptyKind === "loading"}
        <span class="vy-spinner" aria-hidden="true"></span>
        <p role="status">{t("list.loading")}</p>
      {:else if emptyKind === "error"}
        <Icon name="alertCircle" size={28} />
        <p role="alert">{t("list.loadError", { message: store.loadError ?? "" })}</p>
      {:else if emptyKind === "noMatch"}
        <Icon name="search" size={28} />
        <h2>{t("list.noMatch.title", { query: store.query.trim() })}</h2>
        <p>{t("list.noMatch.body")}</p>
        <button class="vy-btn vy-btn--secondary" onclick={() => (store.query = "")}>{t("list.noMatch.clear")}</button>
      {:else}
        <span class="art" aria-hidden="true"><Icon name="download" size={26} /></span>
        <h2>{t("list.empty.title")}</h2>
        <p>{t("list.empty.body", { shortcut: `${modKey}+N` })}</p>
        <button class="vy-btn vy-btn--primary" onclick={() => (app.addOpen = true)}>
          <Icon name="plus" size={15} />{t("list.empty.action")}
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .list {
    position: relative;
    flex: 1;
    min-inline-size: 0;
    min-block-size: 0;
    display: flex;
  }
  .scroller {
    flex: 1;
    overflow: auto;
    overscroll-behavior: contain;
    outline: none;
    contain: strict;
  }
  .table {
    min-block-size: 100%;
  }
  .body {
    position: relative;
  }
  .window {
    position: absolute;
    inset-inline: 0;
    inset-block-start: 0;
    will-change: transform;
  }
  .empty {
    position: absolute;
    inset-inline: 0;
    inset-block-end: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--vy-space-2);
    padding: var(--vy-space-8);
    text-align: center;
    color: var(--vy-color-text-muted);
    pointer-events: none;
  }
  .empty > * {
    pointer-events: auto;
  }
  .empty h2 {
    color: var(--vy-color-text);
    font-size: var(--vy-font-size-lg);
    font-weight: var(--vy-font-weight-semibold);
  }
  .empty p {
    max-inline-size: 42ch;
  }
  .empty .vy-btn {
    margin-block-start: var(--vy-space-2);
  }
  .art {
    display: grid;
    place-items: center;
    inline-size: 56px;
    block-size: 56px;
    margin-block-end: var(--vy-space-2);
    border-radius: var(--vy-radius-lg);
    color: var(--vy-color-accent-text);
    background: var(--vy-color-accent-subtle);
  }
</style>
