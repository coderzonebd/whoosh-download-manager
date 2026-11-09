<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  interface Props {
    /** Samples, oldest first. */
    values: readonly number[];
    /** How many samples fill the width (older ones scroll off to the start). */
    capacity: number;
    label: string;
  }
  let { values, capacity, label }: Props = $props();

  const W = 300;
  const H = 56;
  const PAD = 3;

  const max = $derived(Math.max(1, ...values) * 1.12);
  const points = $derived.by(() => {
    const n = values.length;
    if (n === 0) return "";
    const step = W / Math.max(1, capacity - 1);
    const offset = (capacity - n) * step;
    return values.map((v, i) => `${(offset + i * step).toFixed(1)},${(H - PAD - (v / max) * (H - PAD * 2)).toFixed(1)}`).join(" ");
  });
  const area = $derived.by(() => {
    if (!points) return "";
    const first = points.split(" ")[0]?.split(",")[0] ?? "0";
    return `M${first},${H} L${points.replaceAll(" ", " L")} L${W},${H} Z`;
  });
</script>

<svg class="spark" viewBox="0 0 {W} {H}" preserveAspectRatio="none" role="img" aria-label={label}>
  <line x1="0" x2={W} y1={H / 2} y2={H / 2} class="grid" />
  {#if points}
    <path d={area} class="area" />
    <polyline points={points} class="line" />
  {/if}
</svg>

<style>
  .spark {
    display: block;
    inline-size: 100%;
    block-size: 56px;
    overflow: visible;
  }
  :global([dir="rtl"]) .spark {
    /* time flows with reading direction */
    transform: scaleX(-1);
  }
  .grid {
    stroke: var(--vy-color-border);
    stroke-dasharray: 3 4;
    vector-effect: non-scaling-stroke;
  }
  .area {
    fill: color-mix(in srgb, var(--vy-color-accent) 16%, transparent);
  }
  .line {
    fill: none;
    stroke: var(--vy-color-accent);
    stroke-width: 1.75;
    stroke-linejoin: round;
    stroke-linecap: round;
    vector-effect: non-scaling-stroke;
  }
</style>
