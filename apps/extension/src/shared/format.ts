// SPDX-License-Identifier: GPL-3.0-or-later

/** 1536 → "1.5 KiB". Binary units, one decimal below 10. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  const digits = unit === 0 || value >= 10 ? 0 : 1;
  return `${value.toFixed(digits)} ${units[unit]}`;
}
