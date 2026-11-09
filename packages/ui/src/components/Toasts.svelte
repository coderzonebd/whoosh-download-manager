<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Toasts stack at the block-end/inline-end corner. Announcements go through
  two persistent live regions (polite / assertive): screen readers announce
  changes to existing live regions far more reliably than new ones.
-->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import type { ToastTone } from "../lib/state/toasts.svelte.ts";
  import Icon from "./Icon.svelte";
  import type { IconName } from "./icons.ts";

  const app = getApp();
  const { toasts } = app;
  const { t } = app.i18n;

  const ICON: Record<ToastTone, IconName> = { info: "checkCircle", success: "checkCircle", warning: "alert", error: "alertCircle" };

  let polite = $state("");
  let assertive = $state("");
  let lastId = 0;

  $effect(() => {
    const latest = toasts.items.at(-1);
    if (!latest || latest.id <= lastId) return;
    lastId = latest.id;
    if (latest.tone === "error") assertive = latest.message;
    else polite = latest.message;
  });

  $effect(() => {
    const onvis = () => (document.hidden ? toasts.hold() : toasts.release());
    document.addEventListener("visibilitychange", onvis);
    return () => document.removeEventListener("visibilitychange", onvis);
  });
</script>

<div class="vy-sr-only" aria-live="polite">{polite}</div>
<div class="vy-sr-only" aria-live="assertive">{assertive}</div>

<section
  class="toasts"
  aria-label={t("toast.region")}
  onpointerenter={() => toasts.hold()}
  onpointerleave={() => toasts.release()}
  onfocusin={() => toasts.hold()}
  onfocusout={() => toasts.release()}
>
  <ol>
    {#each toasts.items as toast (toast.id)}
      <li class="toast tone-{toast.tone}">
        <span class="icon"><Icon name={ICON[toast.tone]} size={17} /></span>
        <p class="msg">{toast.message}</p>
        {#if toast.action}
          {@const action = toast.action}
          <button
            class="vy-btn vy-btn--sm action"
            onclick={() => {
              action.run();
              toasts.dismiss(toast.id);
            }}>{action.label}</button
          >
        {/if}
        <button class="vy-btn vy-btn--icon vy-btn--sm" aria-label={t("action.dismiss")} onclick={() => toasts.dismiss(toast.id)}>
          <Icon name="x" size={14} />
        </button>
      </li>
    {/each}
  </ol>
</section>

<style>
  .toasts {
    position: fixed;
    inset-block-end: calc(var(--vy-size-statusbar) + var(--vy-space-3));
    inset-inline-end: var(--vy-space-4);
    z-index: var(--vy-z-toast);
    inline-size: min(380px, calc(100vw - 2rem));
    pointer-events: none;
  }
  ol {
    display: flex;
    flex-direction: column;
    gap: var(--vy-space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .toast {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2-5);
    padding-block: var(--vy-space-2);
    padding-inline: var(--vy-space-3) var(--vy-space-1-5);
    border-radius: var(--vy-radius-md);
    border: 1px solid var(--vy-color-border);
    background: var(--vy-color-surface-raised);
    box-shadow: var(--vy-shadow-lg);
    pointer-events: auto;
    transition:
      opacity var(--vy-duration-slow) var(--vy-ease-out),
      transform var(--vy-duration-slow) var(--vy-ease-out);
  }
  @starting-style {
    .toast {
      opacity: 0;
      transform: translateY(12px) scale(0.98);
    }
  }
  .icon {
    display: inline-flex;
    color: var(--tone);
  }
  .msg {
    flex: 1;
    min-inline-size: 0;
    overflow-wrap: anywhere;
  }
  .action {
    color: var(--vy-color-accent-text);
  }
  .tone-info {
    --tone: var(--vy-color-accent-text);
  }
  .tone-success {
    --tone: var(--vy-color-success-text);
  }
  .tone-warning {
    --tone: var(--vy-color-warning-text);
  }
  .tone-error {
    --tone: var(--vy-color-danger-text);
    border-color: color-mix(in srgb, var(--vy-color-danger) 40%, var(--vy-color-border));
  }
  @media (prefers-reduced-motion: reduce) {
    .toast {
      transition: opacity 120ms ease;
    }
  }
</style>
