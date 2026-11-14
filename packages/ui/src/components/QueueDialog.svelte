<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Create a queue, or edit one: its name (a rename), how many of its
  downloads run at once, and its schedule (start and stop times, days, and
  what to do once it is done). The main queue has only the limit: it can't
  be renamed and has no schedule (PROTOCOL.md).
-->
<script lang="ts">
  import { MAIN_QUEUE, type AfterDone, type QueueConfig, type Schedule } from "@whoosh/api-client";
  import { untrack } from "svelte";
  import { isClock, normalizeDays } from "../lib/schedule.ts";
  import { getApp, type QueueDialog } from "../lib/state/app.svelte.ts";
  import { queueNameProblem } from "../lib/state/queues.svelte.ts";
  import DaysPicker from "./DaysPicker.svelte";
  import Dialog from "./Dialog.svelte";

  interface Props {
    dialog: QueueDialog;
    onclose: () => void;
  }
  let { dialog, onclose }: Props = $props();

  const app = getApp();
  const { i18n, queues } = app;
  const { t } = i18n;

  const original = untrack(() => (dialog.mode === "edit" ? queues.get(dialog.name) : undefined));
  const isMain = original?.name === MAIN_QUEUE;
  const s = original?.schedule;

  let name = $state(original?.name ?? "");
  let maxActive = $state<string | number>(String(original?.maxActive ?? 1));
  let startOn = $state(s?.start !== undefined);
  let startAt = $state(s?.start?.at ?? "22:00");
  let startDays = $state<number[]>(s?.start?.days ?? []);
  let stopOn = $state(s?.stop !== undefined);
  let stopAt = $state(s?.stop?.at ?? "07:00");
  let stopDays = $state<number[]>(s?.stop?.days ?? []);
  let afterDone = $state<AfterDone>(s?.afterDone ?? "none");
  let touched = $state(false);
  let saving = $state(false);
  let saveError = $state<string | undefined>(undefined);

  const errors = $derived.by(() => {
    const e: Partial<Record<"name" | "maxActive" | "startAt" | "stopAt", string>> = {};
    if (!isMain) {
      const n = name.trim();
      const p = queueNameProblem(n);
      if (p === "empty") e.name = t("settings.required");
      else if (p === "long") e.name = t("queue.nameTooLong");
      else if (p === "control") e.name = t("queue.nameInvalid");
      else if (n !== original?.name && queues.has(n)) e.name = t("queue.nameTaken", { name: n });
    }
    const m = String(maxActive).trim();
    if (!/^\d+$/.test(m) || Number(m) < 1 || Number(m) > 64) e.maxActive = t("settings.range", { min: i18n.number(1), max: i18n.number(64) });
    if (startOn && !isClock(startAt)) e.startAt = t("schedule.badTime");
    if (stopOn && !isClock(stopAt)) e.stopAt = t("schedule.badTime");
    return e;
  });
  const valid = $derived(Object.keys(errors).length === 0);

  function schedule(): Schedule | undefined {
    if (isMain) return undefined;
    const out: Schedule = {};
    if (startOn) {
      out.start = { at: startAt };
      const d = normalizeDays(startDays);
      if (d) out.start.days = d;
    }
    if (stopOn) {
      out.stop = { at: stopAt };
      const d = normalizeDays(stopDays);
      if (d) out.stop.days = d;
    }
    if (afterDone !== "none") out.afterDone = afterDone;
    return Object.keys(out).length === 0 ? undefined : out;
  }

  async function submit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    touched = true;
    if (!valid || saving) return;
    saving = true;
    saveError = undefined;
    const config: QueueConfig = { name: isMain ? MAIN_QUEUE : name.trim(), maxActive: Number(String(maxActive).trim()) };
    const sched = schedule();
    if (sched) config.schedule = sched;
    try {
      if (dialog.mode === "create") {
        await app.createQueue(config);
        app.toasts.push({ tone: "success", message: t("queue.created", { name: config.name }) }, 3000);
        app.store.view = { kind: "queue", queue: config.name };
        app.view = "downloads";
      } else {
        await app.editQueue(original?.name ?? config.name, config);
        app.toasts.push({ tone: "success", message: t("queue.saved", { name: app.queueLabel(config.name) }) }, 3000);
      }
      onclose();
    } catch (err) {
      saveError = t("queue.saveFailed", { message: app.describe(err) });
    } finally {
      saving = false;
    }
  }

  const title = $derived(dialog.mode === "create" ? t("queue.createTitle") : t("queue.editTitle", { name: app.queueLabel(original?.name ?? "") }));
  const show = (k: keyof typeof errors) => (touched || k !== "name") && errors[k] !== undefined;
</script>

