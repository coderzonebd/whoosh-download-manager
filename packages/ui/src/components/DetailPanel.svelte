<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import { getApp } from "../lib/state/app.svelte.ts";
  import { HISTORY_LENGTH, PAUSABLE, RESUMABLE, type Download } from "../lib/state/downloads.svelte.ts";
  import { blockCoverage, estimateSegments } from "../lib/segments.ts";
  import FileIcon from "./FileIcon.svelte";
  import Icon from "./Icon.svelte";
  import SegmentBar from "./SegmentBar.svelte";
  import Sparkline from "./Sparkline.svelte";
  import StatusBadge from "./StatusBadge.svelte";

  interface Props {
    download: Download;
  }
  let { download }: Props = $props();

  const app = getApp();
  const { i18n, prefs } = app;
  const { t } = i18n;

  const d = $derived(download.data);
  const snapshot = $derived(app.segments.get(download.id));
  const moving = $derived(d.state === "downloading");
  const frac = $derived(download.fraction);

  $effect(() => {
    app.segments.setFocus(download.id);
    return () => app.segments.setFocus(undefined);
  });

  const fills = $derived.by(() => {
    const total = d.totalBytes;
    if (d.state === "completed" && total !== undefined) return [{ start: 0, end: total, done: total }];
    if (!snapshot || total === undefined) return undefined;
    return estimateSegments(snapshot.detail.segments, d.downloadedBytes);
  });
  const BLOCKS = 200;
  const blocks = $derived(fills && d.totalBytes ? blockCoverage(fills, d.totalBytes, BLOCKS) : undefined);

  // `history` is a plain array; reading `d` makes this re-run on each progress tick.
  const history = $derived.by(() => {
    void d;
    return [...download.history];
  });
  const peak = $derived(history.length ? Math.max(...history) : 0);

  let copied = $state(false);
</script>

