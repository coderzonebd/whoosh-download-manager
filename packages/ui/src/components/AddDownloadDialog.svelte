<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  URL → probe → confirm. The link is checked as you type (debounced), so by
  the time you look up the file name, size and resumability are there; the
  name and category follow the probe until you edit them yourself.
-->
<script lang="ts">
  import type { ProbeResult } from "@whoosh/api-client";
  import { CATEGORIES, categoryForFilename, type Category } from "@whoosh/api-client/categories";
  import { getApp } from "../lib/state/app.svelte.ts";
  import Dialog from "./Dialog.svelte";
  import FileIcon from "./FileIcon.svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    initialUrl?: string;
    onclose: () => void;
  }
  let { initialUrl = "", onclose }: Props = $props();

  const app = getApp();
  const { i18n } = app;
  const { t } = i18n;

  type Probe =
    | { status: "idle" }
    | { status: "checking"; url: string }
    | { status: "ok"; url: string; result: ProbeResult }
    | { status: "error"; url: string; message: string };

  let url = $state(untrackInitial());
  let filename = $state("");
  let filenameEdited = $state(false);
  let directory = $state(app.settings?.defaultDirectory ?? "");
  let category = $state<Category>("other");
  let categoryEdited = $state(false);
  let startNow = $state(true);
  let probe = $state<Probe>({ status: "idle" });
  let submitting = $state(false);
  let submitError = $state<string | undefined>(undefined);
  let touched = $state(false);
  let seq = 0;

  function untrackInitial(): string {
    return initialUrl;
  }

  function isUrl(s: string): boolean {
    try {
      const u = new URL(s);
      return ["http:", "https:", "ftp:"].includes(u.protocol) && u.hostname !== "";
    } catch {
      return false;
    }
  }

  const trimmed = $derived(url.trim());
  const valid = $derived(isUrl(trimmed));
  const showInvalid = $derived(touched && trimmed !== "" && !valid);

  async function runProbe(target: string): Promise<void> {
    const mine = ++seq;
    probe = { status: "checking", url: target };
    try {
      const result = await app.client.call("download.probe", { url: target });
      if (mine !== seq) return;
      probe = { status: "ok", url: target, result };
      if (!filenameEdited) filename = result.filename;
      if (!categoryEdited) category = categoryForFilename(result.filename);
    } catch (e) {
      if (mine !== seq) return;
      probe = { status: "error", url: target, message: app.describe(e) };
    }
  }

  // Check the link shortly after typing stops.
  $effect(() => {
    const target = trimmed;
    if (!isUrl(target)) {
      seq++;
      probe = { status: "idle" };
      return;
    }
    const timer = setTimeout(() => void runProbe(target), 350);
    return () => clearTimeout(timer);
  });

  // The default folder arrives with settings; fill it in if the user hasn't typed one.
  $effect(() => {
    const dir = app.settings?.defaultDirectory;
    if (dir && directory === "") directory = dir;
  });

  const ready = $derived(valid && probe.status === "ok" && probe.url === trimmed && !submitting);

  async function submit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    touched = true;
    if (!ready) return;
    submitting = true;
    submitError = undefined;
    try {
      const params: Parameters<typeof app.add>[0] = { url: trimmed, start: startNow, category };
      if (filename.trim()) params.filename = filename.trim();
      if (directory.trim()) params.directory = directory.trim();
      const id = await app.add(params);
      app.toasts.push({ tone: "success", message: t("add.added", { name: app.store.get(id)?.data.filename ?? filename }) }, 3000);
      onclose();
      app.reveal(id);
    } catch (err) {
      submitError = t("add.failed", { message: app.describe(err) });
    } finally {
      submitting = false;
    }
  }

  const result = $derived(probe.status === "ok" ? probe.result : undefined);
</script>

