<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Days of the week as toggle chips (real checkboxes, so a screen reader
  hears "Monday, checkbox, checked"). 0 = Sunday … 6 = Saturday, as in the
  protocol. No day ticked means every day; the hint says so.
-->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";

  interface Props {
    days: number[];
    legend: string;
    disabled?: boolean;
  }
  let { days = $bindable(), legend, disabled = false }: Props = $props();

  const uid = $props.id();
  const { i18n } = getApp();
  const { t } = i18n;

  // 4 January 1970 was a Sunday.
  const names = $derived.by(() => {
    const short = new Intl.DateTimeFormat(i18n.intlLocale, { weekday: "short", timeZone: "UTC" });
    const long = new Intl.DateTimeFormat(i18n.intlLocale, { weekday: "long", timeZone: "UTC" });
    return [0, 1, 2, 3, 4, 5, 6].map((d) => {
      const date = new Date(Date.UTC(1970, 0, 4 + d));
      return { day: d, short: short.format(date), long: long.format(date) };
    });
  });

  function toggle(day: number, on: boolean): void {
    const set = new Set(days);
    if (on) set.add(day);
    else set.delete(day);
    days = [...set].sort((a, b) => a - b);
  }
</script>

<fieldset class="days" {disabled} aria-describedby="{uid}-hint">
  <legend class="vy-label">{legend}</legend>
  <div class="chips">
    {#each names as n (n.day)}
      <label class="chip" class:on={days.includes(n.day)} title={n.long}>
        <input type="checkbox" checked={days.includes(n.day)} aria-label={n.long} onchange={(e) => toggle(n.day, e.currentTarget.checked)} />
        <span aria-hidden="true">{n.short}</span>
      </label>
    {/each}
  </div>
  <span id="{uid}-hint" class="vy-hint">{days.length === 0 || days.length === 7 ? t("schedule.everyDay") : t("schedule.someDays")}</span>
</fieldset>

<style>
  .days {
    display: grid;
    gap: var(--vy-space-1);
    margin: 0;
    padding: 0;
    border: 0;
    min-inline-size: 0;
  }
  legend {
    padding: 0;
    margin-block-end: var(--vy-space-1);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--vy-space-1);
  }
  .chip {
    position: relative;
    display: inline-grid;
    place-items: center;
    min-inline-size: 44px;
    min-block-size: 28px;
    padding-inline: var(--vy-space-1-5);
    border-radius: var(--vy-radius-sm);
    border: 1px solid var(--vy-color-border-strong);
    background: var(--vy-color-bg);
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
    cursor: pointer;
    user-select: none;
  }
  .chip.on {
    border-color: var(--vy-color-accent);
    background: var(--vy-color-accent);
    color: var(--vy-color-text-on-accent);
    font-weight: var(--vy-font-weight-medium);
  }
  .chip input {
    position: absolute;
    inset: 0;
    margin: 0;
    opacity: 0;
    cursor: pointer;
  }
  .chip:has(input:focus-visible) {
    outline: var(--vy-size-focus-width) solid var(--vy-color-focus);
    outline-offset: 1px;
  }
  .days:disabled .chip {
    opacity: 0.55;
    cursor: not-allowed;
  }
  @media (forced-colors: active) {
    .chip.on {
      border-width: 2px;
      forced-color-adjust: none;
      background: Highlight;
      color: HighlightText;
    }
  }
</style>
