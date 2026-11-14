# Translating Whoosh

Whoosh is translated on **hosted Weblate** (the free plan for libre projects, ADR 0007 amendment 1). Translators work in the browser; Weblate opens pull requests against `dev`, and maintainers review and merge them like any other PR. You do not need to know Git or JSON.

- **Translate:** <https://hosted.weblate.org/engage/whoosh-download-manager/> (live once the owner has created the project; see [Setting up Weblate](#setting-up-weblate-for-the-owner) below).
- **Languages today:** English (source), বাংলা (Bangla, reviewed by people), and ten languages seeded by machine translation and waiting for review: Arabic, Chinese (Simplified), Hindi, Indonesian, Portuguese (Brazil), Russian, Spanish, Turkish, Urdu and Vietnamese. The app's language picker marks those ten as machine-translated, in their own language, and links here.

## How to help

1. Open the project on Weblate, pick your language and a component (start with **UI**).
2. Translate the strings marked *Needs editing* or *Untranslated*. For a machine-seeded language, review every string: fix the wording, then mark it as translated.
3. Use the glossary below (Weblate shows it beside each string) so the same thing always has the same name.
4. Weblate checks placeholders, plurals and lengths as you type. A red check usually means a placeholder was changed: keep `{{name}}` and `$FILE$` exactly as they are, but move them wherever your grammar needs them.

A language counts as **reviewed** when a native speaker has gone through every string. Then a maintainer removes `machine: true` from its entry in `packages/ui/src/lib/i18n/languages.ts`, and the "machine translated" note disappears from the language picker.

To add a language that is not listed, use *Start new translation* on Weblate. It appears in the app once at least half of the UI is translated and its file is merged (`packages/ui/src/lib/i18n/languages.ts` can add its Intl locale and native name; without an entry the name comes from the browser).

## What gets translated

| Component (Weblate) | Files | Format | Placeholders | Plurals | License |
|---|---|---|---|---|---|
| **UI**: the desktop app and its tray menu | `packages/ui/src/lib/i18n/locales/<lang>.json` | i18next JSON v4 (nested keys) | `{{name}}` | `key_one`, `key_other`, … one key per CLDR plural form of the language | Apache-2.0 |
| **Browser extension**: popup, options page, context menus, store name and description | `apps/extension/_locales/<lang>/messages.json` | WebExtension JSON | `$NAME$` (defined under `placeholders`) | none (the API has no plurals; the English avoids them) | GPL-3.0-or-later |

