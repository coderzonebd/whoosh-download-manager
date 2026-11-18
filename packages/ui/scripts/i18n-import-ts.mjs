// SPDX-License-Identifier: Apache-2.0
// Moves UI strings written in the old TypeScript format (`en.ts`, `bn.ts`:
// `{name}` placeholders, `{ one, other }` plural objects) into the locale
// files Weblate reads (`src/lib/i18n/locales/<lang>.json`, i18next JSON v4:
// nested keys, `{{name}}` placeholders, `key_one` / `key_other` plurals).
//
//   node packages/ui/scripts/i18n-import-ts.mjs                 # en.ts / bn.ts next to the locales, if present
//   node packages/ui/scripts/i18n-import-ts.mjs en=/tmp/en.ts bn=/tmp/bn.ts
//   node packages/ui/scripts/i18n-import-ts.mjs --overwrite     # TS values replace JSON values that differ
//   node packages/ui/scripts/i18n-import-ts.mjs --check         # report only, write nothing (exit 1 if anything would change)
//
// Keys already in the JSON are kept as they are unless --overwrite is given:
// after the move, the JSON files are the source of truth. Keys only in the
// JSON are never removed. Afterwards every other locale file gets the new
// keys as empty strings (untranslated: the UI falls back to English and
// Weblate lists them as "needs translation"), in the source's key order.
//
// Needs Node 22.18 or newer (it imports the .ts files with Node's built-in
// type stripping). See docs/translating.md, "Strings from an older branch".

import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
export const I18N_DIR = resolve(here, "../src/lib/i18n");
export const LOCALES_DIR = join(I18N_DIR, "locales");
export const SOURCE = "en";
export const PLURAL_SUFFIXES = ["zero", "one", "two", "few", "many", "other"];

/**
 * Keys that were renamed when the strings moved to JSON. A branch that still
 * has the old TS files must not bring the old names back.
 * old key → new key (null: dropped, replaced by code changes).
 */
export const RENAMED = {
  "detail.resumable": "detail.resumable.label",
  "history.filter": "history.filter.label",
};

/** Intl locale for plural rules; mirrors `languages.ts` for the tags that differ. */
const INTL = { bn: "bn-BD", "zh-Hans": "zh-Hans", "pt-BR": "pt-BR" };

export function pluralCategories(tag) {
  return new Intl.PluralRules(INTL[tag] ?? tag).resolvedOptions().pluralCategories;
}

/** `{name}` → `{{name}}`; `{{name}}` stays as it is. */
export function convertPlaceholders(s) {
  return s.replace(/(\{)?\{(\w+)\}(\})?/g, (whole, open, name, close) => (open && close ? whole : `${open ?? ""}{{${name}}}${close ?? ""}`));
}

function isPluralObject(v) {
  return v !== null && typeof v === "object" && typeof v.other === "string";
}

/** A flat `{ "a.b": "…", "c_one": "…" }` table from one old-format TS table. */
export function convertTable(table, tag) {
  const out = {};
  const cats = pluralCategories(tag);
  for (const [oldKey, value] of Object.entries(table)) {
    const key = Object.hasOwn(RENAMED, oldKey) ? RENAMED[oldKey] : oldKey;
    if (key === null) continue;
    if (typeof value === "string") out[key] = convertPlaceholders(value);
    else if (isPluralObject(value)) {
      for (const c of cats) out[`${key}_${c}`] = convertPlaceholders(value[c] ?? value.other);
    } else throw new Error(`${tag}: ${oldKey} is neither a string nor a plural object`);
  }
  return out;
}

/** Nested object → flat dotted keys (plural suffixes stay on the leaf). */
export function flatten(obj, prefix = "", out = {}) {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (typeof v === "string") out[key] = v;
    else if (v && typeof v === "object") flatten(v, key, out);
    else throw new Error(`${key}: unexpected ${typeof v}`);
  }
  return out;
}

/** Flat dotted keys → nested object, in the order given. */
export function unflatten(flat) {
  const root = {};
  for (const [key, value] of Object.entries(flat)) {
    const parts = key.split(".");
    let node = root;
    for (const [i, p] of parts.entries()) {
      if (i === parts.length - 1) {
        if (typeof node[p] === "object") throw new Error(`"${key}" is both a string and a group of strings`);
        node[p] = value;
      } else {
        if (typeof node[p] === "string") throw new Error(`"${parts.slice(0, i + 1).join(".")}" is both a string and a group of strings`);
        node[p] ??= {};
        node = node[p];
      }
    }
  }
  return root;
}

