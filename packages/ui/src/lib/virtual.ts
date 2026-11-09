// SPDX-License-Identifier: Apache-2.0
// Fixed-row-height virtualisation maths. Pure, so it is unit-tested directly.

export interface Range {
  /** First rendered index (inclusive). */
  start: number;
  /** Last rendered index (exclusive). */
  end: number;
}

export function visibleRange(scrollTop: number, viewport: number, rowHeight: number, count: number, overscan = 6): Range {
  if (count <= 0 || rowHeight <= 0) return { start: 0, end: 0 };
  const first = Math.floor(Math.max(0, scrollTop) / rowHeight);
  const visible = Math.ceil(Math.max(0, viewport) / rowHeight) + 1;
  const start = Math.max(0, Math.min(count - 1, first) - overscan);
  const end = Math.min(count, first + visible + overscan);
  return { start, end };
}

/**
 * The scrollTop that brings row `index` fully into view, or undefined if it
 * already is. `header` is the sticky header's height inside the scroller.
 */
export function scrollTopFor(index: number, scrollTop: number, viewport: number, rowHeight: number, header = 0): number | undefined {
  const top = index * rowHeight;
  const bottom = top + rowHeight;
  const visibleTop = scrollTop;
  const visibleBottom = scrollTop + Math.max(0, viewport - header);
  if (top < visibleTop) return top;
  if (bottom > visibleBottom) return bottom - Math.max(rowHeight, viewport - header);
  return undefined;
}

/** Rows per page for PageUp/PageDown. */
export function pageSize(viewport: number, rowHeight: number, header = 0): number {
  return Math.max(1, Math.floor((viewport - header) / rowHeight) - 1);
}

/** A valid, unique DOM id for a row, whatever characters the daemon's opaque id contains. */
export function rowDomId(id: string): string {
  return `vy-row-${id.replace(/[^A-Za-z0-9_-]/g, (ch) => `_${ch.charCodeAt(0).toString(16)}`)}`;
}
