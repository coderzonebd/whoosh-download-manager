<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Delete a named queue. Its downloads move to the main queue (PROTOCOL.md). -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import Dialog from "./Dialog.svelte";

  interface Props {
    name: string;
    onclose: () => void;
  }
  let { name, onclose }: Props = $props();

  const app = getApp();
  const { t } = app.i18n;
  const count = $derived(app.store.counts.queues[name] ?? 0);

  async function confirm(): Promise<void> {
    const n = name;
    onclose();
    await app.removeQueue(n);
  }
</script>

<Dialog
  role="alertdialog"
  size="sm"
  title={t("queue.removeTitle", { name })}
  description={count > 0 ? t("queue.removeBody", { count }) : t("queue.removeBodyEmpty")}
  closeLabel={t("action.close")}
  {onclose}
  initialFocus=".danger"
>
  {#snippet children()}{/snippet}
  {#snippet footer()}
    <button class="vy-btn vy-btn--secondary" onclick={onclose}>{t("action.cancel")}</button>
    <button class="vy-btn vy-btn--danger danger" onclick={confirm}>{t("queue.remove")}</button>
  {/snippet}
</Dialog>
