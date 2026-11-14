<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Right-click / Shift+F10 menu for the selection. ARIA menu pattern. -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import Icon from "./Icon.svelte";
  import type { IconName } from "./icons.ts";

  interface Props {
    id: string;
    x: number;
    y: number;
    onclose: () => void;
  }
  let { id, x, y, onclose }: Props = $props();

  const app = getApp();
  const { t } = app.i18n;
  const d = $derived(app.store.get(id));

  interface Item {
    icon: IconName;
    label: string;
    run: () => void;
    danger?: boolean;
  }

  const items: Item[] = $derived.by(() => {
    const sel = app.store.selected();
    const out: Item[] = [];
    const pausable = app.pausableIds(sel);
    const resumable = app.resumableIds(sel);
    if (resumable.length) out.push({ icon: "play", label: t("action.resume"), run: () => app.resume(resumable) });
    if (pausable.length) out.push({ icon: "pause", label: t("action.pause"), run: () => app.pause(pausable) });
    if (d?.data.state === "needs_refresh") out.push({ icon: "refresh", label: t("action.refreshLink"), run: () => app.refreshLink(id) });
    out.push({ icon: "panel", label: t("action.details"), run: () => app.reveal(id) });
    // Moving only means something once there is a queue to move to.
    const movable = sel.filter((s) => s.data.state !== "completed").map((s) => s.id);
    if (app.queues.enabled && app.queues.named.length > 0 && movable.length > 0) {
      out.push({ icon: "layers", label: t("action.moveToQueue"), run: () => (app.moveIds = movable) });
    }
    out.push({ icon: "copy", label: t("action.copyLink"), run: () => app.copyLink(id) });
    out.push({ icon: "trash", label: t("action.remove"), run: () => app.requestRemove(sel.map((s) => s.id)), danger: true });
    return out;
  });

  let menu: HTMLDivElement | undefined = $state();
  let pos = $state({ left: 0, top: 0 });
  let returnTo: HTMLElement | null = null;

  $effect(() => {
    const el = menu;
    if (!el) return;
    returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const r = el.getBoundingClientRect();
    pos = {
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
    };
    el.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    const away = (e: PointerEvent) => {
      if (!el.contains(e.target as Node)) close(false);
    };
    const blur = () => close(false);
    window.addEventListener("pointerdown", away, true);
    window.addEventListener("blur", blur);
    window.addEventListener("resize", blur);
    return () => {
      window.removeEventListener("pointerdown", away, true);
      window.removeEventListener("blur", blur);
      window.removeEventListener("resize", blur);
    };
  });

  function close(restore: boolean): void {
    onclose();
    if (restore && returnTo?.isConnected) returnTo.focus();
  }

  function onkeydown(e: KeyboardEvent): void {
    const nodes = [...(menu?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])];
    const i = nodes.indexOf(document.activeElement as HTMLElement);
    let next: number | undefined;
    if (e.key === "ArrowDown") next = (i + 1) % nodes.length;
    else if (e.key === "ArrowUp") next = (i - 1 + nodes.length) % nodes.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = nodes.length - 1;
    else if (e.key === "Escape" || e.key === "Tab") {
      e.preventDefault();
      close(true);
      return;
    } else return;
    e.preventDefault();
    nodes[next]?.focus();
  }

  function choose(item: Item): void {
    close(true);
    item.run();
  }
</script>

<div
  bind:this={menu}
  class="menu"
  role="menu"
  tabindex="-1"
  aria-label={t("menu.label", { name: d?.data.filename ?? "" })}
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  {onkeydown}
>
  {#each items as item (item.label)}
    <button role="menuitem" tabindex="-1" class="item" class:danger={item.danger} onclick={() => choose(item)}>
      <Icon name={item.icon} size={15} />
      <span>{item.label}</span>
    </button>
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: var(--vy-z-dropdown);
    display: grid;
    min-inline-size: 200px;
    padding: var(--vy-space-1);
    border-radius: var(--vy-radius-md);
    border: 1px solid var(--vy-color-border);
    background: var(--vy-color-surface-raised);
    box-shadow: var(--vy-shadow-lg);
    outline: none;
    transform-origin: top left;
    transition:
      opacity var(--vy-duration-fast) var(--vy-ease-out),
      transform var(--vy-duration-fast) var(--vy-ease-out);
  }
  @starting-style {
    .menu {
      opacity: 0;
      transform: scale(0.97);
    }
  }
  .item {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2-5);
    min-block-size: 30px;
    padding-inline: var(--vy-space-2-5);
    border: 0;
    border-radius: var(--vy-radius-sm);
    background: transparent;
    text-align: start;
    cursor: pointer;
  }
  .item:focus-visible,
  .item:focus {
    outline: none;
    background: var(--vy-color-accent);
    color: var(--vy-color-text-on-accent);
  }
  @media (hover: hover) and (pointer: fine) {
    .item:hover {
      background: var(--vy-color-surface-hover);
    }
    .item:focus:hover {
      background: var(--vy-color-accent);
    }
  }
  .danger {
    color: var(--vy-color-danger-text);
  }
</style>
