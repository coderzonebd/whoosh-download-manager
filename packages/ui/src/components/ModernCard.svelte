<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import { PAUSABLE, RESUMABLE, type Download } from "../lib/state/downloads.svelte.ts";
  import FileIcon from "./FileIcon.svelte";
  import Icon from "./Icon.svelte";
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

  const app = getApp();
  const { i18n } = app;
  const { t } = i18n;
  const d = $derived(download.data);

  const meta = $derived.by(() => {
    const parts: string[] = [];
    const size = d.totalBytes;
    switch (d.state) {
      case "downloading":
        parts.push(size === undefined ? t("progress.indeterminate", { done: i18n.bytes(d.downloadedBytes) }) : t("progress.of", { done: i18n.bytes(d.downloadedBytes), total: i18n.bytes(size) }));
        parts.push(i18n.speed(d.speedBps));
        if (d.etaSecs !== undefined) parts.push(t("eta.left", { eta: i18n.eta(d.etaSecs) }));
        parts.push(t("connections.count", { count: d.connections }));
        break;
      case "completed":
        parts.push(i18n.bytes(size ?? d.downloadedBytes), i18n.date(d.completedAt ?? d.addedAt));
        break;
      case "failed":
      case "needs_refresh":
        if (d.error) parts.push(d.error.message);
        break;
      default:
        parts.push(size === undefined ? i18n.bytes(d.downloadedBytes) : t("progress.of", { done: i18n.bytes(d.downloadedBytes), total: i18n.bytes(size) }));
    }
    return parts.join("  ·  ");
  });

  const primary = $derived.by(() => {
    if (PAUSABLE.has(d.state)) return { icon: "pause", label: t("action.pause"), run: () => app.pause([download.id]) } as const;
    if (d.state === "needs_refresh") return { icon: "refresh", label: t("action.refreshLink"), run: () => app.refreshLink(download.id) } as const;
    if (RESUMABLE.has(d.state)) return { icon: d.state === "failed" ? "refresh" : "play", label: t(d.state === "failed" ? "action.retry" : "action.resume"), run: () => app.resume([download.id]) } as const;
    return undefined;
  });

  function openMenu(e: MouseEvent): void {
    e.stopPropagation();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    if (!app.store.selection.has(download.id)) app.store.selectOnly(download.id);
    app.menu = { id: download.id, x: r.left, y: r.bottom + 4 };
  }
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
  <div class="card" role="gridcell">
    <span class="vy-pick" data-pick role="checkbox" aria-checked={selected} aria-label={i18n.t("list.pick")}></span>
    <FileIcon category={download.category} size="lg" />
    <div class="main">
      <div class="title">
        <span class="name vy-truncate" title={d.filename} dir="auto">{d.filename}</span>
        <StatusBadge state={d.state} variant="pill" step={d.media?.step} />
      </div>
      <SegmentBar download={download} size="md" />
      <div class="meta vy-num" class:problem={d.state === "failed" || d.state === "needs_refresh"}>
        <span class="vy-truncate">{meta}</span>
      </div>
    </div>
    <div class="actions">
      {#if primary}
        <button
          class="vy-btn vy-btn--icon vy-btn--sm"
          tabindex="-1"
          onmousedown={(e) => e.preventDefault()}
          aria-label={primary.label}
          title={primary.label}
          onclick={(e) => {
            e.stopPropagation();
            primary.run();
          }}
        >
          <Icon name={primary.icon} size={15} />
        </button>
      {/if}
      <button class="vy-btn vy-btn--icon vy-btn--sm" tabindex="-1" onmousedown={(e) => e.preventDefault()} aria-label={t("menu.label", { name: d.filename })} title={t("menu.label", { name: d.filename })} onclick={openMenu}>
        <Icon name="more" size={15} />
      </button>
    </div>
  </div>
</div>

<style>
  .row {
    block-size: 76px;
    padding-block: 3px;
    padding-inline: var(--vy-space-3);
  }
  .card {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    block-size: 100%;
    padding-inline: var(--vy-space-3);
    border-radius: var(--vy-radius-md);
    border: 1px solid transparent;
    transition: background-color var(--vy-duration-fast) var(--vy-ease-standard);
  }
  @media (hover: hover) and (pointer: fine) {
    .row:hover .card {
      background: var(--vy-color-surface-hover);
    }
    .row:hover .actions {
      opacity: 1;
    }
  }
  .selected .card {
    background: var(--vy-color-selection);
  }
  :global(.scroller:focus-within) .selected .card {
    background: var(--vy-color-selection-strong);
  }
  :global(.scroller:focus-visible) .active .card {
    border-color: var(--vy-color-focus);
    box-shadow: 0 0 0 1px var(--vy-color-focus);
  }
  .main {
    display: grid;
    gap: 6px;
    flex: 1;
    min-inline-size: 0;
  }
  .title {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--vy-space-3);
    min-inline-size: 0;
  }
  .title > :global(.badge) {
    /* the file name truncates, the status never does */
    flex: none;
  }
  .name {
    font-weight: var(--vy-font-weight-semibold);
    font-size: var(--vy-font-size-md);
  }
  .meta {
    display: flex;
    min-inline-size: 0;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .meta.problem {
    color: var(--vy-color-danger-text);
  }
  [data-state="needs_refresh"] .meta.problem {
    color: var(--vy-color-warning-text);
  }
  .actions {
    display: flex;
    gap: var(--vy-space-1);
    opacity: 0.6;
    transition: opacity var(--vy-duration-fast) var(--vy-ease-standard);
  }
  .selected .actions,
  .active .actions {
    opacity: 1;
  }
</style>
