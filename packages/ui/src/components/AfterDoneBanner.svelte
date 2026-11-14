<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  A queue finished and its after-done action (sleep, shut down) runs in a
  minute unless someone cancels (`event.queue.finished`). The sentence is
  announced once; the ticking seconds are visual only, so a screen reader
  isn't interrupted every second.
-->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import type { PendingAction } from "../lib/state/queues.svelte.ts";
  import Icon from "./Icon.svelte";

  const app = getApp();
  const { i18n, queues } = app;
  const { t } = i18n;

  let now = $state(Date.now());

  $effect(() => {
    if (queues.pending.length === 0) return;
    const timer = setInterval(() => {
      now = Date.now();
      queues.expire(now);
    }, 500);
    return () => clearInterval(timer);
  });

  function left(p: PendingAction): number {
    return Math.max(0, Math.ceil((p.atMs - now) / 1000));
  }

  function clock(secs: number): string {
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return `${i18n.number(m)}:${i18n.number(Math.floor(s / 10))}${i18n.number(s % 10)}`;
  }

  function message(p: PendingAction): string {
    const name = app.queueLabel(p.name);
    return t(p.action === "sleep" ? "afterDone.sleepSoon" : "afterDone.shutdownSoon", { name, secs: i18n.number(Math.max(0, Math.round((p.atMs - Date.now()) / 1000))) });
  }
</script>

{#each queues.pending as p (p.name)}
  {@const secs = left(p)}
  <div class="banner" role="alert">
    <Icon name="power" size={18} />
    <p class="text">{message(p)}</p>
    <span class="count vy-num" aria-hidden="true">{secs > 0 ? clock(secs) : t("afterDone.now")}</span>
    <button class="vy-btn vy-btn--secondary" onclick={() => app.cancelAction(p.name)}>
      {t("afterDone.cancel")}
    </button>
  </div>
{/each}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    padding-block: var(--vy-space-2);
    padding-inline: var(--vy-space-4);
    background: var(--vy-color-warning-subtle);
    color: var(--vy-color-warning-text);
    border-block-end: 1px solid var(--vy-color-border);
  }
  .text {
    flex: 1;
    min-inline-size: 0;
    color: var(--vy-color-text);
  }
  .count {
    font-size: var(--vy-font-size-lg);
    font-weight: var(--vy-font-weight-semibold);
  }
  @media (forced-colors: active) {
    .banner {
      border: 2px solid CanvasText;
    }
  }
</style>
