<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Told once: crash reports are on by default (builds that can send them), so
  the first run says so, links to the switch in Settings, and can be
  dismissed. It never blocks anything; it is read once as a polite status.
-->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import Icon from "./Icon.svelte";

  const app = getApp();
  const { t } = app.i18n;
</script>

{#if app.crashNotice}
  <div class="notice" role="status">
    <Icon name="alert" size={17} />
    <p class="text">{t("crashNotice.text")}</p>
    <button class="vy-btn vy-btn--secondary vy-btn--sm" onclick={() => app.openCrashSetting()}>{t("crashNotice.open")}</button>
    <button class="vy-btn vy-btn--icon vy-btn--sm" aria-label={t("action.dismiss")} title={t("action.dismiss")} onclick={() => app.dismissCrashNotice()}>
      <Icon name="x" size={14} />
    </button>
  </div>
{/if}

<style>
  .notice {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    padding-block: var(--vy-space-2);
    padding-inline: var(--vy-space-4);
    background: var(--vy-color-info-subtle);
    color: var(--vy-color-info-text);
    border-block-end: 1px solid var(--vy-color-border);
  }
  .text {
    flex: 1;
    min-inline-size: 0;
    color: var(--vy-color-text);
  }
  @media (forced-colors: active) {
    .notice {
      border: 2px solid CanvasText;
    }
  }
</style>
