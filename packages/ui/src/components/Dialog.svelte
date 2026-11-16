<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Modal dialog primitive: labelled, focus moves in on open and back to the
  trigger on close, Tab is trapped, Escape closes. The app marks the rest of
  the window `inert` while a dialog is open. Enters with a short scale+fade
  (CSS @starting-style), leaves instantly — exits should never make you wait.

  On a phone (narrow screen, touch) it is a bottom sheet instead: it rises
  from the bottom edge, clears the system bars (`--wdm-inset-*`, set by the
  Android shell) and the keyboard, and follows a finger dragging its top
  edge down; let go far or fast enough and it closes, else it settles back.
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

  // Drag-to-dismiss, only where the dialog is a sheet. 1:1 with the finger from where it was
  // grabbed; on release the motion is projected forward (how far it would coast) to decide.
  const SHEET = "(max-width: 600px) and (pointer: coarse)";
  let drag = $state<{ id: number; y0: number; dy: number; samples: { y: number; t: number }[] } | undefined>(undefined);
  function grab(e: PointerEvent): void {
    if (!matchMedia(SHEET).matches || e.button !== 0) return;
    if (e.target instanceof Element && e.target.closest("button, a, input, select, textarea")) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag = { id: e.pointerId, y0: e.clientY, dy: 0, samples: [{ y: e.clientY, t: e.timeStamp }] };
  }
  function move(e: PointerEvent): void {
    if (!drag || e.pointerId !== drag.id) return;
    const raw = e.clientY - drag.y0;
    // Upwards it resists (rubber band): there is nothing above a sheet.
    drag.dy = raw >= 0 ? raw : -Math.sqrt(-raw) * 2;
    drag.samples = [...drag.samples.slice(-4), { y: e.clientY, t: e.timeStamp }];
  }
  function release(e: PointerEvent): void {
    if (!drag || e.pointerId !== drag.id) return;
    const s = drag.samples;
    const a = s[0];
    const b = s[s.length - 1];
    const v = a && b && b.t > a.t ? ((b.y - a.y) / (b.t - a.t)) * 1000 : 0; // px/s
    const projected = drag.dy + (v / 1000) * (0.998 / (1 - 0.998));
    const h = panel?.offsetHeight ?? 400;
    drag = undefined;
    if (projected > h * 0.45 || v > 1100) onclose();
  }

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
    class:dragging={drag !== undefined}
    style:translate={drag ? `0 ${drag.dy}px` : undefined}
  >
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="grip" onpointerdown={grab} onpointermove={move} onpointerup={release} onpointercancel={release}>
      <span class="handle" aria-hidden="true"></span>
    <header>
      <h2 id="{uid}-title">{title}</h2>
      <button class="vy-btn vy-btn--icon vy-btn--sm close" aria-label={closeLabel} onclick={onclose}>
        <Icon name="x" size={15} />
      </button>
    </header>
    </div>
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
    /* A definite row, so the dialog's max-block-size: 100% holds and its body scrolls. */
    grid-template-rows: minmax(0, 1fr);
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
    min-inline-size: 0;
    /* A title that quotes a file name may hold one long unbreakable word. */
    overflow-wrap: anywhere;
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
    /* A tall dialog scrolls its body; the header and the buttons stay in view. */
    min-block-size: 0;
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
  .handle {
    display: none;
  }
  /* Phone: a bottom sheet. */
  @media (max-width: 600px) and (pointer: coarse) {
    .layer {
      grid-template-columns: minmax(0, 1fr);
      place-items: end stretch;
      padding: 0;
      padding-block-start: calc(var(--wdm-inset-top, env(safe-area-inset-top, 0px)) + var(--vy-space-6));
    }
    .dialog,
    .sm,
    .md,
    .lg {
      max-inline-size: none;
      max-block-size: 100%;
      border-end-start-radius: 0;
      border-end-end-radius: 0;
      border-start-start-radius: var(--vy-radius-xl);
      border-start-end-radius: var(--vy-radius-xl);
      padding-block-end: max(var(--wdm-inset-bottom, env(safe-area-inset-bottom, 0px)), var(--wdm-inset-ime, 0px));
      transition:
        translate 380ms cubic-bezier(0.32, 0.72, 0, 1),
        opacity var(--vy-duration-normal) var(--vy-ease-out);
    }
    .dialog.dragging {
      transition: none;
    }
    .grip {
      touch-action: none;
    }
    .handle {
      display: block;
      inline-size: 36px;
      block-size: 5px;
      margin-block: var(--vy-space-2) 0;
      margin-inline: auto;
      border-radius: var(--vy-radius-full);
      background: var(--vy-color-border-strong);
    }
    header {
      padding-block-start: var(--vy-space-2);
    }
    footer {
      padding-block-end: var(--vy-space-3);
    }
    footer > :global(.vy-btn) {
      flex: 1 1 auto;
    }
    @starting-style {
      .dialog {
        opacity: 1;
        transform: none;
        translate: 0 100%;
      }
    }
  }
  @media (prefers-reduced-motion: reduce) {
    /* keep the fade, drop the movement */
    .dialog,
    .scrim {
      transition: opacity 120ms ease;
      transform: none;
    }
    @starting-style {
      .dialog {
        opacity: 0;
        translate: none;
      }
    }
  }
</style>
