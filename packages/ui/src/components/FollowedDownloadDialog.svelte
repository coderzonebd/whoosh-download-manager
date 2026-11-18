<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  The prompt window after "Start download", like IDM's progress window and
  its "Download complete" dialog: one download the prompt window follows
  (`AppState.followed`).

  While it runs: its progress, speed and time left, with Pause/Resume and
  Hide (the window goes; the download goes on, and the window comes back
  when it ends). When it ends: complete (Open, Open folder, Close) or failed
  (the reason, Retry, Close).
-->
<script lang="ts">
  import { getApp, ENDED } from "../lib/state/app.svelte.ts";
  import { PAUSABLE, RESUMABLE, type Download } from "../lib/state/downloads.svelte.ts";
  import Dialog from "./Dialog.svelte";
  import FileIcon from "./FileIcon.svelte";
  import Icon from "./Icon.svelte";
  import SegmentBar from "./SegmentBar.svelte";
  import StatusBadge from "./StatusBadge.svelte";

  interface Props {
    download: Download;
  }
  let { download }: Props = $props();

  const app = getApp();
  const { i18n } = app;
  const { t } = i18n;

  const d = $derived(download.data);
  const ended = $derived(ENDED.has(d.state));
  const moving = $derived(d.state === "downloading");
  // Downloading with no connection open: waiting out a retry or the server (a stall at 99%).
  // Said plainly, instead of a "Downloading" that sits at 0 B/s.
  const waiting = $derived(moving && d.connections === 0);
  const canOpen = $derived(d.state === "completed" && app.host.openDownload !== undefined);
  const title = $derived(d.state === "completed" ? t("follow.complete") : d.state === "failed" ? t("follow.failed") : t("follow.title"));

  // Escape, the close button: a running download is hidden, an ended one is done with.
  function close(): void {
    if (ended) app.unfollow(download.id);
    else app.hideFollowed(download.id);
  }

  // Cancel takes two clicks: the first only arms it, so a stray click never throws away a
  // nearly finished download. It disarms after a few seconds.
  let cancelArmed = $state(false);
  let cancelling = $state(false);
  let disarm: ReturnType<typeof setTimeout> | undefined;
  async function cancel(): Promise<void> {
    if (!cancelArmed) {
      cancelArmed = true;
      clearTimeout(disarm);
      disarm = setTimeout(() => (cancelArmed = false), 4000);
      return;
    }
    clearTimeout(disarm);
    cancelling = true;
    const id = download.id;
    try {
      // Removes the download and its partial file; the removed event forgets it here.
      await app.cancelWaiting(id);
      app.unfollow(id);
    } catch (e) {
      app.report(e);
      cancelling = false;
      cancelArmed = false;
    }
  }
  $effect(() => () => clearTimeout(disarm));

  function open(reveal: boolean): void {
    app.openFile(download.id, reveal);
    app.unfollow(download.id);
  }
</script>

