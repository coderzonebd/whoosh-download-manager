<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import Icon from "./Icon.svelte";

  const app = getApp();
  const { i18n } = app;
  const { t } = i18n;

  const stats = $derived(app.stats);
  const limit = $derived(app.settings?.speedLimitBps);
  const selectedCount = $derived(app.store.selection.size);
</script>

<footer class="statusbar" aria-label={t("status.label")}>
  <span class="speed" title={t("status.speed")}>
    <Icon name="download" size={13} />
    <span class="vy-sr-only">{t("status.speed")}:</span>
    <span class="vy-num value">{i18n.speed(stats.downloadBps)}</span>
  </span>
  <span class="sep" aria-hidden="true"></span>
  <span class="vy-num">{t("status.active", { count: stats.active })}</span>
  <span class="vy-num">{t("status.queued", { count: stats.queued })}</span>
  <span class="sep" aria-hidden="true"></span>
  <span class="vy-num limit">
    <Icon name="gauge" size={13} />
    {limit === undefined ? t("status.noLimit") : t("status.limit", { speed: i18n.speed(limit) })}
  </span>
  {#if selectedCount > 1}
    <span class="sep" aria-hidden="true"></span>
    <span class="vy-num">{t("list.selected", { count: selectedCount })}</span>
  {/if}
  <span class="spacer"></span>
  <span class="conn conn-{app.connection}">
    <span class="dot" aria-hidden="true"></span>
    {t(`conn.${app.connection}`)}
  </span>
</footer>

<style>
  .statusbar {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    block-size: var(--vy-size-statusbar);
    padding-inline: var(--vy-space-4);
    border-block-start: 1px solid var(--vy-color-border);
    background: var(--vy-color-bg-sunken);
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
    white-space: nowrap;
    overflow: hidden;
  }
  .speed {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1);
    color: var(--vy-color-text);
  }
  .speed :global(.icon) {
    color: var(--vy-color-accent-text);
  }
  .value {
    font-weight: var(--vy-font-weight-semibold);
    min-inline-size: 7.5em;
  }
  .limit {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1);
  }
  .sep {
    inline-size: 1px;
    block-size: 12px;
    background: var(--vy-color-border);
  }
  .spacer {
    flex: 1;
  }
  .conn {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1-5);
  }
  .dot {
    inline-size: 7px;
    block-size: 7px;
    border-radius: 50%;
    background: var(--vy-color-neutral);
  }
  .conn-open .dot {
    background: var(--vy-color-success);
  }
  .conn-demo .dot {
    background: var(--vy-color-info);
  }
  .conn-reconnecting .dot,
  .conn-connecting .dot {
    background: var(--vy-color-warning);
  }
  .conn-closed .dot {
    background: var(--vy-color-danger);
  }
  @container vy-app (max-width: 640px) {
    .limit,
    .sep {
      display: none;
    }
  }
</style>
