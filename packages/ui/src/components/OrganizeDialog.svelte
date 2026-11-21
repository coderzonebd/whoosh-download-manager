<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Asked once: sort downloads into folders by type (Settings.organizeByCategory). Sorting
     creates folders in the user's download folder, so it waits for a yes. -->
<script lang="ts">
  import { CATEGORY_FOLDERS } from "@wdm/api-client/categories";
  import { getApp } from "../lib/state/app.svelte.ts";
  import Dialog from "./Dialog.svelte";
  import Icon from "./Icon.svelte";

  const app = getApp();
  const { t } = app.i18n;
  const folder = $derived(app.settings?.defaultDirectory ?? "");
  let busy = $state(false);

  async function answer(on: boolean): Promise<void> {
    busy = true;
    await app.setOrganize(on);
    busy = false;
  }
</script>

<Dialog
  size="sm"
  title={t("organize.title")}
  description={t("organize.body", { folder })}
  closeLabel={t("action.close")}
  onclose={() => (app.organizeDismissed = true)}
  initialFocus=".yes"
>
  {#snippet children()}
    <p class="lead">{t("organize.folders")}</p>
    <ul class="folders">
      {#each CATEGORY_FOLDERS as name (name)}
        <li><Icon name="folder" size={15} /><span dir="ltr">{name}</span></li>
      {/each}
    </ul>
    <p class="note">{t("organize.note", { folder })}</p>
  {/snippet}
  {#snippet footer()}
    <button class="vy-btn vy-btn--secondary" disabled={busy} onclick={() => answer(false)}>{t("organize.no")}</button>
    <button class="vy-btn vy-btn--primary yes" disabled={busy} onclick={() => answer(true)}>{t("organize.yes")}</button>
  {/snippet}
</Dialog>

<style>
  .lead,
  .note {
    margin: 0;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .note {
    margin-top: var(--vy-space-3);
  }
  .folders {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--vy-space-1) var(--vy-space-3);
    margin: var(--vy-space-2) 0 0;
    padding: 0;
    list-style: none;
  }
  .folders li {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2);
    min-width: 0;
    color: var(--vy-color-text);
  }
</style>