<Dialog {title} closeLabel={ended ? t("action.close") : t("follow.hide")} onclose={close} initialFocus={canOpen ? "#vy-follow-open" : "#vy-follow-main"}>
  <div class="follow">
    <div class="head">
      <FileIcon category={download.category} size="lg" />
      <div class="name">
        <p class="file" dir="auto" title={d.filename}>{d.filename}</p>
        <StatusBadge state={d.state} variant="pill" step={d.media?.step} />
      </div>
    </div>

    {#if !ended}
      <section class="progress" aria-live="polite">
        <SegmentBar {download} size="lg" />
        <p class="vy-num">{i18n.progress(d.downloadedBytes, d.totalBytes)}</p>
      </section>
      <dl class="facts">
        <div><dt>{t("detail.speed")}</dt><dd class="vy-num">{moving && !waiting ? i18n.speed(d.speedBps) : "—"}</dd></div>
        <div><dt>{t("detail.eta")}</dt><dd class="vy-num">{moving && !waiting ? i18n.eta(d.etaSecs) : "—"}</dd></div>
        <div><dt>{t("detail.connections")}</dt><dd class="vy-num">{moving ? i18n.number(d.connections) : "—"}</dd></div>
        <div><dt>{t("detail.size")}</dt><dd class="vy-num">{i18n.bytes(d.totalBytes)}</dd></div>
      </dl>
      {#if waiting}
        <p class="hint waiting" role="status"><span class="vy-spinner" aria-hidden="true"></span>{t("follow.waiting")}</p>
      {:else}
        <p class="hint">{t("follow.hint")}</p>
      {/if}
    {:else if d.state === "completed"}
      <dl class="facts outcome">
        <div><dt>{t("detail.size")}</dt><dd class="vy-num">{i18n.bytes(d.totalBytes ?? d.downloadedBytes)}</dd></div>
        {#if d.activeMs !== undefined}
          <div><dt>{t("detail.timeTaken")}</dt><dd class="vy-num">{i18n.eta(Math.max(1, Math.round(d.activeMs / 1000)))}</dd></div>
        {/if}
        <div class="wide"><dt>{t("detail.savedTo")}</dt><dd class="path" dir="auto">{d.directory}/{d.filename}</dd></div>
      </dl>
    {:else}
      <div class="problem outcome" role="alert">
        <Icon name="alert" size={16} />
        <p>{d.error?.message ?? t("follow.failed")}</p>
      </div>
    {/if}
  </div>

  {#snippet footer()}
    {#if !ended}
      <button
        type="button"
        class="vy-btn cancel {cancelArmed ? 'vy-btn--danger' : 'vy-btn--secondary'}"
        disabled={cancelling}
        aria-live="polite"
        onclick={cancel}
      >
        {cancelArmed ? t("follow.cancelAgain") : t("action.cancel")}
      </button>
      {#if PAUSABLE.has(d.state)}
        <button type="button" class="vy-btn vy-btn--secondary" onclick={() => app.pause([download.id])}><Icon name="pause" size={15} />{t("action.pause")}</button>
      {:else if RESUMABLE.has(d.state)}
        <button type="button" class="vy-btn vy-btn--secondary" onclick={() => app.resume([download.id])}><Icon name="play" size={15} />{t("action.resume")}</button>
      {/if}
      <button id="vy-follow-main" type="button" class="vy-btn vy-btn--primary" onclick={close}>{t("follow.hide")}</button>
    {:else if d.state === "completed"}
      <button id="vy-follow-main" type="button" class="vy-btn vy-btn--secondary" onclick={close}>{t("action.close")}</button>
      {#if canOpen}
        <button type="button" class="vy-btn vy-btn--secondary" onclick={() => open(true)}><Icon name="folder" size={15} />{t("follow.openFolder")}</button>
        <button id="vy-follow-open" type="button" class="vy-btn vy-btn--primary" onclick={() => open(false)}>{t("follow.open")}</button>
      {/if}
    {:else}
      <button id="vy-follow-main" type="button" class="vy-btn vy-btn--secondary" onclick={close}>{t("action.close")}</button>
      <button type="button" class="vy-btn vy-btn--primary" onclick={() => app.resume([download.id])}><Icon name="refresh" size={15} />{t("action.retry")}</button>
    {/if}
  {/snippet}
</Dialog>

<style>
  .follow {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--vy-space-4);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--vy-space-3);
    padding: var(--vy-space-3);
    border-radius: var(--vy-radius-md);
    background: var(--vy-color-bg);
    border: 1px solid var(--vy-color-border);
  }
  .name {
    display: grid;
    justify-items: start;
    gap: var(--vy-space-1);
    min-inline-size: 0;
  }
  .file {
    margin: 0;
    max-inline-size: 100%;
    font-weight: var(--vy-font-weight-semibold);
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .progress {
    display: grid;
    gap: var(--vy-space-2);
  }
  .progress p {
    margin: 0;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--vy-space-3) var(--vy-space-4);
    margin: 0;
  }
  .facts .wide {
    grid-column: 1 / -1;
  }
  .facts dt {
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-xs);
  }
  .facts dd {
    margin: 0;
  }
  .path {
    overflow-wrap: anywhere;
  }
  .waiting {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2);
    color: var(--vy-color-warning-text);
  }
  /* How it ended arrives rather than appears: a short rise and fade from where the progress was. */
  .outcome {
    transition:
      opacity 220ms var(--vy-ease-out),
      translate 220ms var(--vy-ease-out);
  }
  @starting-style {
    .outcome {
      opacity: 0;
      translate: 0 6px;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .outcome {
      transition: opacity 160ms ease;
      translate: none;
    }
  }
  .hint {
    margin: 0;
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .problem {
    display: flex;
    gap: var(--vy-space-2);
    align-items: flex-start;
    padding: var(--vy-space-3);
    border-radius: var(--vy-radius-md);
    background: var(--vy-color-danger-subtle);
    color: var(--vy-color-danger-text);
  }
  .cancel {
    margin-inline-end: auto;
  }
  .problem p {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>
