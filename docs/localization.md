# Localization

Culprit speaks English, Spanish, French and German. Every word it shows (the app's buttons and headings, each finding's
title, summary, fix and verify steps, metric labels, notes) comes from a **language catalog**: one JSON file per language
in [`locales/`](../locales).

## Adding a language

1. Copy `locales/en.json` to `locales/<code>.json`, where `<code>` is the language's code (`it`, `pt-BR`, `zh-Hans`).
2. Fill in `_meta`:
   - `name`: the language's name for itself (`Italiano`);
   - `english_name`: its name in English (`Italian`);
   - `plural`: `one_is_1` when "one" is exactly 1 (English, German, Italian, Spanish), or `one_below_2` when it covers 0
     and 1 (French);
   - `decimal` and `group`: the decimal and thousands separators (`","` and `"."` in German; `","` and a narrow
     no-break space in French).
3. Translate the values. Leave out any you haven't translated yet: those messages read in English.
4. Run `bun test src/lib/catalogs.test.ts` in `app/` (or `cargo test -p culprit-core --lib i18n`). It checks that every
   message reads, that no key is one English lacks, and that each uses the same arguments as English.

That's all: the build compiles every file in `locales/` into the app and the CLI. Nothing is read from disk when Culprit
runs, and CI checks that every language is inside the built app.

## The message format

Messages use a subset of [ICU MessageFormat](https://unicode-org.github.io/icu/userguide/format_parse/messages/), which
translation tools know:

| Form | Meaning |
|---|---|
| `{table}` | An argument: a name, or text Culprit has already worded (a duration, a percentage). Numbers are written in the language's digits. |
| `{n, plural, =0 {none} one {# row} other {# rows}}` | A case chosen by the language's plural rule. `#` is the number. |
| `{tense, select, past {took} other {takes}}` | A case chosen by an argument's value; `other` is required. |

Keep every `{argument}` a message has, spelled as in English; reorder them as the language needs. Braces are never
literal text. Write a translation as a whole sentence in your language, not word for word: arguments that are phrases
(`{since}`: "since the server last restarted 3 days ago") are themselves translated messages.

## Writing code that shows text

English lives in `locales/en.json`, never in code. Rust says a message with `t!` (from `culprit_core`), the app with
`t` (from `app/src/lib/i18n.ts`):

```rust
f.title = t!("check.heap_table.title.no_clustered_index", table = &t.name, rows = format::count(rows));
```

```tsx
<h3>{t("ui.dashboard.loading")}</h3>
```

- **Keys** are namespaced: `check.<check_id>.<part>` for a check (`check.heap_table.summary.heap`), `view.<view>.<part>`,
  `insight.<rule>.<part>`, `engine.<part>`, `provider.<engine>.<part>`, `ui.<area>.<part>`, and `common.<part>` for
  words many places share (`common.metric.rows`, `common.noun.time`). Keep a key stable once it exists.
- **One message is one whole sentence** (or one whole label). Never build a sentence from English pieces in code
  ("takes" + "until it stopped"): word order and grammar differ between languages. Where a sentence has variants, write
  each variant as its own message (`...summary.forwarded` and `...summary.forwarded_per_day`), or use `select`. A
  paragraph is sentences joined with a space.
- **Arguments** are names, numbers, SQL, or phrases that are themselves messages. Numbers, sizes, durations and
  percentages are written through `format::` (Rust) or `lib/format.ts` (app), which follow the language.
- **A counted noun** is a plural message (`common.noun.time`: `{n, plural, one {time} other {times}}`), handed to helpers
  by key with `key!("common.noun.time")` and said with `support::counted`.
- **Engine vocabulary** (MySQL's "rows examined" against "pages") comes from the engine's facts, not ad-hoc strings.
- **No logic reads text**: decide on facts, then word them.
- **Render text as text**: the app shows messages as text nodes, never as HTML.
- SQL a fix gives is never translated: it is an argument.

## Checks: decide, then render

Every built-in check is a `culprit_core::Diagnosis`, declared with
`check!(Name: info, decide = .., render = .., case<'s> = .., evidence = ..)`:

- **`decide(&Snapshot) -> Vec<Case>`** establishes what the evidence supports: typed cases holding facts (the table, the
  numbers, which advice applies, the severity), never prose.
- **`render(&Snapshot, Case) -> Finding`** words one case from the catalog, making no further judgement.

`crates/culprit-checks/src/checks/maintenance/heap_table.rs` is the reference.

## The language Culprit speaks

The app remembers the chosen language on the device and tells the desktop app, which renders findings in it when it reads
a report (reports are analyzed again on every read, so a saved scan reads in the language chosen now). The CLI takes
`--lang <code>` or `CULPRIT_LANG`. Output for coding agents (the brief, digests, MCP tools, CLI JSON) stays in English.
Rust code runs in the language `culprit_core::i18n::scope` sets for the work on its thread.

## Merging catalogs

Branches that each add keys to `locales/en.json` merge key by key with the `catalog` merge driver
(`.gitattributes`). Enable it once per clone:

```sh
git config merge.catalog.driver "python3 dev/merge-catalog.py %O %A %B"
```
