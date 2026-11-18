// SPDX-License-Identifier: Apache-2.0
// Per-segment progress between polls. Progress events (4 Hz) carry only the
// download's total; segment detail comes from `download.get` about once a
// second. To keep segmented bars moving smoothly we distribute the bytes
// gained since the last snapshot over the open segments, weighted by each
// segment's last known speed. The total is always exact; the split is an
// estimate corrected on the next poll.

import type { SegmentInfo } from "@wdm/api-client";

export interface SegmentFill {
  start: number;
  end: number;
  /** Bytes of this segment that are on disk. */
  done: number;
}

export function estimateSegments(segments: readonly SegmentInfo[], currentDownloaded: number): SegmentFill[] {
  const fills = segments.map((s) => ({ start: s.start, end: s.end, done: Math.min(s.downloadedBytes, s.end - s.start) }));
  const snapshot = fills.reduce((a, f) => a + f.done, 0);
  let delta = currentDownloaded - snapshot;
  if (delta <= 0) return fills;

  // Two passes: segments that fill up hand their surplus to the others.
  for (let pass = 0; pass < 2 && delta > 0; pass++) {
    const open = fills.map((f, i) => ({ f, w: Math.max(1, segments[i]?.speedBps ?? 0) })).filter(({ f }) => f.done < f.end - f.start);
    const weight = open.reduce((a, o) => a + o.w, 0);
    if (weight === 0) break;
    let used = 0;
    for (const { f, w } of open) {
      const room = f.end - f.start - f.done;
      const add = Math.min(room, Math.floor((delta * w) / weight));
      f.done += add;
      used += add;
    }
    delta -= used;
  }
  return fills;
}

/**
 * Coverage of `blocks` equal slices of the file by downloaded bytes, 0…1 each.
 * Drives the detail panel's segment map.
 */
export function blockCoverage(fills: readonly SegmentFill[], total: number, blocks: number): number[] {
  const out = new Array<number>(blocks).fill(0);
  if (total <= 0 || blocks <= 0) return out;
  const size = total / blocks;
  for (const f of fills) {
    const a = Math.max(0, f.start);
    const b = Math.min(total, f.start + f.done);
    if (b <= a) continue;
    // Each block gets its overlap with [a, b), measured from the block's own
    // edges: summing steps of a float `size` drifts on large files and could
    // stall after a few blocks.
    const first = Math.min(blocks - 1, Math.floor(a / size));
    const last = Math.min(blocks - 1, Math.floor((b - 1) / size));
    for (let i = first; i <= last; i++) {
      const lo = i * size;
      const hi = i === blocks - 1 ? total : (i + 1) * size;
      const overlap = Math.min(b, hi) - Math.max(a, lo);
      if (overlap > 0) out[i] = (out[i] ?? 0) + overlap / (hi - lo);
    }
  }
  return out.map((v) => Math.min(1, v));
}
