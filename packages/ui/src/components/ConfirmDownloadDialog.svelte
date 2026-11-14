<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  "Ask before downloading from the browser": a download the browser handed
  over waits (paused, `awaitingConfirmation`) until the user answers here.
  The daemon probes it meanwhile, so size and resumability fill in as they
  arrive; the name and category follow the probe until the user edits them.

  Start download → download.confirm { start: true }
  Download later → download.confirm { start: false } (also Escape, the close
                   button and a click outside: never loses the download)
  Cancel         → download.remove (nothing was fetched yet)

  Several waiting downloads are asked about one at a time, oldest first.
-->
<script lang="ts">
  import { MAIN_QUEUE, type ConfirmParams } from "@wdm/api-client";
  import { untrack } from "svelte";
  import { CATEGORIES, categoryForFilename, isCategory, type Category } from "@wdm/api-client/categories";
  import { getApp } from "../lib/state/app.svelte.ts";
  import { queueOf, type Download } from "../lib/state/downloads.svelte.ts";
  import Dialog from "./Dialog.svelte";
  import FileIcon from "./FileIcon.svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    download: Download;
    /** 1-based position among the waiting downloads, and how many there are. */
    index: number;
    total: number;
  }
  let { download, index, total }: Props = $props();

  const app = getApp();
  const { i18n } = app;
  const { t } = i18n;

  const d = $derived(download.data);

  // What the user sees and may edit; until edited, it follows the daemon (the probe).
  // Filled in at once, so "Start download" can take the focus when the dialog opens.
  const first = untrack(() => download.data);
  let filename = $state(first.filename);
  let filenameEdited = $state(false);
  let directory = $state(first.directory);
  let directoryEdited = $state(false);
  let category = $state<string>(first.category ?? categoryForFilename(first.filename));
  let categoryEdited = $state(false);
  let queue = $state(queueOf(first));
  let queueEdited = $state(false);
  let busy = $state<"start" | "later" | "cancel" | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let choosingFolder = $state(false);

  $effect(() => {
    const data = d;
    if (!filenameEdited) filename = data.filename;
    if (!directoryEdited) directory = data.directory;
    if (!categoryEdited) category = data.category ?? categoryForFilename(data.filename);
    if (!queueEdited) queue = queueOf(data);
  });

  const icon = $derived<Category>(isCategory(category) ? category : categoryForFilename(filename));
  const categories = $derived<string[]>(isCategory(category) ? [...CATEGORIES] : [...CATEGORIES, category]);
  const queueNames = $derived(app.queues.enabled ? [MAIN_QUEUE, ...app.queues.named.map((q) => q.name)] : []);
  const host = $derived.by(() => {
    try {
      return new URL(d.url).host;
    } catch {
      return "";
    }
  });
  const ready = $derived(filename.trim() !== "" && directory.trim() !== "" && busy === undefined);

  async function chooseFolder(): Promise<void> {
    const pick = app.host.pickFolder;
    if (!pick || choosingFolder) return;
    choosingFolder = true;
    try {
      const dir = await pick({ title: t("add.chooseFolder"), defaultPath: directory.trim() || undefined });
      if (dir) {
        directory = dir;
        directoryEdited = true;
      }
    } catch (e) {
      app.report(e);
    } finally {
      choosingFolder = false;
    }
  }

  /** Only what the user changed travels; the rest stays as the daemon has it. */
  function answer(start: boolean): ConfirmParams {
    const p: ConfirmParams = { id: download.id, start };
    const name = filename.trim();
    if (filenameEdited && name && name !== d.filename) p.filename = name;
    const dir = directory.trim();
    if (dir && dir !== d.directory) p.directory = dir;
    if (category !== (d.category ?? categoryForFilename(d.filename))) p.category = category;
    if (queue !== queueOf(d)) p.queue = queue;
    return p;
  }

  async function run(kind: "start" | "later" | "cancel"): Promise<void> {
    if (busy) return;
    if (kind !== "cancel" && !ready) return;
    busy = kind;
    error = undefined;
    try {
      if (kind === "cancel") await app.cancelWaiting(download.id);
      else await app.confirmDownload(answer(kind === "start"));
    } catch (e) {
      error = t("confirm.failed", { message: app.describe(e) });
    } finally {
      busy = undefined;
    }
  }

  function submit(e: SubmitEvent): void {
    e.preventDefault();
    void run("start");
  }
</script>

<Dialog
  title={t("confirm.title")}
  description={t("confirm.description")}
  closeLabel={t("action.close")}
  onclose={() => void run("later")}
  initialFocus="#vy-confirm-start"
