<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Above the list while a queue is shown: its name, whether it runs, its
  schedule in words, and what you can do with it.
-->
<script lang="ts">
  import { MAIN_QUEUE } from "@whoosh/api-client";
  import { scheduleSummary } from "../lib/schedule.ts";
  import { getApp } from "../lib/state/app.svelte.ts";
  import Icon from "./Icon.svelte";

  interface Props {
    name: string;
  }
  let { name }: Props = $props();

  const app = getApp();
  const { i18n, queues } = app;
  const { t } = i18n;

  const queue = $derived(queues.get(name));
  const isMain = $derived(name === MAIN_QUEUE);
  const count = $derived(app.store.counts.queues[name] ?? 0);
  const summary = $derived(isMain ? t("queue.mainSummary", { count: queue?.maxActive ?? app.settings?.maxActiveDownloads ?? 1 }) : scheduleSummary(i18n, queue?.schedule));
</script>

{#if queue}
  <section class="bar" aria-labelledby="vy-queue-title">
    <span class="icon" aria-hidden="true"><Icon name="layers" size={18} /></span>
    <div class="text">
      <h1 id="vy-queue-title" class="vy-truncate" dir="auto">{app.queueLabel(name)}</h1>
      <p class="meta">
        {#if !isMain}
          <span class="pill" class:stopped={!queue.running}>{queue.running ? t("queue.running") : t("queue.stopped")}</span>
        {/if}
        <span class="vy-num">{t("queue.count", { count })}</span>
        <span class="summary">
          {#if !isMain && queue.schedule}<Icon name="calendar" size={13} />{/if}
          {summary}
        </span>
      </p>
    </div>
    <div class="actions">
      {#if !isMain}
        {#if queue.running}
          <button class="vy-btn vy-btn--secondary" onclick={() => app.stopQueue(name)}>
            <Icon name="stop" size={14} /><span class="label">{t("queue.stop")}</span>
          </button>
        {:else}
          <button class="vy-btn vy-btn--secondary" onclick={() => app.startQueue(name)}>
            <Icon name="play" size={14} /><span class="label">{t("queue.start")}</span>
          </button>
        {/if}
      {/if}
      <button class="vy-btn vy-btn--secondary" onclick={() => (app.queueDialog = { mode: "edit", name })}>
        <Icon name={isMain ? "settings" : "calendar"} size={14} /><span class="label">{isMain ? t("queue.edit") : t("queue.editSchedule")}</span>
      </button>
      {#if !isMain}
        <button class="vy-btn vy-btn--icon" aria-label={t("queue.removeNamed", { name })} title={t("queue.removeNamed", { name })} onclick={() => (app.removeQueueName = name)}>
          <Icon name="trash" size={15} />
        </button>
      {/if}
    </div>
  </section>
{/if}

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    padding-block: var(--vy-space-2-5);
    padding-inline: var(--vy-space-4);
    border-block-end: 1px solid var(--vy-color-border);
    background: var(--vy-color-surface);
  }
  .icon {
    display: grid;
    place-items: center;
    inline-size: 32px;
    block-size: 32px;
    flex: none;
    border-radius: var(--vy-radius-md);
    background: var(--vy-color-accent-subtle);
    color: var(--vy-color-accent-text);
  }
  .text {
    flex: 1;
    min-inline-size: 0;
  }
  h1 {
    font-size: var(--vy-font-size-md);
    font-weight: var(--vy-font-weight-semibold);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--vy-space-1) var(--vy-space-3);
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .summary {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1);
  }
  .pill {
    padding-inline: var(--vy-space-2);
    border-radius: var(--vy-radius-full);
    background: var(--vy-color-success-subtle);
    color: var(--vy-color-success-text);
    font-size: var(--vy-font-size-xs);
    font-weight: var(--vy-font-weight-medium);
  }
  .pill.stopped {
    background: var(--vy-color-warning-subtle);
    color: var(--vy-color-warning-text);
  }
  .actions {
    display: flex;
    flex: none;
    gap: var(--vy-space-2);
  }
  @container vy-app (max-width: 760px) {
    .actions .label {
      position: absolute;
      inline-size: 1px;
      block-size: 1px;
      overflow: hidden;
      clip-path: inset(50%);
      white-space: nowrap;
    }
  }
</style>
