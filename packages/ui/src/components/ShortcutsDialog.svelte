<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import type { MessageKey } from "../lib/i18n/en.ts";
  import Dialog from "./Dialog.svelte";

  interface Props {
    onclose: () => void;
  }
  let { onclose }: Props = $props();
  const { t } = getApp().i18n;

  const mac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
  const mod = mac ? "⌘" : "Ctrl";

  const rows: [string[], MessageKey][] = [
    [[mod, "N"], "shortcuts.add"],
    [[mod, "F"], "shortcuts.search"],
    [[mod, ","], "shortcuts.settings"],
    [["↑", "↓", "PgUp", "PgDn", "Home", "End"], "shortcuts.move"],
    [["Shift", "↑ / ↓"], "shortcuts.extend"],
    [[mod, "A"], "shortcuts.selectAll"],
    [["Space"], "shortcuts.toggle"],
    [["Enter"], "shortcuts.details"],
    [[mac ? "⌘ ⌫" : "Delete"], "shortcuts.remove"],
    [["Shift", "F10"], "shortcuts.menu"],
    [["Esc"], "shortcuts.escape"],
    [["?"], "shortcuts.help"],
  ];
</script>

<Dialog title={t("shortcuts.title")} closeLabel={t("action.close")} {onclose} size="md">
  <dl class="list">
    {#each rows as [keys, label] (label)}
      <div class="row">
        <dt>{t(label)}</dt>
        <dd>
          {#each keys as k (k)}<kbd class="vy-kbd">{k}</kbd>{/each}
        </dd>
      </div>
    {/each}
  </dl>
</Dialog>

<style>
  .list {
    display: grid;
    margin: 0;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--vy-space-4);
    padding-block: var(--vy-space-2);
    border-block-end: 1px solid var(--vy-color-border);
  }
  .row:last-child {
    border-block-end: 0;
  }
  dd {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--vy-space-1);
    margin: 0;
  }
</style>