<Dialog title={t("add.title")} description={t("add.description")} closeLabel={t("action.close")} {onclose} initialFocus="#vy-add-url">
  <form id="vy-add-form" class="form" onsubmit={submit} novalidate>
    <div class="vy-field">
      <label class="vy-label" for="vy-add-url">{t("add.url")}</label>
      <div class="url-row">
        <input
          id="vy-add-url"
          class="vy-input"
          type="url"
          inputmode="url"
          autocomplete="off"
          spellcheck="false"
          dir="ltr"
          placeholder={t("add.urlPlaceholder")}
          bind:value={url}
          onblur={() => (touched = true)}
          aria-invalid={showInvalid || probe.status === "error"}
          aria-describedby="vy-add-url-status"
        />
      </div>
      <div id="vy-add-url-status" class="status" aria-live="polite">
        {#if showInvalid}
          <span class="vy-error">{t("add.invalidUrl")}</span>
        {:else if probe.status === "checking"}
          <span class="checking"><span class="vy-spinner" aria-hidden="true"></span>{t("add.checking")}</span>
        {:else if probe.status === "error"}
          <span class="vy-error">{t("add.probeFailed", { message: probe.message })}</span>
          <button type="button" class="vy-btn vy-btn--sm" onclick={() => probe.status === "error" && runProbe(probe.url)}>
            <Icon name="refresh" size={13} />{t("add.retry")}
          </button>
        {/if}
      </div>
    </div>

    {#if result}
      <div class="preview">
        <FileIcon category={category} size="lg" />
        <div class="facts">
          <span class="vy-num size">{result.totalBytes === undefined ? t("add.unknownSize") : i18n.bytes(result.totalBytes)}</span>
          <span class="resumable" class:no={!result.resumable}>
            <Icon name={result.resumable ? "check" : "alert"} size={13} />
            {result.resumable ? t("add.resumable") : t("add.notResumable")}
          </span>
          {#if result.contentType}<span class="type">{result.contentType}</span>{/if}
        </div>
      </div>

      <div class="vy-field">
        <label class="vy-label" for="vy-add-name">{t("add.filename")}</label>
        <input id="vy-add-name" class="vy-input" dir="auto" bind:value={filename} oninput={() => (filenameEdited = true)} />
      </div>
      <div class="two">
        <div class="vy-field">
          <label class="vy-label" for="vy-add-dir">{t("add.folder")}</label>
          <div class="with-icon">
            <Icon name="folder" size={15} />
            <input id="vy-add-dir" class="vy-input" dir="auto" bind:value={directory} />
          </div>
        </div>
        <div class="vy-field">
          <label class="vy-label" for="vy-add-cat">{t("add.category")}</label>
          <select id="vy-add-cat" class="vy-select" bind:value={category} onchange={() => (categoryEdited = true)}>
            {#each CATEGORIES as c (c)}
              <option value={c}>{t(`category.${c}`)}</option>
            {/each}
          </select>
        </div>
      </div>
    {/if}

    {#if submitError}<p class="vy-error" role="alert">{submitError}</p>{/if}
  </form>

  {#snippet footer()}
    <label class="vy-check start">
      <input type="checkbox" bind:checked={startNow} />
      {t("add.startNow")}
    </label>
    <button type="button" class="vy-btn vy-btn--secondary" onclick={onclose}>{t("action.cancel")}</button>
    <button type="submit" form="vy-add-form" class="vy-btn vy-btn--primary" disabled={!ready}>
      {#if submitting}<span class="vy-spinner" aria-hidden="true"></span>{:else}<Icon name="download" size={15} />{/if}
      {startNow ? t("add.submit") : t("add.submitPaused")}
    </button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: grid;
    gap: var(--vy-space-4);
  }
  .status {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2);
    min-block-size: 1.5em;
    font-size: var(--vy-font-size-sm);
  }
  .checking {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-2);
    color: var(--vy-color-text-muted);
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
  .type {
    color: var(--vy-color-text-muted);
    font-family: var(--vy-font-mono);
    font-size: var(--vy-font-size-xs);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 180px;
    gap: var(--vy-space-3);
  }
  @container vy-app (max-width: 560px) {
    .two {
      grid-template-columns: 1fr;
    }
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
  .start {
    margin-inline-end: auto;
    color: var(--vy-color-text-muted);
  }
</style>