- English is the source in both components. Files are monolingual: each language file has the same keys as the English one.
- **File names.** The UI uses BCP 47 tags with a hyphen: `pt-BR.json`, `zh-Hans.json`. The extension uses the folder names browsers accept: `pt_BR`, `zh_CN` (Chrome's list; `apps/extension/test/locales.test.ts` refuses any other).
- **Plurals** follow CLDR, the same rules the app uses at run time (`Intl.PluralRules`): English and Bangla have *one* and *other*, Arabic six forms, Russian *one*/*few*/*many*/*other*, Chinese, Indonesian and Vietnamese only *other*. Weblate shows one box per form. A form left empty falls back to *other*.
- **Untranslated strings** are empty (`""`); the app shows English for them.
- **The desktop shell** (the tray menu) reads its strings from the UI files (`tray.*`), embedded into the Rust binary at build time, so there is no third component.
- **Numbers, dates, sizes and percentages** are formatted by the app for each language (Bangla digits, Arabic separators, `%42,3` in Turkish); only the words around them are translated.
- **The extension follows the browser's language** (that is how the WebExtension i18n API works); the desktop app follows *Settings → Language*, which is shared with the command line through the daemon.

### Things to keep as they are

- `Whoosh` (the brand), `app.name`, `extName`.
- Placeholders: `{{count}}`, `{{name}}`, `$FILE$`, … (their position is yours to choose). In a singular form you may leave out `{{count}}` when the language says "one file" without a number, but not in a form that also covers other numbers (Russian *one* covers 21, 31…).
- Technical tokens in examples: `https://example.com/file.zip`, `http://`, `https://`, `ftp://`, `KB`/`MB`/`GB`, `23:30`, `zip iso mp4`, `example.com`, `Alt`.
- A trailing `…` on a menu item or button: it means "opens a dialog".
- `extDescription` is at most 132 characters (store limit); `badgeOff` at most 4 (the toolbar badge).

## Glossary

The agreed English and Bangla terms. Weblate's glossary for the project should hold the same list; the seeded languages' choices are listed so reviewers can see what to check.

| English | Meaning in Whoosh | বাংলা |
|---|---|---|
| download (noun) | one file being fetched, an entry in the list | ডাউনলোড |
| download (verb) | fetch a file | ডাউনলোড করা (not নামানো) |
| queue | a named group of downloads that run in turn | সারি |
| main queue | the default queue | প্রধান সারি |
| segment | one part of a file fetched over its own connection | অংশ |
| connection | an HTTP connection to the server | সংযোগ |
| pause / paused | stop for now, keeping what was fetched | থামান / বিরতিতে |
| resume | continue a paused download | চালিয়ে যান |
| remove | take off the list (files stay on disk) | সরান |
| delete | erase from disk, or delete a queue | মুছুন / মুছে ফেলা |
| move (to a queue) | put a download in another queue | অন্য সারিতে নিয়ে যান |
| link | what the user pastes or clicks | লিংক |
| address | the URL of a download | ঠিকানা |
| expired (link) | the server no longer accepts the address | মেয়াদ শেষ |
| refresh (a link) | give an expired download a new address | লিংক নবায়ন |
| schedule | times when a queue starts or stops | সময়সূচি |
| time window | a period with its own speed limit | সময়-পর্ব |
| speed limit | maximum total download speed | গতিসীমা / গতির সীমা |
| category | file kind: video, music, documents… | শ্রেণি |
| history | finished and failed downloads | ইতিহাস |
| settings | | সেটিংস |
| folder | | ফোল্ডার |
| tray | the notification-area icon | ট্রে |
| quit | close the app completely (tray menu) | প্রস্থান |
| notification | | বিজ্ঞপ্তি |
| browser extension | | ব্রাউজার এক্সটেনশন |
| catch (a download) | the extension takes over a browser download | ধরা |
| sleep / shut down | after-queue actions | স্লিপ মোড / কম্পিউটার বন্ধ |

Bangla style: written for readers in Bangladesh; everyday words before formal Sanskritised ones, and the loanwords people actually use (ডাউনলোড, লিংক, ফোল্ডার, সেটিংস); polite imperative (করুন); counts with টি (৩টি ডাউনলোড), digits come from the app.

### Seeded languages (machine translation, to review)

| English | ar | es | hi | id | pt-BR | ru | tr | ur | vi | zh-Hans |
|---|---|---|---|---|---|---|---|---|---|---|
| download | تنزيل | descarga | डाउनलोड | unduhan | download | загрузка | indirme | ڈاؤن لوڈ | tải xuống | 下载 |
| queue | قائمة الانتظار | cola | कतार | antrean | fila | очередь | kuyruk | قطار | hàng đợi | 队列 |
| pause | إيقاف مؤقت | pausar | रोकें | jeda | pausar | приостановить | duraklat | روکیں | tạm dừng | 暂停 |
| resume | استئناف | reanudar | जारी रखें | lanjutkan | retomar | возобновить | sürdür | جاری رکھیں | tiếp tục | 继续 |
| segment | جزء | segmento | खंड | segmen | segmento | сегмент | parça | حصہ | phân đoạn | 分段 |
| connection | اتصال | conexión | कनेक्शन | koneksi | conexão | соединение | bağlantı | کنکشن | kết nối | 连接 |
| link | رابط | enlace | लिंक | tautan | link | ссылка | bağlantı | لنک | liên kết | 链接 |
| address | عنوان | dirección | पता | alamat | endereço | адрес | adres | پتہ | địa chỉ | 地址 |
| schedule | الجدولة | programación | शेड्यूल | jadwal | agendamento | расписание | zamanlama | شیڈول | lịch | 计划 |
| speed limit | حد السرعة | límite de velocidad | गति सीमा | batas kecepatan | limite de velocidade | ограничение скорости | hız sınırı | رفتار کی حد | giới hạn tốc độ | 限速 |
| tray | علبة النظام | bandeja | ट्रे | baki sistem | bandeja | трей | sistem tepsisi | ٹرے | khay hệ thống | 托盘 |
| history | السجل | historial | इतिहास | riwayat | histórico | история | Geçmiş | سرگزشت | lịch sử | 历史记录 |

Known questions for reviewers: Arabic "tray" (علبة النظام vs. منطقة الإعلام); Turkish uses *bağlantı* for both link and connection; Russian uses *Удалить* for both remove and delete; Hindi and Urdu write durations in full words; the toolbar badge reads `OFF` in Spanish, Portuguese and Turkish because no native word fits in four letters.

## For developers

### Adding or changing a string

1. Add it to `packages/ui/src/lib/i18n/locales/en.json` (or the extension's `_locales/en/messages.json`, with a `description` for translators and `placeholders` for every `$NAME$`).
2. Add the Bangla text to `bn.json` / `_locales/bn/messages.json` in the same PR if you can; otherwise leave it for a Bangla reviewer.
3. Run `pnpm --filter @whoosh/ui i18n:sync`. It adds the new keys to every other UI locale as empty strings (untranslated), in English's key order, with each language's plural forms. `pnpm --filter @whoosh/ui i18n:check` only reports.
4. `pnpm -r test` checks key and placeholder parity, plural forms, Bangla completeness and the extension's locales.

In code, `t("group.key", { count, name })` in the UI (keys are type-checked against `en.json`; a `count` number picks the plural form), and `t("messageName", [subs])` in the extension (`src/shared/i18n.ts`).

Rules that keep strings translatable:

- One sentence per string; never build a sentence from pieces. Put elements inside a message with placeholders (`tNodes` in the extension).
- Use a plural key whenever a number is counted, even if English reads fine without it.
- "Exactly one" is not a plural form (Russian *one* is 1, 21, 31…). When the singular text names the item instead of counting it, use a separate key (`remove.titleOne` with `{{name}}`, `remove.title_*` with `{{count}}`) and choose in code.
- Do not rename keys casually: Weblate loses the history. If you must, add the old name to `RENAMED` in `packages/ui/scripts/i18n-import-ts.mjs`.

### Strings from an older branch

Before wave 3 (November 2026), UI strings were TypeScript objects in `packages/ui/src/lib/i18n/en.ts` and `bn.ts` (`{name}` placeholders, `{ one, other }` plurals). A branch that still adds strings there converts them mechanically:

```bash
# after merging dev, if git kept your en.ts / bn.ts (modify/delete conflict): keep them for a moment
node packages/ui/scripts/i18n-import-ts.mjs                     # reads src/lib/i18n/en.ts and bn.ts if present
node packages/ui/scripts/i18n-import-ts.mjs en=/tmp/en.ts bn=/tmp/bn.ts   # or explicit files, e.g. from `git show`
git rm packages/ui/src/lib/i18n/en.ts packages/ui/src/lib/i18n/bn.ts
```

The script adds the keys that the JSON files do not have yet (`{name}` → `{{name}}`, plural objects → `_one`/`_other`), keeps existing JSON values (pass `--overwrite` to replace them), maps renamed keys, and fills the other locales with empty strings. Then replace imports of `../lib/i18n/en.ts` with `MessageKey` from `../lib/i18n/i18n.svelte.ts`, and check for the `remove`/`move` pattern above. Needs Node 22.18 or newer (it imports the `.ts` files directly).

## Setting up Weblate (for the owner)

Nobody but the owner connects services to this repository. These are the values to enter on <https://hosted.weblate.org> after signing in.

### 1. Project

*Add new translation project* (and apply for the libre plan: *Hosting → Libre*, which needs a public repository with an OSI license, both of which we have):

| Field | Value |
|---|---|
| Project name | `Whoosh` |
| URL slug | `whoosh-download-manager` (the app links to `https://hosted.weblate.org/engage/whoosh-download-manager/`; if you choose another slug, change `TRANSLATE_URL` in `packages/ui/src/lib/i18n/languages.ts` and the URLs in this file) |
| Project website | `https://github.com/coderzonebd/whoosh-download-manager` |
| Translation instructions | `https://github.com/coderzonebd/whoosh-download-manager/blob/dev/docs/translating.md` |
| Access control | Public |
| Language aliases | (none needed) |

Then under *Settings → Glossary*, add the glossary table above (English → Bangla; other languages as they are reviewed).

### 2. Component "UI"

*Add new translation component → From version control*:

| Field | Value |
|---|---|
| Component name / slug | `UI` / `ui` |
| Version control system | **GitHub pull request** (Weblate pushes to its own fork and opens PRs; it never pushes to our branches) |
| Source code repository | `https://github.com/coderzonebd/whoosh-download-manager.git` |
| Repository branch | `dev` |
| Repository push URL | leave empty |
| Push branch | `weblate-ui` (or leave the default) |
| File format | **i18next JSON file v4** |
| File mask | `packages/ui/src/lib/i18n/locales/*.json` |
| Monolingual base language file | `packages/ui/src/lib/i18n/locales/en.json` |
| Edit base file | off (English changes come through PRs) |
| Template for new translations | `packages/ui/src/lib/i18n/locales/en.json` |
| Adding new translation | Create new language file |
| Language code style | **BCP style using hyphen as a separator** (`pt-BR`, `zh-Hans`) |
| Source language | English |
| Translation flags | `i18next-interpolation` |
| Translation license | Apache License 2.0 |
| Push on commit | on; Age of changes to commit: 24 hours |
| Merge style | Rebase |

Addons for this component:
- **Customize JSON output**: indentation 2 spaces, *Sort JSON keys* off, so diffs stay small and the key order matches English.
- **Squash Git commits** (optional): per language, so each PR is easy to review.

### 3. Component "Browser extension"

| Field | Value |
|---|---|
| Component name / slug | `Browser extension` / `extension` |
| Source code repository | `weblate://whoosh-download-manager/ui` (shares the UI component's repository, so both arrive in one PR) |
| File format | **WebExtension JSON** |
| File mask | `apps/extension/_locales/*/messages.json` |
| Monolingual base language file | `apps/extension/_locales/en/messages.json` |
| Edit base file | off |
| Template for new translations | `apps/extension/_locales/en/messages.json` |
| Adding new translation | Create new language file |
| Language code style | **WebExtension style** (gives `pt_BR`, `zh_CN`; if your Weblate lacks it, use *POSIX style using underscore* and check that Chinese becomes `zh_CN`) |
| Source language | English |
| Translation license | GNU General Public License v3.0 or later |

Afterwards, on the string `extDescription` add the flag `max-length:132`, and on `badgeOff` the flag `max-length:4`.

### 4. After the first Weblate PR

- CI runs the same tests on it as on any PR (`pnpm -r test` checks keys, placeholders and plural forms).
- Weblate commits are authored by the translators; review and merge as usual. The `weblate` user appears in the PR.
- When a seeded language has been reviewed completely, drop its `machine: true` in `languages.ts`.

### Command-line client (optional)

`.weblate` in the repository root configures [`wlc`](https://docs.weblate.org/en/latest/wlc.html), Weblate's command-line client, for this project (`wlc stats`, `wlc lock`, `wlc pull`). Put your API key in `~/.config/weblate`, never in the repository.
