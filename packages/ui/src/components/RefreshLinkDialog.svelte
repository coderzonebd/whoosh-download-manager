<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- The manual half of "self-healing links": paste a fresh URL, keep the bytes. -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import Dialog from "./Dialog.svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    id: string;
    onclose: () => void;
  }
  let { id, onclose }: Props = $props();

  const app = getApp();
  const { t } = app.i18n;
  let url = $state("");
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  const name = $derived(app.store.get(id)?.data.filename ?? "");

  async function submit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    if (!url.trim()) return;
    busy = true;
    error = undefined;
    try {
      await app.refreshUrl(id, url.trim());
      onclose();
    } catch (err) {
      error = app.describe(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title={t("refresh.title")} description={t("refresh.body", { name })} closeLabel={t("action.close")} {onclose} initialFocus="#vy-refresh-url">
  <form id="vy-refresh-form" class="vy-field" onsubmit={submit}>
    <label class="vy-label" for="vy-refresh-url">{t("refresh.url")}</label>
    <input
      id="vy-refresh-url"
      class="vy-input"
      type="url"
      dir="ltr"
      spellcheck="false"
      placeholder={t("add.urlPlaceholder")}
      bind:value={url}
      aria-invalid={error !== undefined}
      aria-describedby={error ? "vy-refresh-error" : undefined}
    />
    {#if error}<p id="vy-refresh-error" class="vy-error" role="alert">{error}</p>{/if}
  </form>
  {#snippet footer()}
    <button class="vy-btn vy-btn--secondary" onclick={onclose}>{t("action.cancel")}</button>
    <button type="submit" form="vy-refresh-form" class="vy-btn vy-btn--primary" disabled={!url.trim() || busy}>
      <Icon name="refresh" size={15} />{t("refresh.submit")}
    </button>
  {/snippet}
</Dialog>
