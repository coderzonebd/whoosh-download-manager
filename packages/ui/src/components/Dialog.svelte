<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Modal dialog primitive: labelled, focus moves in on open and back to the
  trigger on close, Tab is trapped, Escape closes. The app marks the rest of
  the window `inert` while a dialog is open. Enters with a short scale+fade
  (CSS @starting-style), leaves instantly — exits should never make you wait.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    title: string;
    description?: string;
    closeLabel: string;
    onclose: () => void;
    size?: "sm" | "md" | "lg";
    /** alertdialog for confirmations. */
    role?: "dialog" | "alertdialog";
    /** CSS selector of the element to focus first (default: first field or button). */
    initialFocus?: string;
    children: Snippet;
    footer?: Snippet;
  }
  let { title, description, closeLabel, onclose, size = "md", role = "dialog", initialFocus, children, footer }: Props = $props();

  const uid = $props.id();
  let panel: HTMLDivElement | undefined = $state();

  const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  $effect(() => {
    const el = panel;
    if (!el) return;
    const returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const first =
      (initialFocus ? el.querySelector<HTMLElement>(initialFocus) : null) ??
      el.querySelector<HTMLElement>(".body " + FOCUSABLE.split(", ").join(", .body ")) ??
      el.querySelector<HTMLElement>(FOCUSABLE) ??
      el;
    first.focus();

    const onkey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onclose();
        return;
      }
      if (e.key !== "Tab") return;
      const items = [...el.querySelectorAll<HTMLElement>(FOCUSABLE)];
      if (items.length === 0) return;
      const firstItem = items[0];
      const lastItem = items[items.length - 1];
      if (e.shiftKey && document.activeElement === firstItem) {
        e.preventDefault();
        lastItem?.focus();
      } else if (!e.shiftKey && document.activeElement === lastItem) {
        e.preventDefault();
        firstItem?.focus();
      }
    };
    el.addEventListener("keydown", onkey);
    return () => {
      el.removeEventListener("keydown", onkey);
      if (returnTo?.isConnected) returnTo.focus();
    };
  });
</script>

<div class="layer">
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="scrim" aria-hidden="true" onclick={onclose}></div>
  <div
    bind:this={panel}
    class="dialog {size}"
    {role}
    aria-modal="true"
    aria-labelledby="{uid}-title"
    aria-describedby={description ? `${uid}-desc` : undefined}
    tabindex="-1"
  >
    <header>
      <h2 id="{uid}-title">{title}</h2>
      <button class="vy-btn vy-btn--icon vy-btn--sm close" aria-label={closeLabel} onclick={onclose}>
        <Icon name="x" size={15} />
      </button>
    </header>
    {#if description}
      <p id="{uid}-desc" class="desc">{description}</p>
    {/if}
    <div class="body">
      {@render children()}
    </div>
    {#if footer}
      <footer>{@render footer()}</footer>
    {/if}
  </div>
</div>

<style>
  .layer {
    position: fixed;
    inset: 0;
    z-index: var(--vy-z-dialog);
    display: grid;
    place-items: center;
    padding: var(--vy-space-4);
  }
  .scrim {
    position: absolute;
    inset: 0;
    background: var(--vy-color-scrim);
    transition: opacity var(--vy-duration-normal) var(--vy-ease-out);
  }
  .dialog {
    position: relative;
    display: flex;
    flex-direction: column;
    inline-size: 100%;
    max-block-size: min(100%, 720px);
    border-radius: var(--vy-radius-lg);
    background: var(--vy-color-surface-raised);
    box-shadow: var(--vy-shadow-lg);
    outline: none;
    transition:
      opacity var(--vy-duration-normal) var(--vy-ease-out),
      transform var(--vy-duration-normal) var(--vy-ease-out);
  }
  @starting-style {
    .scrim {
      opacity: 0;
    }
    .dialog {
      opacity: 0;
      transform: scale(0.97) translateY(6px);
    }
  }
  .sm {
    max-inline-size: 420px;
  }
  .md {
    max-inline-size: 540px;
  }
  .lg {
    max-inline-size: 680px;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--vy-space-3);
    padding-block: var(--vy-space-4) var(--vy-space-1);
    padding-inline: var(--vy-space-5) var(--vy-space-3);
  }
  h2 {
    font-size: var(--vy-font-size-xl);
    font-weight: var(--vy-font-weight-semibold);
    letter-spacing: var(--vy-letter-spacing-tight);
    line-height: var(--vy-line-height-tight);
  }
  .desc {
    padding-inline: var(--vy-space-5);
    color: var(--vy-color-text-muted);
  }
  .body {
    overflow-y: auto;
    padding-block: var(--vy-space-4);
    padding-inline: var(--vy-space-5);
  }
  footer {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--vy-space-2);
    padding-block: var(--vy-space-3) var(--vy-space-4);
    padding-inline: var(--vy-space-5);
    border-block-start: 1px solid var(--vy-color-border);
  }
  @media (prefers-reduced-motion: reduce) {
    /* keep the fade, drop the movement */
    .dialog,
    .scrim {
      transition: opacity 120ms ease;
      transform: none;
    }
  }
</style>
