<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import type { DownloadState } from "@vayu/api-client";
  import { getApp } from "../lib/state/app.svelte.ts";
  import { toneFor } from "../lib/tone.ts";

  interface Props {
    state: DownloadState;
    /** "dot" for dense tables, "pill" for cards and the detail panel. */
    variant?: "dot" | "pill";
  }
  let { state, variant = "dot" }: Props = $props();
  const { t } = getApp().i18n;
</script>

<span class="badge {variant} tone-{toneFor(state)}" data-state={state}>
  <span class="dot" aria-hidden="true"></span>
  <span class="vy-truncate">{t(`state.${state}`)}</span>
</span>

<style>
  .badge {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1-5);
    min-inline-size: 0;
    max-inline-size: 100%;
    color: var(--fg);
    font-size: var(--vy-font-size-sm);
    font-weight: var(--vy-font-weight-medium);
  }
  .dot {
    flex: none;
    inline-size: 7px;
    block-size: 7px;
    border-radius: 50%;
    background: var(--fill);
  }
  .pill {
    padding-block: 1px;
    padding-inline: var(--vy-space-2);
    border-radius: var(--vy-radius-full);
    background: var(--bg);
  }
  .tone-accent {
    --fill: var(--vy-color-accent);
    --fg: var(--vy-color-accent-text);
    --bg: var(--vy-color-accent-subtle);
  }
  .tone-success {
    --fill: var(--vy-color-success);
    --fg: var(--vy-color-success-text);
    --bg: var(--vy-color-success-subtle);
  }
  .tone-warning {
    --fill: var(--vy-color-warning);
    --fg: var(--vy-color-warning-text);
    --bg: var(--vy-color-warning-subtle);
  }
  .tone-danger {
    --fill: var(--vy-color-danger);
    --fg: var(--vy-color-danger-text);
    --bg: var(--vy-color-danger-subtle);
  }
  .tone-info {
    --fill: var(--vy-color-info);
    --fg: var(--vy-color-info-text);
    --bg: var(--vy-color-info-subtle);
  }
  .tone-neutral {
    --fill: var(--vy-color-neutral);
    --fg: var(--vy-color-text-muted);
    --bg: var(--vy-color-surface-active);
  }
</style>