>
  <form id="vy-confirm-form" class="form" onsubmit={submit} novalidate>
    <div class="preview">
      <FileIcon category={icon} size="lg" />
      <div class="facts">
        <span class="vy-num size">{d.totalBytes === undefined ? t("add.unknownSize") : i18n.bytes(d.totalBytes)}</span>
        <span class="resumable" class:no={!d.resumable}>
          <Icon name={d.resumable ? "check" : "alert"} size={13} />
          {d.resumable ? t("add.resumable") : t("add.notResumable")}
        </span>
        {#if host}<span class="from vy-truncate" title={d.url}>{t("confirm.from", { host })}</span>{/if}
      </div>
    </div>

    <div class="vy-field">
      <label class="vy-label" for="vy-confirm-name">{t("add.filename")}</label>
      <input id="vy-confirm-name" class="vy-input" dir="auto" spellcheck="false" bind:value={filename} oninput={() => (filenameEdited = true)} />
    </div>
    <div class="two">
      <div class="vy-field">
        <label class="vy-label" for="vy-confirm-dir">{t("add.folder")}</label>
        <div class="folder-row">
          <div class="with-icon">
            <Icon name="folder" size={15} />
            <input id="vy-confirm-dir" class="vy-input" dir="auto" spellcheck="false" bind:value={directory} oninput={() => (directoryEdited = true)} />
          </div>
          {#if app.host.pickFolder}
            <button type="button" class="vy-btn vy-btn--secondary" disabled={choosingFolder} onclick={chooseFolder}>{t("action.browse")}</button>
          {/if}
        </div>
      </div>
      <div class="vy-field">
        <label class="vy-label" for="vy-confirm-cat">{t("add.category")}</label>
        <select id="vy-confirm-cat" class="vy-select" bind:value={category} onchange={() => (categoryEdited = true)}>
          {#each categories as c (c)}
            <option value={c}>{isCategory(c) ? t(`category.${c}`) : c}</option>
          {/each}
        </select>
      </div>
    </div>
    {#if queueNames.length > 1}
      <div class="vy-field">
        <label class="vy-label" for="vy-confirm-queue">{t("add.queue")}</label>
        <select id="vy-confirm-queue" class="vy-select queue" bind:value={queue} onchange={() => (queueEdited = true)}>
          {#each queueNames as n (n)}
            <option value={n}>{app.queueLabel(n)}</option>
          {/each}
        </select>
      </div>
    {/if}

    {#if error}<p class="vy-error" role="alert">{error}</p>{/if}
  </form>

  {#snippet footer()}
    {#if total > 1}
      <span class="position vy-num">{t("confirm.position", { index: i18n.number(index), total: i18n.number(total) })}</span>
    {/if}
    <button type="button" class="vy-btn vy-btn--secondary cancel" disabled={busy !== undefined} onclick={() => run("cancel")}>{t("action.cancel")}</button>
    <button type="button" class="vy-btn vy-btn--secondary" disabled={!ready} onclick={() => run("later")}>
      {#if busy === "later"}<span class="vy-spinner" aria-hidden="true"></span>{:else}<Icon name="clock" size={15} />{/if}
      {t("confirm.later")}
    </button>
    <button id="vy-confirm-start" type="submit" form="vy-confirm-form" class="vy-btn vy-btn--primary" disabled={!ready}>
      {#if busy === "start"}<span class="vy-spinner" aria-hidden="true"></span>{:else}<Icon name="download" size={15} />{/if}
      {t("confirm.start")}
    </button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: grid;
    /* One column that may be narrower than its longest unbreakable text
       (a long host or file name), which then truncates. */
    grid-template-columns: minmax(0, 1fr);
    gap: var(--vy-space-4);
  }
  .preview {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    padding: var(--vy-space-3);
    border-radius: var(--vy-radius-md);
    background: var(--vy-color-bg);
    border: 1px solid var(--vy-color-border);
  }
  .facts {
    /* Takes the rest of the row and may shrink, so a long host name is
       cut off with an ellipsis instead of widening the dialog. */
    flex: 1 1 auto;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--vy-space-1) var(--vy-space-3);
    min-inline-size: 0;
  }
  .size {
    font-size: var(--vy-font-size-lg);
    font-weight: var(--vy-font-weight-semibold);
  }
  .resumable {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1);
    color: var(--vy-color-success-text);
    font-size: var(--vy-font-size-sm);
  }
  .resumable.no {
    color: var(--vy-color-warning-text);
  }
  .from {
    flex: 1 1 100%;
    min-inline-size: 0;
    max-inline-size: 100%;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .two {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 180px;
    gap: var(--vy-space-3);
  }
  @container vy-app (max-width: 560px) {
    .two {
      grid-template-columns: 1fr;
    }
  }
  .folder-row {
    display: flex;
    gap: var(--vy-space-2);
  }
  .folder-row .with-icon {
    flex: 1;
    min-inline-size: 0;
  }
  .queue {
    max-inline-size: 280px;
  }
  .with-icon {
    position: relative;
    display: flex;
    align-items: center;
    color: var(--vy-color-text-subtle);
  }
  .with-icon > :global(.icon) {
    position: absolute;
    inset-inline-start: var(--vy-space-2-5);
    pointer-events: none;
  }
  .with-icon input {
    padding-inline-start: var(--vy-space-8);
    color: var(--vy-color-text);
  }
  .position {
    align-self: center;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .cancel {
    margin-inline-end: auto;
  }
</style>
