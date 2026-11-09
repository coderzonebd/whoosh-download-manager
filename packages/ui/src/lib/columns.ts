// SPDX-License-Identifier: Apache-2.0
import type { SortKey } from "./state/downloads.svelte.ts";
import { COLUMN_IDS, type ColumnId } from "./state/prefs.svelte.ts";

export interface ColumnDef {
  id: ColumnId;
  sort: SortKey;
  /** Direction on first click: numbers that matter most when big start descending. */
  firstDir: "asc" | "desc";
  numeric: boolean;
}

export const COLUMNS: readonly ColumnDef[] = COLUMN_IDS.map((id) => ({
  id,
  sort: id,
  firstDir: id === "name" || id === "status" || id === "eta" ? "asc" : "desc",
  numeric: id === "size" || id === "speed" || id === "eta" || id === "connections",
}));

export const ROW_HEIGHT = { classic: 30, modern: 76 } as const;
export const HEADER_HEIGHT = 32;
