// SPDX-License-Identifier: Apache-2.0
// Hand-drawn 24×24 stroke icons (original artwork, Apache-2.0). Drawn on a
// 1.75px stroke so they sit with 13px UI text.

const circle = "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z";

export const ICONS = {
  plus: "M12 5v14M5 12h14",
  play: "M7.5 5.6v12.8a.6.6 0 0 0 .92.5l10-6.4a.6.6 0 0 0 0-1L8.42 5.1a.6.6 0 0 0-.92.5z",
  pause: "M8.5 5.5v13M15.5 5.5v13",
  trash: "M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4.5h6V7",
  settings: "M4 7h9M17 7h3M4 12h3M11 12h9M4 17h11M19 17h1M17 7a2 2 0 1 1-4 0 2 2 0 0 1 4 0zM11 12a2 2 0 1 1-4 0 2 2 0 0 1 4 0zM19 17a2 2 0 1 1-4 0 2 2 0 0 1 4 0z",
  search: "M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zM20 20l-4-4",
  x: "M6.5 6.5l11 11M17.5 6.5l-11 11",
  rows: "M4 6h16M4 10h16M4 14h16M4 18h16",
  cards: "M5.5 4h13A1.5 1.5 0 0 1 20 5.5v4a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 9.5v-4A1.5 1.5 0 0 1 5.5 4zM5.5 13h13a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 18.5v-4A1.5 1.5 0 0 1 5.5 13z",
  panel: "M5 4.5h14a1.5 1.5 0 0 1 1.5 1.5v12a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18V6A1.5 1.5 0 0 1 5 4.5zM14.5 4.5v15",
  folder: "M3.5 7.5a2 2 0 0 1 2-2h3.8l2 2h7.2a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z",
  link: "M10 14a4 4 0 0 0 5.66 0l3-3a4 4 0 0 0-5.66-5.66l-1 1M14 10a4 4 0 0 0-5.66 0l-3 3a4 4 0 0 0 5.66 5.66l1-1",
  copy: "M10 9h8.5a1.5 1.5 0 0 1 1.5 1.5V19a1.5 1.5 0 0 1-1.5 1.5H10A1.5 1.5 0 0 1 8.5 19v-8.5A1.5 1.5 0 0 1 10 9zM5.5 15H5a1.5 1.5 0 0 1-1.5-1.5V5A1.5 1.5 0 0 1 5 3.5h8.5A1.5 1.5 0 0 1 15 5v.5",
  chevronDown: "M6 9l6 6 6-6",
  chevronEnd: "M9 6l6 6-6 6",
  download: "M12 4v11M7 10.5l5 5 5-5M5 20h14",
  check: "M5 12.5l4.5 4.5L19 7.5",
  alert: "M10.3 4.6 2.7 17.8a2 2 0 0 0 1.7 3h15.2a2 2 0 0 0 1.7-3L13.7 4.6a2 2 0 0 0-3.4 0zM12 9.5v4M12 17h.01",
  refresh: "M20 11.5a8 8 0 1 0-2.4 5.7M20 4.5v7h-7",
  inbox: "M3.5 13.5h4.5l1.5 2.5h5l1.5-2.5h4.5M3.5 13.5 6.2 6a1.5 1.5 0 0 1 1.4-1h8.8a1.5 1.5 0 0 1 1.4 1l2.7 7.5V18a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18z",
  arrowCircle: `${circle}M12 7.5v9M8.5 13 12 16.5 15.5 13`,
  clock: `${circle}M12 7v5l3.2 2`,
  globe: `${circle}M3 12h18M12 3c2.4 2.5 3.6 5.5 3.6 9s-1.2 6.5-3.6 9c-2.4-2.5-3.6-5.5-3.6-9s1.2-6.5 3.6-9z`,
  pauseCircle: `${circle}M10 9v6M14 9v6`,
  checkCircle: `${circle}M8.5 12.3l2.4 2.4 4.6-4.9`,
  alertCircle: `${circle}M12 7.5v5.5M12 16.2h.01`,
  layers: "M12 3.5 3.5 8 12 12.5 20.5 8zM3.5 12.5 12 17l8.5-4.5M3.5 16.5 12 21l8.5-4.5",
  keyboard: "M4.5 6h15A1.5 1.5 0 0 1 21 7.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 16.5v-9A1.5 1.5 0 0 1 4.5 6zM7 10h.01M10.5 10h.01M14 10h.01M17.5 10h.01M8 14h8",
  menu: "M4 6h16M4 12h16M4 18h16",
  more: "M12 5.5v.01M12 12v.01M12 18.5v.01",
  gauge: "M12 14.5l3.8-4.3M4.2 18.5a9 9 0 1 1 15.6 0",
  wind: "M3 8.5h10.5a3 3 0 1 0-3-3M3 12.5h15a3 3 0 1 1-3 3M3 16.5h6.5",
  file: "M7 3.5h7l4.5 4.5v12a.5.5 0 0 1-.5.5H7a.5.5 0 0 1-.5-.5V4a.5.5 0 0 1 .5-.5zM13.5 3.5V8h5",
  video: "M4.5 6h15A1.5 1.5 0 0 1 21 7.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 16.5v-9A1.5 1.5 0 0 1 4.5 6zM10 9.5v5l4.2-2.5z",
  music: "M9 17.5V6.5l10-2v11M9 17.5a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0zM19 15.5a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0z",
  document: "M7 3.5h7l4.5 4.5v12a.5.5 0 0 1-.5.5H7a.5.5 0 0 1-.5-.5V4a.5.5 0 0 1 .5-.5zM13.5 3.5V8h5M9.5 12.5h5M9.5 16h5",
  archive: "M4 4.5h16v4H4zM5.5 8.5h13V19a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1zM10 12h4",
  app: "M5 4.5h14A1.5 1.5 0 0 1 20.5 6v12a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18V6A1.5 1.5 0 0 1 5 4.5zM3.5 9h17M6.5 6.8h.01M9 6.8h.01",
  history: "M4 12a8 8 0 1 0 2.4-5.7M4 4.5v4.2h4.2M12 8v4l2.8 1.8",
  calendar: "M5 5.5h14A1.5 1.5 0 0 1 20.5 7v11.5A1.5 1.5 0 0 1 19 20H5a1.5 1.5 0 0 1-1.5-1.5V7A1.5 1.5 0 0 1 5 5.5zM3.5 10h17M8 3.5v4M16 3.5v4",
  stop: "M7.5 6.5h9a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1z",
  edit: "M4.5 19.5h4l10.2-10.2a1.5 1.5 0 0 0 0-2.1l-1.9-1.9a1.5 1.5 0 0 0-2.1 0L4.5 15.5zM13.5 6.5l4 4",
  power: "M12 3.5v8M7.2 6.4a7.5 7.5 0 1 0 9.6 0",
  image: "M5 4.5h14A1.5 1.5 0 0 1 20.5 6v12a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18V6A1.5 1.5 0 0 1 5 4.5zM3.5 16l5-5 4.5 4.5 2.5-2.5 5 5M15.5 9h.01",
} as const;

export type IconName = keyof typeof ICONS;

/** Icons that point along the reading direction and must mirror in RTL. */
export const DIRECTIONAL: ReadonlySet<IconName> = new Set(["chevronEnd"]);

/** Icons drawn as dots that need a heavier stroke to be seen. */
export const DOTTED: ReadonlySet<IconName> = new Set(["more"]);
