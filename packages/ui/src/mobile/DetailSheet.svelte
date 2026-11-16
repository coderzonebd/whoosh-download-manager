<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  A download's details on a phone: the desktop's detail panel (same facts,
  actions, segment map and speed history) in a sheet that rises from the
  bottom. Its close button, Escape, the scrim, Android's back gesture and a
  drag down on the handle all close it.
-->
<script lang="ts">
  import type { Download } from "../lib/state/downloads.svelte.ts";
  import DetailPanel from "../components/DetailPanel.svelte";

  interface Props {
    download: Download;
    onclose: () => void;
    label: string;
  }
  let { download, onclose, label }: Props = $props();

  let panel: HTMLDivElement | undefined = $state();
  let drag = $state<{ id: number; y0: number; dy: number; samples: { y: number; t: number }[] } | undefined>(undefined);

  $effect(() => {
    const el = panel;
    if (!el) return;
    const returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    el.focus();
    const onkey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onclose();
      }
    };
    el.addEventListener("keydown", onkey);
    return () => {
      el.removeEventListener("keydown", onkey);
      if (returnTo?.isConnected) returnTo.focus();
    };
  });

  function grab(e: PointerEvent): void {
    if (e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag = { id: e.pointerId, y0: e.clientY, dy: 0, samples: [{ y: e.clientY, t: e.timeStamp }] };
  }
  function move(e: PointerEvent): void {
    if (!drag || e.pointerId !== drag.id) return;
    const raw = e.clientY - drag.y0;
    drag.dy = raw >= 0 ? raw : -Math.sqrt(-raw) * 2;
    drag.samples = [...drag.samples.slice(-4), { y: e.clientY, t: e.timeStamp }];
  }
  function release(e: PointerEvent): void {
    if (!drag || e.pointerId !== drag.id) return;
    const s = drag.samples;
    const a = s[0];
    const b = s[s.length - 1];
    const v = a && b && b.t > a.t ? ((b.y - a.y) / (b.t - a.t)) * 1000 : 0;
    const projected = drag.dy + (v / 1000) * (0.998 / (1 - 0.998));
    const h = panel?.offsetHeight ?? 600;
    drag = undefined;
    if (projected > h * 0.4 || v > 1100) onclose();
  }
</script>

<div class="layer">
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="scrim" aria-hidden="true" onclick={onclose}></div>
  <div
    bind:this={panel}
    class="sheet"
    class:dragging={drag !== undefined}
    style:translate={drag ? `0 ${drag.dy}px` : undefined}
    role="dialog"
    aria-modal="true"
    aria-label={label}
    tabindex="-1"
  >
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="grip" onpointerdown={grab} onpointermove={move} onpointerup={release} onpointercancel={release}>
      <span class="handle" aria-hidden="true"></span>
    </div>
    <DetailPanel {download} />
  </div>
</div>

<style>
  .layer {
    position: fixed;
    inset: 0;
    z-index: var(--vy-z-overlay);
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
    padding-block-start: calc(var(--wdm-inset-top, env(safe-area-inset-top, 0px)) + var(--vy-space-6));
  }
  .scrim {
    position: absolute;
    inset: 0;
    background: var(--vy-color-scrim);
    transition: opacity var(--vy-duration-normal) var(--vy-ease-out);
  }
  .sheet {
    position: relative;
    display: flex;
    flex-direction: column;
    /* A tall sheet: the panel's own scroll area needs a definite height to scroll in. */
    block-size: 100%;
    min-block-size: 0;
    padding-block-end: var(--wdm-inset-bottom, env(safe-area-inset-bottom, 0px));
    border-start-start-radius: var(--vy-radius-xl);
    border-start-end-radius: var(--vy-radius-xl);
    background: var(--vy-color-surface-raised);
    box-shadow: var(--vy-shadow-lg);
    outline: none;
    transition: translate 380ms cubic-bezier(0.32, 0.72, 0, 1);
  }
  .sheet.dragging {
    transition: none;
  }
  @starting-style {
    .scrim {
      opacity: 0;
    }
    .sheet {
      translate: 0 100%;
    }
  }
  .grip {
    flex: none;
    padding-block: var(--vy-space-2) var(--vy-space-1);
    touch-action: none;
  }
  .handle {
    display: block;
    inline-size: 36px;
    block-size: 5px;
    margin-inline: auto;
    border-radius: var(--vy-radius-full);
    background: var(--vy-color-border-strong);
  }
  /* The desktop's side panel, laid out for the sheet. */
  .sheet :global(.panel) {
    inline-size: auto;
    block-size: auto;
    min-block-size: 0;
    flex: 1 1 0;
    border: 0;
    background: transparent;
  }
  @media (prefers-reduced-motion: reduce) {
    .sheet {
      transition: none;
    }
    @starting-style {
      .sheet {
        translate: none;
        opacity: 0;
      }
    }
  }
</style>
