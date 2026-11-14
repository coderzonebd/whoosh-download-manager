<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Progress bar that shows each connection's range. Segments alternate two
  shades so neighbours are distinguishable; bytes gained between segment
  polls are distributed by `estimateSegments` so the bar glides at 4 Hz.
-->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import type { Download } from "../lib/state/downloads.svelte.ts";
  import { estimateSegments, type SegmentFill } from "../lib/segments.ts";
  import { toneFor } from "../lib/tone.ts";

  interface Props {
    download: Download;
    size?: "sm" | "md" | "lg";
    /** Hide from assistive tech when the same numbers are already in adjacent text. */
    decorative?: boolean;
  }
  let { download, size = "sm", decorative = false }: Props = $props();

  const app = getApp();
  const { i18n } = app;

  const d = $derived(download.data);
  const snapshot = $derived(app.segments.get(download.id));
  const total = $derived(d.totalBytes);
  const frac = $derived(download.fraction);
  const indeterminate = $derived(total === undefined && d.state === "downloading");

  const fills: SegmentFill[] | undefined = $derived.by(() => {
    if (total === undefined || total <= 0 || d.state === "completed" || !snapshot) return undefined;
    const segs = snapshot.detail.segments;
    if (segs.length < 2) return undefined;
    return estimateSegments(segs, d.downloadedBytes);
  });

  const pct = $derived(frac === undefined ? undefined : Math.floor(frac * 100));
</script>

<div
  class="bar {size} tone-{toneFor(d.state)}"
  class:indeterminate
  role={decorative ? undefined : "progressbar"}
  aria-hidden={decorative ? "true" : undefined}
  aria-valuemin={decorative ? undefined : 0}
  aria-valuemax={decorative ? undefined : 100}
  aria-valuenow={decorative ? undefined : pct}
  aria-valuetext={decorative ? undefined : i18n.progress(d.downloadedBytes, total)}
  data-segments={fills?.length ?? 0}
>
  {#if fills && total}
    {#each fills as f, i (f.start)}
      <span
        class="seg"
        class:alt={i % 2 === 1}
        style:inset-inline-start="{(f.start / total) * 100}%"
        style:inline-size="{((f.end - f.start) / total) * 100}%"
      >
        <span class="fill" style:transform="scaleX({f.end > f.start ? f.done / (f.end - f.start) : 0})"></span>
      </span>
    {/each}
  {:else if !indeterminate}
    <span class="seg whole">
      <span class="fill" style:transform="scaleX({frac ?? 0})"></span>
    </span>
  {/if}
</div>

<style>
  .bar {
    position: relative;
    overflow: hidden;
    inline-size: 100%;
    background: var(--vy-color-progress-track);
    border-radius: var(--vy-radius-full);
    isolation: isolate;
  }
  .sm {
    block-size: 6px;
  }
  .md {
    block-size: 8px;
  }
  .lg {
    block-size: 12px;
  }

  .seg {
    position: absolute;
    inset-block: 0;
    overflow: hidden;
  }
  .seg + .seg {
    /* hairline between neighbouring connections */
    border-inline-start: 1px solid var(--vy-color-surface);
  }
  .whole {
    inset-inline: 0;
  }
  .fill {
    display: block;
    block-size: 100%;
    background: var(--fill);
    transform-origin: left center;
    transition: transform var(--vy-duration-progress) linear;
    will-change: transform;
  }
  :global([dir="rtl"]) .fill {
    transform-origin: right center;
  }
  .alt .fill {
    background: var(--fill-alt);
  }

  .tone-accent {
    --fill: var(--vy-color-accent);
    --fill-alt: var(--vy-color-segment-alt);
  }
  .tone-success {
    --fill: var(--vy-color-success);
    --fill-alt: var(--vy-color-success);
  }
  .tone-warning {
    --fill: var(--vy-color-warning);
    --fill-alt: var(--vy-color-warning);
  }
  .tone-danger {
    --fill: var(--vy-color-danger);
    --fill-alt: var(--vy-color-danger);
  }
  .tone-info {
    --fill: var(--vy-color-info);
    --fill-alt: var(--vy-color-info);
  }
  .tone-neutral {
    --fill: var(--vy-color-neutral);
    --fill-alt: color-mix(in srgb, var(--vy-color-neutral) 75%, var(--vy-color-surface));
  }

  /* Unknown size: a calm moving sheen instead of a fake percentage. */
  .indeterminate::after {
    content: "";
    position: absolute;
    inset-block: 0;
    inset-inline-start: 0;
    inline-size: 35%;
    border-radius: inherit;
    background: var(--fill);
    animation: vy-indeterminate 1.6s var(--vy-ease-standard) infinite;
  }
  @keyframes vy-indeterminate {
    from {
      translate: -100% 0;
    }
    to {
      translate: 290% 0;
    }
  }
  :global([dir="rtl"]) .indeterminate::after {
    animation-name: vy-indeterminate-rtl;
  }
  @keyframes vy-indeterminate-rtl {
    from {
      translate: 100% 0;
    }
    to {
      translate: -290% 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .indeterminate::after {
      animation: none;
      inline-size: 100%;
      opacity: 0.45;
    }
  }

  @media (forced-colors: active) {
    .bar {
      border: 1px solid CanvasText;
    }
    .fill,
    .indeterminate::after {
      forced-color-adjust: none;
      background: Highlight;
    }
  }
</style>