<Dialog {title} description={isMain ? t("queue.mainDescription") : t("queue.description")} closeLabel={t("action.close")} {onclose} size="lg">
  <form id="vy-queue-form" class="form" onsubmit={submit} novalidate>
    <div class="two">
      {#if !isMain}
        <div class="vy-field">
          <label class="vy-label" for="vy-queue-name">{t("queue.name")}</label>
          <input
            id="vy-queue-name"
            class="vy-input"
            dir="auto"
            autocomplete="off"
            maxlength="64"
            bind:value={name}
            onblur={() => (touched = true)}
            aria-invalid={show("name")}
            aria-describedby={show("name") ? "vy-queue-name-err" : undefined}
          />
          {#if show("name")}<span id="vy-queue-name-err" class="vy-error">{errors.name}</span>{/if}
        </div>
      {/if}
      <div class="vy-field">
        <label class="vy-label" for="vy-queue-max">{t("queue.maxActive")}</label>
        <input
          id="vy-queue-max"
          class="vy-input num"
          type="number"
          min="1"
          max="64"
          inputmode="numeric"
          bind:value={maxActive}
          aria-invalid={show("maxActive")}
          aria-describedby="vy-queue-max-hint"
        />
        <span id="vy-queue-max-hint" class={show("maxActive") ? "vy-error" : "vy-hint"}>{show("maxActive") ? errors.maxActive : t("queue.maxActiveHint")}</span>
      </div>
    </div>

    {#if !isMain}
      <fieldset class="schedule">
        <legend>{t("schedule.title")}</legend>

        <div class="when">
          <label class="vy-check">
            <input type="checkbox" bind:checked={startOn} />
            {t("schedule.start")}
          </label>
          <input
            class="vy-input time"
            type="time"
            aria-label={t("schedule.startTime")}
            disabled={!startOn}
            bind:value={startAt}
            aria-invalid={show("startAt")}
          />
          {#if show("startAt")}<span class="vy-error">{errors.startAt}</span>{/if}
          <DaysPicker bind:days={startDays} legend={t("schedule.startDays")} disabled={!startOn} />
        </div>

        <div class="when">
          <label class="vy-check">
            <input type="checkbox" bind:checked={stopOn} />
            {t("schedule.stop")}
          </label>
          <input
            class="vy-input time"
            type="time"
            aria-label={t("schedule.stopTime")}
            disabled={!stopOn}
            bind:value={stopAt}
            aria-invalid={show("stopAt")}
          />
          {#if show("stopAt")}<span class="vy-error">{errors.stopAt}</span>{/if}
          <DaysPicker bind:days={stopDays} legend={t("schedule.stopDays")} disabled={!stopOn} />
        </div>

        <div class="vy-field">
          <label class="vy-label" for="vy-queue-after">{t("schedule.afterDone")}</label>
          <select id="vy-queue-after" class="vy-select after" bind:value={afterDone} aria-describedby="vy-queue-after-hint">
            <option value="none">{t("afterDone.none")}</option>
            <option value="sleep">{t("afterDone.sleep")}</option>
            <option value="shutdown">{t("afterDone.shutdown")}</option>
          </select>
          <span id="vy-queue-after-hint" class="vy-hint">{t("schedule.afterDoneHint")}</span>
        </div>
        <p class="vy-hint">{t("schedule.localTime")}</p>
      </fieldset>
    {/if}

    {#if saveError}<p class="vy-error" role="alert">{saveError}</p>{/if}
  </form>

  {#snippet footer()}
    <button type="button" class="vy-btn vy-btn--secondary" onclick={onclose}>{t("action.cancel")}</button>
    <button type="submit" form="vy-queue-form" class="vy-btn vy-btn--primary" disabled={saving}>
      {#if saving}<span class="vy-spinner" aria-hidden="true"></span>{/if}
      {dialog.mode === "create" ? t("queue.create") : t("action.save")}
    </button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: grid;
    gap: var(--vy-space-5);
  }
  .two {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: var(--vy-space-4);
  }
  .num {
    max-inline-size: 140px;
  }
  .schedule {
    display: grid;
    gap: var(--vy-space-4);
    margin: 0;
    padding: var(--vy-space-4);
    border: 1px solid var(--vy-color-border);
    border-radius: var(--vy-radius-md);
    min-inline-size: 0;
  }
  .schedule legend {
    padding-inline: var(--vy-space-1);
    font-weight: var(--vy-font-weight-semibold);
  }
  .when {
    display: grid;
    grid-template-columns: minmax(150px, auto) auto 1fr;
    align-items: center;
    gap: var(--vy-space-2) var(--vy-space-3);
  }
  .when > :global(.days) {
    grid-column: 1 / -1;
    padding-inline-start: var(--vy-space-6);
  }
  .time {
    inline-size: 9rem;
  }
  .after {
    max-inline-size: 280px;
  }
  @container vy-app (max-width: 560px) {
    .when {
      grid-template-columns: 1fr;
    }
    .when > :global(.days) {
      padding-inline-start: 0;
    }
  }
</style>
