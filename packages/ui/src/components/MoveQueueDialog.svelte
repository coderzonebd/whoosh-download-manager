<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- "Move to queue…": pick the queue for the selected downloads. -->
<script lang="ts">
  import { MAIN_QUEUE } from "@whoosh/api-client";
  import { untrack } from "svelte";
  import { queueOf } from "../lib/state/downloads.svelte.ts";
  import { getApp } from "../lib/state/app.svelte.ts";
  import Dialog from "./Dialog.svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    ids: string[];
    onclose: () => void;
  }
  let { ids, onclose }: Props = $props();

  const app = getApp();
  const { t } = app.i18n;

  const first = untrack(() => app.store.get(ids[0]));
  const current = first ? queueOf(first.data) : MAIN_QUEUE;
  const names = $derived([MAIN_QUEUE, ...app.queues.named.map((q) => q.name)]);
  // Start on another queue than the one they are in: that is why you opened this.
  let target = $state(untrack(() => names.find((n) => n !== current) ?? MAIN_QUEUE));
  let moving = $state(false);
  const name = $derived(first?.data.filename ?? "");

  async function submit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    if (moving) return;
    moving = true;
    const ok = await app.moveToQueue([...ids], target);
    moving = false;
    if (ok) onclose();
  }
</script>

<Dialog
  title={t("move.title", { count: ids.length, name })}
  description={t("move.description")}
  closeLabel={t("action.close")}
  size="sm"
  {onclose}
  initialFocus="input[type=radio]:checked"
>
  <form id="vy-move-form" onsubmit={submit}>
    <fieldset class="list">
      <legend class="vy-sr-only">{t("move.queue")}</legend>
      {#each names as n (n)}
        <label class="option" class:on={target === n}>
          <input type="radio" name="vy-move-queue" value={n} bind:group={target} />
          <Icon name="layers" size={15} />
          <span class="vy-truncate label" dir="auto">{app.queueLabel(n)}</span>
          {#if n === current && ids.length === 1}<span class="here">{t("move.current")}</span>{/if}
        </label>
      {/each}
    </fieldset>
  </form>
  {#snippet footer()}
    <button type="button" class="vy-btn vy-btn--secondary" onclick={onclose}>{t("action.cancel")}</button>
    <button type="submit" form="vy-move-form" class="vy-btn vy-btn--primary" disabled={moving}>
      {#if moving}<span class="vy-spinner" aria-hidden="true"></span>{/if}{t("move.submit")}
    </button>
  {/snippet}
</Dialog>

<style>
  .list {
    display: grid;
    gap: 2px;
    margin: 0;
    padding: 0;
    border: 0;
  }
  .option {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2-5);
    min-block-size: 36px;
    padding-inline: var(--vy-space-3);
    border-radius: var(--vy-radius-sm);
    cursor: pointer;
  }
  .option.on {
    background: var(--vy-color-accent-subtle);
    box-shadow: inset 0 0 0 1px var(--vy-color-accent);
  }
  .option:has(input:focus-visible) {
    outline: var(--vy-size-focus-width) solid var(--vy-color-focus);
    outline-offset: 1px;
  }
  .label {
    flex: 1;
  }
  .here {
    color: var(--vy-color-text-subtle);
    font-size: var(--vy-font-size-xs);
  }
  @media (hover: hover) and (pointer: fine) {
    .option:hover {
      background: var(--vy-color-surface-hover);
    }
  }
</style>