function baseKey(key) {
  const m = /^(.*)_(zero|one|two|few|many|other)$/.exec(key);
  return m ? m[1] : key;
}

/**
 * The keys a locale must have, in the source's order: plain keys as they
 * are, plural keys expanded to the locale's CLDR categories.
 */
export function expectedKeys(sourceFlat, tag) {
  const cats = pluralCategories(tag);
  const out = [];
  const seen = new Set();
  for (const key of Object.keys(sourceFlat)) {
    const base = baseKey(key);
    if (base !== key) {
      if (seen.has(base)) continue;
      seen.add(base);
      for (const c of cats) out.push(`${base}_${c}`);
    } else out.push(key);
  }
  return out;
}

/** Reorders `flat` to the source order, adding missing keys as "" and keeping extras last. */
export function fillLocale(sourceFlat, flat, tag) {
  const out = {};
  for (const key of expectedKeys(sourceFlat, tag)) out[key] = flat[key] ?? "";
  for (const [k, v] of Object.entries(flat)) if (!(k in out)) out[k] = v;
  return out;
}

export function readLocale(tag) {
  const file = join(LOCALES_DIR, `${tag}.json`);
  return existsSync(file) ? flatten(JSON.parse(readFileSync(file, "utf8"))) : {};
}

export function serialize(flat) {
  return `${JSON.stringify(unflatten(flat), null, 2)}\n`;
}

async function loadTs(path) {
  const mod = await import(pathToFileURL(resolve(path)).href);
  const table = Object.values(mod).find((v) => v && typeof v === "object");
  if (!table) throw new Error(`${path}: no exported string table`);
  return table;
}

async function main(argv) {
  const overwrite = argv.includes("--overwrite");
  const check = argv.includes("--check");
  const inputs = argv.filter((a) => !a.startsWith("--")).map((a) => {
    const [tag, path] = a.split("=");
    if (!tag || !path) throw new Error(`expected <lang>=<file.ts>, got "${a}"`);
    return { tag, path };
  });
  if (inputs.length === 0) {
    for (const tag of ["en", "bn"]) {
      const path = join(I18N_DIR, `${tag}.ts`);
      if (existsSync(path)) inputs.push({ tag, path });
    }
  }
  // English first, so the other locales are checked against the new keys.
  inputs.sort((a, b) => (a.tag === SOURCE ? -1 : b.tag === SOURCE ? 1 : 0));

  const tags = new Set(readdirSync(LOCALES_DIR).filter((f) => f.endsWith(".json")).map((f) => basename(f, ".json")));
  const result = new Map();
  for (const tag of tags) result.set(tag, readLocale(tag));

  const report = [];
  for (const { tag, path } of inputs) {
    const table = convertTable(await loadTs(path), tag);
    const current = result.get(tag) ?? {};
    let added = 0;
    let changed = 0;
    for (const [k, v] of Object.entries(table)) {
      if (!(k in current) || current[k] === "") {
        if (current[k] !== v) added++;
        current[k] = v;
      } else if (current[k] !== v && overwrite) {
        current[k] = v;
        changed++;
      }
    }
    result.set(tag, current);
    report.push(`${tag}: ${added} added${overwrite ? `, ${changed} replaced` : ""} from ${path}`);
  }

  const source = result.get(SOURCE);
  if (!source || Object.keys(source).length === 0) throw new Error(`no ${SOURCE}.json and no ${SOURCE}.ts to start from`);
  let dirty = false;
  for (const [tag, flat] of result) {
    const next = tag === SOURCE ? flat : fillLocale(source, flat, tag);
    const file = join(LOCALES_DIR, `${tag}.json`);
    const text = serialize(next);
    const before = existsSync(file) ? readFileSync(file, "utf8") : "";
    if (text === before) continue;
    dirty = true;
    report.push(`${check ? "would update" : "updated"} ${file}`);
    if (!check) writeFileSync(file, text);
  }
  console.log(report.length > 0 ? report.join("\n") : "nothing to do");
  if (check && dirty) process.exitCode = 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).catch((e) => {
    console.error(`i18n-import-ts: ${e instanceof Error ? e.message : e}`);
    process.exitCode = 1;
  });
}
