// SPDX-License-Identifier: Apache-2.0
// Per-segment progress between polls. Progress events (4 Hz) carry only the
// download's total; segment detail comes from `download.get` about once a
// second. To keep segmented bars moving smoothly we distribute the bytes
// gained since the last snapshot over the open segments, weighted by each
// segment's last known speed. The total is always exact; the split is an
// estimate corrected on the next poll.

import type { SegmentInfo } from "@whoosh/api-client";

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
    let a = f.start;
    const b = f.start + f.done;
    while (a < b) {
      const i = Math.min(blocks - 1, Math.floor(a / size));
      const blockEnd = (i + 1) * size;
      const take = Math.min(b, blockEnd) - a;
      out[i] = (out[i] ?? 0) + take / size;
      a += take;
      if (take <= 0) break;
    }
  }
  return out.map((v) => Math.min(1, v));
}
