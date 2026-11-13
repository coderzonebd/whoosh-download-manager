<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Sortable, resizable column headers. One tab stop for the whole header row
  (roving tabindex): ←/→ move between columns, Enter/Space sorts,
  Alt+←/→ resizes the focused column, double-click a divider to reset.
-->
<script lang="ts">
  import { COLUMNS, HEADER_HEIGHT } from "../lib/columns.ts";
  import { getApp } from "../lib/state/app.svelte.ts";
  import { DEFAULT_WIDTHS, type ColumnId } from "../lib/state/prefs.svelte.ts";
  import Icon from "./Icon.svelte";

  const app = getApp();
  const { store, prefs, i18n } = app;
  const { t } = i18n;

  let focusIndex = $state(0);
  const buttons: HTMLButtonElement[] = $state([]);

  function sortBy(i: number): void {
    const col = COLUMNS[i];
    if (!col) return;
    const cur = store.sort;
    store.sort = cur.key === col.sort ? { key: col.sort, dir: cur.dir === "asc" ? "desc" : "asc" } : { key: col.sort, dir: col.firstDir };
  }

  function onkeydown(e: KeyboardEvent, i: number): void {
    const col = COLUMNS[i];
    if (!col) return;
    const rtl = i18n.dir === "rtl";
    const forward = rtl ? "ArrowLeft" : "ArrowRight";
    const back = rtl ? "ArrowRight" : "ArrowLeft";
    if (e.altKey && (e.key === forward || e.key === back)) {
      e.preventDefault();
      prefs.setWidth(col.id, prefs.widths[col.id] + (e.key === forward ? 16 : -16));
      return;
    }
    let next = i;
    if (e.key === forward) next = Math.min(COLUMNS.length - 1, i + 1);
    else if (e.key === back) next = Math.max(0, i - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = COLUMNS.length - 1;
    else return;
    e.preventDefault();
    focusIndex = next;
    buttons[next]?.focus();
  }

  function startResize(e: PointerEvent, id: ColumnId): void {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const startX = e.clientX;
    const startW = prefs.widths[id];
    const sign = i18n.dir === "rtl" ? -1 : 1;
    const move = (ev: PointerEvent) => prefs.setWidth(id, startW + (ev.clientX - startX) * sign);
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }
</script>

<div class="head" role="rowgroup" style:block-size="{HEADER_HEIGHT}px">
  <div class="hrow" role="row" aria-rowindex={1}>
    {#each COLUMNS as col, i (col.id)}
      {@const sorted = store.sort.key === col.sort}
      <div
        class="th"
        class:numeric={col.numeric}
        role="columnheader"
        aria-colindex={i + 1}
        aria-sort={sorted ? (store.sort.dir === "asc" ? "ascending" : "descending") : "none"}
      >
        <button
          bind:this={buttons[i]}
          class="sort"
          class:sorted
          tabindex={focusIndex === i ? 0 : -1}
          onclick={() => sortBy(i)}
          onfocus={() => (focusIndex = i)}
          onkeydown={(e) => onkeydown(e, i)}
        >
          <span class="vy-truncate">{t(`col.${col.id}`)}</span>
          {#if sorted}
            <span class="arrow" class:asc={store.sort.dir === "asc"}><Icon name="chevronDown" size={12} /></span>
          {/if}
        </button>
        <span
          class="resizer"
          aria-hidden="true"
          title={t("col.resize", { column: t(`col.${col.id}`) })}
          onpointerdown={(e) => startResize(e, col.id)}
          ondblclick={() => prefs.setWidth(col.id, DEFAULT_WIDTHS[col.id])}
        ></span>
      </div>
    {/each}
  </div>
</div>

<style>
  .head {
    position: sticky;
    inset-block-start: 0;
    z-index: var(--vy-z-sticky);
    background: var(--vy-color-surface);
    border-block-end: 1px solid var(--vy-color-border);
  }
  .hrow {
    display: grid;
    grid-template-columns: var(--cols);
    block-size: 100%;
  }
  .th {
    position: relative;
    display: flex;
    align-items: stretch;
    min-inline-size: 0;
  }
  .sort {
    display: flex;
    align-items: center;
    gap: var(--vy-space-1);
    inline-size: 100%;
    min-inline-size: 0;
    padding-block: 0;
    padding-inline: var(--vy-space-3);
    border: 0;
    background: transparent;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
    font-weight: var(--vy-font-weight-medium);
    text-align: start;
    cursor: pointer;
  }
  .numeric .sort {
    justify-content: flex-end;
  }
  .sort.sorted {
    color: var(--vy-color-text);
  }
  .sort:focus-visible {
    outline-offset: -2px;
  }
  @media (hover: hover) and (pointer: fine) {
    .sort:hover {
      color: var(--vy-color-text);
      background: var(--vy-color-surface-hover);
    }
  }
  .arrow {
    display: inline-flex;
    color: var(--vy-color-accent-text);
  }
  .arrow.asc {
    transform: rotate(180deg);
  }
  .resizer {
    position: absolute;
    inset-block: 6px;
    inset-inline-end: -4px;
    inline-size: 9px;
    z-index: 1;
    cursor: col-resize;
    touch-action: none;
  }
  .resizer::after {
    content: "";
    position: absolute;
    inset-block: 0;
    inset-inline-start: 4px;
    inline-size: 1px;
    background: var(--vy-color-border);
    transition: background-color var(--vy-duration-fast) var(--vy-ease-standard);
  }
  .resizer:hover::after,
  .resizer:active::after {
    background: var(--vy-color-accent);
    inline-size: 2px;
  }
  .th:last-child .resizer {
    inset-inline-end: 0;
  }
</style>