<aside class="panel" aria-labelledby="vy-detail-title">
  <header class="head">
    <FileIcon category={download.category} size="lg" />
    <div class="title">
      <h2 id="vy-detail-title" dir="auto">{d.filename}</h2>
      <StatusBadge state={d.state} variant="pill" step={d.media?.step} />
    </div>
    <button class="vy-btn vy-btn--icon vy-btn--sm" aria-label={t("detail.close")} title={t("detail.close")} onclick={() => (prefs.detailOpen = false)}>
      <Icon name="x" size={15} />
    </button>
  </header>

  <div class="scroll">
    <section class="progress">
      <SegmentBar {download} size="lg" />
      <p class="vy-num">{i18n.progress(d.downloadedBytes, d.totalBytes)}</p>
      <div class="actions">
        {#if PAUSABLE.has(d.state)}
          <button class="vy-btn vy-btn--secondary vy-btn--sm" onclick={() => app.pause([download.id])}><Icon name="pause" size={14} />{t("action.pause")}</button>
        {:else if d.state === "needs_refresh"}
          <button class="vy-btn vy-btn--primary vy-btn--sm" onclick={() => app.refreshLink(download.id)}><Icon name="refresh" size={14} />{t("action.refreshLink")}</button>
        {:else if RESUMABLE.has(d.state)}
          <button class="vy-btn vy-btn--secondary vy-btn--sm" onclick={() => app.resume([download.id])}>
            <Icon name={d.state === "failed" ? "refresh" : "play"} size={14} />{t(d.state === "failed" ? "action.retry" : "action.resume")}
          </button>
        {/if}
        <button class="vy-btn vy-btn--secondary vy-btn--sm" onclick={() => app.requestRemove([download.id])}><Icon name="trash" size={14} />{t("action.remove")}</button>
      </div>
    </section>

    {#if d.error}
      <div class="problem" class:warning={d.state === "needs_refresh"} role="note">
        <Icon name="alert" size={16} />
        <div>
          <p>{d.error.message}</p>
          <p class="code">{t("detail.errorCode", { code: d.error.code })}</p>
        </div>
      </div>
    {/if}

    <dl class="facts">
      <div><dt>{t("detail.speed")}</dt><dd class="vy-num">{moving ? i18n.speed(d.speedBps) : "—"}</dd></div>
      <div><dt>{t("detail.eta")}</dt><dd class="vy-num">{moving ? i18n.eta(d.etaSecs) : "—"}</dd></div>
      <div><dt>{t("detail.connections")}</dt><dd class="vy-num">{moving ? i18n.number(d.connections) : "—"}</dd></div>
      <div><dt>{t("detail.size")}</dt><dd class="vy-num">{i18n.bytes(d.totalBytes)}</dd></div>
      <div class="wide">
        <dt>{t("detail.resumable")}</dt>
        <dd>{d.resumable ? t("detail.resumable.yes") : t("detail.resumable.no")}</dd>
      </div>
      <div class="wide"><dt>{t("detail.savedTo")}</dt><dd class="path" dir="auto">{d.directory}/{d.filename}</dd></div>
      <div class="wide">
        <dt>{t("detail.source")}</dt>
        <dd class="url">
          <span class="vy-truncate" dir="ltr" title={d.url}>{d.url}</span>
          <button
            class="vy-btn vy-btn--icon vy-btn--sm"
            aria-label={t("action.copyLink")}
            title={t("action.copyLink")}
            onclick={async () => {
              await app.copyLink(download.id);
              copied = true;
              setTimeout(() => (copied = false), 1500);
            }}
          >
            <Icon name={copied ? "check" : "copy"} size={14} />
          </button>
        </dd>
      </div>
      <div><dt>{t("detail.added")}</dt><dd class="vy-num">{i18n.date(d.addedAt)}</dd></div>
      {#if d.completedAt}<div><dt>{t("detail.finished")}</dt><dd class="vy-num">{i18n.date(d.completedAt)}</dd></div>{/if}
    </dl>

    {#if d.state !== "completed"}
      <section>
        <div class="section-head">
          <h3>{t("detail.speedHistory")}</h3>
          {#if peak > 0}<span class="vy-num hint">{t("detail.peak", { speed: i18n.speed(peak) })}</span>{/if}
        </div>
        <Sparkline values={history} capacity={HISTORY_LENGTH} label={`${t("detail.speedHistory")}: ${i18n.speed(d.speedBps)}`} />
      </section>
    {/if}

    <section>
      <div class="section-head">
        <h3>{t("detail.segmentMap")}</h3>
        {#if frac !== undefined}<span class="vy-num hint">{i18n.percent(frac)}</span>{/if}
      </div>
      {#if blocks}
        <div class="map" role="img" aria-label={`${t("detail.segmentMap")}: ${i18n.progress(d.downloadedBytes, d.totalBytes)}`}>
          {#each blocks as v, i (i)}
            <span class="block" class:full={v >= 0.999} class:part={v > 0 && v < 0.999}></span>
          {/each}
        </div>
      {:else}
        <p class="hint">{t("detail.segmentsNone")}</p>
      {/if}
    </section>

    {#if snapshot && fills && d.state !== "completed" && d.totalBytes}
      <section>
        <h3>{t("detail.segments")}</h3>
        <table class="segs">
          <thead class="vy-sr-only">
            <tr><th>#</th><th>{t("col.progress")}</th><th>{t("col.speed")}</th></tr>
          </thead>
          <tbody>
            {#each fills as f, i (f.start)}
              {@const len = f.end - f.start}
              {@const seg = snapshot.detail.segments[i]}
              <tr>
                <th scope="row" class="vy-num">{i18n.number(i + 1)}</th>
                <td>
                  <span class="mini" aria-hidden="true"><span style:transform="scaleX({len > 0 ? f.done / len : 0})"></span></span>
                  <span class="vy-sr-only">{i18n.percent(len > 0 ? f.done / len : 0)}</span>
                </td>
                <td class="vy-num">{seg && seg.speedBps > 0 && moving ? i18n.speed(seg.speedBps) : i18n.bytes(len)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>
    {/if}
  </div>
</aside>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    inline-size: 340px;
    flex: none;
    min-block-size: 0;
    border-inline-start: 1px solid var(--vy-color-border);
    background: var(--vy-color-surface);
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: var(--vy-space-3);
    padding: var(--vy-space-4);
    padding-block-end: var(--vy-space-3);
  }
  .title {
    display: grid;
    justify-items: start;
    gap: var(--vy-space-1-5);
    flex: 1;
    min-inline-size: 0;
  }
  h2 {
    font-size: var(--vy-font-size-lg);
    font-weight: var(--vy-font-weight-semibold);
    line-height: var(--vy-line-height-tight);
    overflow-wrap: anywhere;
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    padding-inline: var(--vy-space-4);
    padding-block-end: var(--vy-space-6);
    display: grid;
    align-content: start;
    gap: var(--vy-space-5);
  }
  .progress {
    display: grid;
    gap: var(--vy-space-2);
  }
  .progress p {
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--vy-space-2);
  }
  .problem {
    display: flex;
    gap: var(--vy-space-2);
    padding: var(--vy-space-3);
    border-radius: var(--vy-radius-md);
    background: var(--vy-color-danger-subtle);
    color: var(--vy-color-danger-text);
  }
  .problem.warning {
    background: var(--vy-color-warning-subtle);
    color: var(--vy-color-warning-text);
  }
  .problem .code {
    margin-block-start: 2px;
    font-family: var(--vy-font-mono);
    font-size: var(--vy-font-size-xs);
    opacity: 0.85;
  }
  .facts {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--vy-space-3) var(--vy-space-4);
    margin: 0;
  }
  .facts > div {
    min-inline-size: 0;
  }
  .facts .wide {
    grid-column: 1 / -1;
  }
  dt {
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-xs);
  }
  dd {
    margin: 0;
    font-weight: var(--vy-font-weight-medium);
  }
  .path {
    overflow-wrap: anywhere;
    font-weight: var(--vy-font-weight-regular);
  }
  .url {
    display: flex;
    align-items: center;
    gap: var(--vy-space-1);
    font-weight: var(--vy-font-weight-regular);
    color: var(--vy-color-accent-text);
  }
  .section-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--vy-space-2);
    margin-block-end: var(--vy-space-2);
  }
  h3 {
    font-size: var(--vy-font-size-sm);
    font-weight: var(--vy-font-weight-semibold);
  }
  section > h3 {
    margin-block-end: var(--vy-space-2);
  }
  .hint {
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .map {
    display: grid;
    grid-template-columns: repeat(25, 1fr);
    gap: 2px;
  }
  .block {
    aspect-ratio: 1;
    border-radius: 2px;
    background: var(--vy-color-progress-track);
  }
  .block.part {
    background: color-mix(in srgb, var(--vy-color-accent) 45%, var(--vy-color-progress-track));
  }
  .block.full {
    background: var(--vy-color-accent);
  }
  .segs {
    inline-size: 100%;
    border-collapse: collapse;
    font-size: var(--vy-font-size-sm);
  }
  .segs th,
  .segs td {
    padding-block: 3px;
    padding-inline: 0;
    text-align: start;
    font-weight: var(--vy-font-weight-regular);
  }
  .segs th {
    inline-size: 2.2em;
    color: var(--vy-color-text-muted);
  }
  .segs td:last-child {
    inline-size: 7.5em;
    text-align: end;
    color: var(--vy-color-text-muted);
  }
  .mini {
    display: block;
    block-size: 6px;
    margin-inline-end: var(--vy-space-3);
    border-radius: var(--vy-radius-full);
    background: var(--vy-color-progress-track);
    overflow: hidden;
  }
  .mini > span {
    display: block;
    block-size: 100%;
    background: var(--vy-color-accent);
    transform-origin: left center;
    transition: transform var(--vy-duration-progress) linear;
  }
  :global([dir="rtl"]) .mini > span {
    transform-origin: right center;
  }

  @container vy-app (max-width: 900px) {
    .panel {
      position: absolute;
      inset-block: 0;
      inset-inline-end: 0;
      z-index: var(--vy-z-overlay);
      inline-size: min(360px, 92cqi);
      box-shadow: var(--vy-shadow-lg);
    }
  }
</style>
