<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Confirm clearing history entries. Files on disk are kept. -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import Dialog from "./Dialog.svelte";

  interface Props {
    ids: string[];
    onclose: () => void;
  }
  let { ids, onclose }: Props = $props();

  const app = getApp();
  const { t } = app.i18n;

  async function confirm(): Promise<void> {
    const list = [...ids];
    onclose();
    await app.clearHistory(list);
  }
</script>

<Dialog
  role="alertdialog"
  size="sm"
  title={t("history.clearTitle", { count: ids.length })}
  description={t("history.clearBody")}
  closeLabel={t("action.close")}
  {onclose}
  initialFocus=".danger"
>
  {#snippet children()}{/snippet}
  {#snippet footer()}
    <button class="vy-btn vy-btn--secondary" onclick={onclose}>{t("action.cancel")}</button>
    <button class="vy-btn vy-btn--danger danger" onclick={confirm}>{t("history.clearConfirm")}</button>
  {/snippet}
</Dialog>
