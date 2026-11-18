<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  One download in the phone list. The whole card opens its details; the round
  button at the end is the one thing you most likely want next (pause, resume,
  retry, refresh the link). Two separate buttons, never one inside the other.
-->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import { PAUSABLE, RESUMABLE, type Download } from "../lib/state/downloads.svelte.ts";
  import FileIcon from "../components/FileIcon.svelte";
  import Icon from "../components/Icon.svelte";
  import SegmentBar from "../components/SegmentBar.svelte";

  interface Props {
    download: Download;
    onopen: (id: string) => void;
  }
  let { download, onopen }: Props = $props();

  const app = getApp();
  const { i18n } = app;
  const { t } = i18n;
  const d = $derived(download.data);
  const done = $derived(d.state === "completed");
  const problem = $derived(d.state === "failed" || d.state === "needs_refresh");

  /**
   * The line under the name: what you would ask about this download first. `lead` may shorten
   * with an ellipsis; `trail` (speed and time left, or the date) always stays whole.
   */
  const meta = $derived.by((): { lead: string; trail?: string } => {
    const size = d.totalBytes;
    const amount = size === undefined ? i18n.bytes(d.downloadedBytes) : t("progress.of", { done: i18n.bytes(d.downloadedBytes), total: i18n.bytes(size) });
    switch (d.state) {
      case "downloading":
        if (d.connections === 0) return { lead: amount, trail: t("mobile.waiting") };
        return { lead: amount, trail: d.etaSecs === undefined ? i18n.speed(d.speedBps) : `${i18n.speed(d.speedBps)} · ${i18n.eta(d.etaSecs)}` };
      case "completed":
        return { lead: i18n.bytes(size ?? d.downloadedBytes), trail: i18n.date(d.completedAt ?? d.addedAt) };
      case "failed":
      case "needs_refresh":
        return { lead: d.error?.message ?? t(`state.${d.state}`) };
      default:
        return { lead: t(`state.${d.state}`), trail: amount };
    }
  });

  const primary = $derived.by(() => {
    if (PAUSABLE.has(d.state)) return { icon: "pause", label: t("action.pause"), run: () => app.pause([download.id]) } as const;
    if (d.state === "needs_refresh") return { icon: "refresh", label: t("action.refreshLink"), run: () => app.refreshLink(download.id) } as const;
    if (RESUMABLE.has(d.state))
      return { icon: d.state === "failed" ? "refresh" : "play", label: t(d.state === "failed" ? "action.retry" : "action.resume"), run: () => app.resume([download.id]) } as const;
    return undefined;
  });
</script>

<article class="card" class:done class:problem data-state={d.state}>
  <button type="button" class="open" onclick={() => onopen(download.id)}>
    <span class="icon">
      <FileIcon category={download.category} size="md" />
      {#if done}<span class="badge ok"><Icon name="check" size={11} /></span>{/if}
      {#if problem}<span class="badge bad"><Icon name="alert" size={11} /></span>{/if}
    </span>
    <span class="body">
      <span class="name" dir="auto">{d.filename}</span>
      {#if !done}
        <span class="bar"><SegmentBar {download} size="sm" decorative /></span>
      {/if}
      <span class="meta vy-num" class:danger={problem}>
        <span class="lead">{meta.lead}</span>
        {#if meta.trail}<span class="trail" class:waiting={d.state === "downloading" && d.connections === 0}>{meta.trail}</span>{/if}
      </span>
    </span>
  </button>
  {#if primary}
    <button type="button" class="act" class:pause={primary.icon === "pause"} aria-label={`${primary.label}: ${d.filename}`} onclick={primary.run}>
      <Icon name={primary.icon} size={18} />
    </button>
  {/if}
</article>

<style>
  .card {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--vy-space-1);
    padding-inline-end: var(--vy-space-2);
    border-radius: var(--vy-radius-lg);
    background: var(--vy-color-surface-raised);
    box-shadow: 0 0 0 1px var(--vy-color-border);
    /* Long lists stay cheap: off-screen cards are skipped until scrolled to. */
    content-visibility: auto;
    contain-intrinsic-size: auto 76px;
  }
  .open {
    flex: 1;
    min-inline-size: 0;
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    padding: var(--vy-space-3);
    padding-inline-end: var(--vy-space-1);
    border: 0;
    border-radius: inherit;
    background: none;
    color: inherit;
    font: inherit;
    text-align: start;
    -webkit-tap-highlight-color: transparent;
    transition: scale 120ms var(--vy-ease-out);
  }
  /* Feedback on the press itself, not on release. */
  .open:active {
    scale: 0.985;
  }
  .icon {
    position: relative;
    flex: none;
  }
  .badge {
    position: absolute;
    inset-block-end: -3px;
    inset-inline-end: -3px;
    display: grid;
    place-items: center;
    inline-size: 16px;
    block-size: 16px;
    border-radius: var(--vy-radius-full);
    box-shadow: 0 0 0 2px var(--vy-color-surface-raised);
    color: var(--vy-color-text-on-accent);
  }
  .badge.ok {
    background: var(--vy-color-success);
  }
  .badge.bad {
    background: var(--vy-color-danger);
  }
  .body {
    flex: 1;
    min-inline-size: 0;
    display: grid;
    gap: var(--vy-space-1-5);
  }
  .name {
    font-weight: var(--vy-font-weight-semibold);
    line-height: var(--vy-line-height-tight);
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  /* Side by side when they fit; else the trail moves under the lead, so no number is ever cut. */
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    column-gap: var(--vy-space-3);
    min-inline-size: 0;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .lead {
    min-inline-size: 0;
    max-inline-size: 100%;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .trail {
    flex: none;
    margin-inline-start: auto;
    white-space: nowrap;
  }
  .trail.waiting {
    color: var(--vy-color-warning-text);
  }
  /* An error message is prose, not a number: it wraps, two lines at most. */
  .meta.danger .lead {
    white-space: normal;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    color: var(--vy-color-danger-text);
  }
  .act {
    flex: none;
    display: grid;
    place-items: center;
    inline-size: 44px;
    block-size: 44px;
    border: 0;
    border-radius: var(--vy-radius-full);
    background: var(--vy-color-accent-subtle);
    color: var(--vy-color-accent-text);
    -webkit-tap-highlight-color: transparent;
    transition:
      scale 120ms var(--vy-ease-out),
      background-color var(--vy-duration-fast) var(--vy-ease-out);
  }
  .act.pause {
    background: var(--vy-color-surface-hover);
    color: var(--vy-color-text);
  }
  .act:active {
    scale: 0.9;
  }
  @media (prefers-reduced-motion: reduce) {
    .open:active,
    .act:active {
      scale: none;
    }
  }
</style>
