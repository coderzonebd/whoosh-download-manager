<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import type { Download } from "../lib/state/downloads.svelte.ts";
  import FileIcon from "./FileIcon.svelte";
  import SegmentBar from "./SegmentBar.svelte";
  import StatusBadge from "./StatusBadge.svelte";

  interface Props {
    download: Download;
    rowIndex: number;
    domId: string;
    selected: boolean;
    active: boolean;
  }
  let { download, rowIndex, domId, selected, active }: Props = $props();

  const { i18n } = getApp();
  const d = $derived(download.data);
  const moving = $derived(d.state === "downloading");
  const frac = $derived(download.fraction);
</script>

<div
  class="row"
  class:selected
  class:active
  id={domId}
  role="row"
  aria-rowindex={rowIndex}
  aria-selected={selected}
  data-row-id={download.id}
  data-state={d.state}
>
  <div class="cell name" role="gridcell">
    <FileIcon category={download.category} size="sm" />
    <span class="vy-truncate" title={d.filename} dir="auto">{d.filename}</span>
  </div>
  <div class="cell num vy-num" role="gridcell">{d.totalBytes === undefined ? "—" : i18n.bytes(d.totalBytes)}</div>
  <div class="cell progress" role="gridcell">
    <SegmentBar download={download} decorative />
    <span class="pct vy-num">{frac === undefined ? i18n.bytes(d.downloadedBytes) : i18n.percent(frac)}</span>
  </div>
  <div class="cell" role="gridcell"><StatusBadge state={d.state} /></div>
  <div class="cell num vy-num" role="gridcell">{moving ? i18n.speed(d.speedBps) : ""}</div>
  <div class="cell num vy-num" role="gridcell">{moving ? i18n.eta(d.etaSecs) : ""}</div>
  <div class="cell num vy-num" role="gridcell">{moving ? i18n.number(d.connections) : ""}</div>
  <div class="cell muted vy-num" role="gridcell"><span class="vy-truncate" title={i18n.date(d.addedAt)}>{i18n.shortDate(d.addedAt)}</span></div>
</div>

<style>
  .row {
    display: grid;
    grid-template-columns: var(--cols);
    block-size: 30px;
    border-block-end: 1px solid color-mix(in srgb, var(--vy-color-border) 55%, transparent);
    cursor: default;
  }
  @media (hover: hover) and (pointer: fine) {
    .row:hover {
      background: var(--vy-color-surface-hover);
    }
  }
  .row.selected {
    background: var(--vy-color-selection);
  }
  :global(.scroller:focus-within) .row.selected {
    background: var(--vy-color-selection-strong);
  }
  :global(.scroller:focus-visible) .row.active {
    box-shadow: inset 0 0 0 2px var(--vy-color-focus);
  }
  .cell {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2);
    min-inline-size: 0;
    padding-inline: var(--vy-space-3);
    white-space: nowrap;
    overflow: hidden;
  }
  .num {
    justify-content: flex-end;
  }
  .muted {
    color: var(--vy-color-text-muted);
  }
  .name {
    font-weight: var(--vy-font-weight-medium);
  }
  .progress :global(.bar) {
    flex: 1;
  }
  .pct {
    flex: none;
    min-inline-size: 3.6em;
    text-align: end;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  [data-state="failed"] .name,
  [data-state="needs_refresh"] .name {
    color: var(--vy-color-text);
  }
</style>
