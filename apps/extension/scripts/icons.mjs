// SPDX-License-Identifier: GPL-3.0-or-later
// Draws the toolbar icon, the Whoosh logo (apps/desktop/src-tauri/icons/whoosh.svg:
// three gusts of wind on a blue-to-violet rounded square), as PNG, so the
// repository carries no binary images. Keep the two in step.

import { crc32, deflateSync } from "node:zlib";

// whoosh.svg, in its 1024 x 1024 box.
const RECT = { lo: 64, hi: 960, radius: 208 };
const FROM = [0x2f, 0x74, 0xec];
const TO = [0x6a, 0x4b, 0xd8];
const FG = [255, 255, 255];
// The strokes are drawn in a 24-unit box: translate(152 197) scale(30).
const OFFSET = [152, 197];
const SCALE = 30;
const HALF_WIDTH = 2.3 / 2;
const LINES = [
  [3, 8.5, 13.5, 8.5],
  [3, 12.5, 18, 12.5],
  [3, 16.5, 9.5, 16.5],
];
// Each arc is a three-quarter circle; `gap` names the quarter it leaves out.
const ARCS = [
  { cx: 13.5, cy: 5.5, r: 3, gap: (dx, dy) => dx < 0 && dy > 0 },
  { cx: 18, cy: 15.5, r: 3, gap: (dx, dy) => dx < 0 && dy < 0 },
];

function insideRoundedRect(x, y) {
  const { lo, hi, radius } = RECT;
  if (x < lo || x > hi || y < lo || y > hi) return false;
  const cx = Math.min(Math.max(x, lo + radius), hi - radius);
  const cy = Math.min(Math.max(y, lo + radius), hi - radius);
  return (x - cx) ** 2 + (y - cy) ** 2 <= radius ** 2;
}

function toSegment(x, y, [x1, y1, x2, y2]) {
  const dx = x2 - x1;
  const dy = y2 - y1;
  const t = Math.min(1, Math.max(0, ((x - x1) * dx + (y - y1) * dy) / (dx * dx + dy * dy)));
  return Math.hypot(x - (x1 + t * dx), y - (y1 + t * dy));
}

function toArc(x, y, { cx, cy, r, gap }) {
  const dx = x - cx;
  const dy = y - cy;
  // Outside the drawn three quarters, the nearest point is an end of the arc,
  // and those ends are where the lines start, so the lines cover it.
  return gap(dx, dy) ? Infinity : Math.abs(Math.hypot(dx, dy) - r);
}

/** Whether a point of the 1024 box falls on a white stroke (round caps). */
function onStroke(x, y) {
  const u = (x - OFFSET[0]) / SCALE;
  const v = (y - OFFSET[1]) / SCALE;
  for (const line of LINES) if (toSegment(u, v, line) <= HALF_WIDTH) return true;
  for (const arc of ARCS) if (toArc(u, v, arc) <= HALF_WIDTH) return true;
  return false;
}

function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, "ascii");
  data.copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}

/** @param {number} size */
export function iconPng(size) {
  const samples = 8;
  const rows = [];
  for (let py = 0; py < size; py++) {
    const row = Buffer.alloc(1 + size * 4); // filter byte 0, then RGBA
    for (let px = 0; px < size; px++) {
      let inside = 0;
      let fg = 0;
      const sum = [0, 0, 0];
      for (let sy = 0; sy < samples; sy++) {
        for (let sx = 0; sx < samples; sx++) {
          const x = ((px + (sx + 0.5) / samples) / size) * 1024;
          const y = ((py + (sy + 0.5) / samples) / size) * 1024;
          if (!insideRoundedRect(x, y)) continue;
          inside++;
          if (onStroke(x, y)) {
            fg++;
            continue;
          }
          // The gradient runs from the top-left corner to the bottom-right.
          const t = (x - RECT.lo + (y - RECT.lo)) / (2 * (RECT.hi - RECT.lo));
          for (let c = 0; c < 3; c++) sum[c] += FROM[c] + (TO[c] - FROM[c]) * t;
        }
      }
      const o = 1 + px * 4;
      if (inside > 0) {
        for (let c = 0; c < 3; c++) row[o + c] = Math.round((sum[c] + FG[c] * fg) / inside);
      }
      row[o + 3] = Math.round((inside / (samples * samples)) * 255);
    }
    rows.push(row);
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(size, 0);
  header.writeUInt32BE(size, 4);
  header[8] = 8; // bit depth
  header[9] = 6; // RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.concat(rows), { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
