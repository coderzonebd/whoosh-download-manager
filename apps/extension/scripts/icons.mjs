// SPDX-License-Identifier: GPL-3.0-or-later
// Draws the toolbar icon (a down arrow on a rounded square) as PNG, so the
// repository carries no binary images. Placeholder art until the brand exists.

import { crc32, deflateSync } from "node:zlib";

const BG = [37, 99, 235];
const FG = [255, 255, 255];

function insideRoundedRect(x, y, margin, radius) {
  const lo = margin + radius;
  const hi = 1 - margin - radius;
  const cx = Math.min(Math.max(x, lo), hi);
  const cy = Math.min(Math.max(y, lo), hi);
  return (x - cx) ** 2 + (y - cy) ** 2 <= radius ** 2 && x >= margin && x <= 1 - margin && y >= margin && y <= 1 - margin;
}

function insideArrow(x, y) {
  const shaft = x >= 0.42 && x <= 0.58 && y >= 0.18 && y <= 0.52;
  // Head: triangle (0.24,0.48) (0.76,0.48) (0.5,0.76).
  const head = y >= 0.48 && y <= 0.76 && Math.abs(x - 0.5) <= ((0.76 - y) / 0.28) * 0.26;
  const tray = x >= 0.24 && x <= 0.76 && y >= 0.81 && y <= 0.87;
  return shaft || head || tray;
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
  const samples = 4;
  const rows = [];
  for (let py = 0; py < size; py++) {
    const row = Buffer.alloc(1 + size * 4); // filter byte 0, then RGBA
    for (let px = 0; px < size; px++) {
      let bg = 0;
      let fg = 0;
      for (let sy = 0; sy < samples; sy++) {
        for (let sx = 0; sx < samples; sx++) {
          const x = (px + (sx + 0.5) / samples) / size;
          const y = (py + (sy + 0.5) / samples) / size;
          if (!insideRoundedRect(x, y, 0.04, 0.2)) continue;
          if (insideArrow(x, y)) fg++;
          else bg++;
        }
      }
      const total = samples * samples;
      const alpha = (bg + fg) / total;
      const mix = bg + fg === 0 ? 0 : fg / (bg + fg);
      const o = 1 + px * 4;
      for (let c = 0; c < 3; c++) row[o + c] = Math.round(BG[c] * (1 - mix) + FG[c] * mix);
      row[o + 3] = Math.round(alpha * 255);
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
