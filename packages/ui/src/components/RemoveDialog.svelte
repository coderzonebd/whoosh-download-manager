<!-- SPDX-License-Identifier: Apache-2.0 -->
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
  let deleteFiles = $state(false);
  const name = $derived(app.store.get(ids[0])?.data.filename ?? "");

  async function confirm(): Promise<void> {
    const list = [...ids];
    const del = deleteFiles;
    onclose();
    await app.remove(list, del);
  }
</script>

<Dialog
  role="alertdialog"
  size="sm"
  title={ids.length === 1 ? t("remove.titleOne", { name }) : t("remove.title", { count: ids.length })}
  description={t("remove.body")}
  closeLabel={t("action.close")}
  {onclose}
  initialFocus=".danger"
>
  <label class="vy-check">
    <input type="checkbox" bind:checked={deleteFiles} />
    {t("remove.deleteFiles")}
  </label>
  {#snippet footer()}
    <button class="vy-btn vy-btn--secondary" onclick={onclose}>{t("action.cancel")}</button>
    <button class="vy-btn vy-btn--danger danger" onclick={confirm}>{t("remove.confirm")}</button>
  {/snippet}
</Dialog>
