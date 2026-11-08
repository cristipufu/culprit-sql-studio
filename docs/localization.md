# Localization

Culprit speaks English, Spanish, French and German. Every word it shows (the app's buttons and headings, each finding's
title, summary, fix and verify steps, metric labels, notes) comes from a **language catalog**: one JSON file per language
in [`locales/`](../locales).

## Adding a language

1. Copy `locales/en.json` to `locales/<code>.json`, using a canonical BCP 47 tag (`it`, `pt-BR`, `zh-Hans`,
   `sr-Latn-RS`). Tags are case sensitive; `pt-br` is not canonical.
2. Fill in `_meta`:
   - `name`: the language's name for itself (`Italiano`);
   - `english_name`: its name in English (`Italian`);
   - `plural`: the CLDR cardinal rule locale (`it`, `fr`, `pl`, `ar`, `pt-PT`); the shared CLDR data supplies its rules;
   - `direction`: `ltr` or `rtl`;
   - `decimal` and `group`: the decimal and thousands separators (`","` and `"."` in German; `","` and a narrow
     no-break space in French).
3. Translate the values. Leave out any you haven't translated yet: those messages read in English.
4. Run `bun test src/lib/catalogs.test.ts` in `app/` (or `cargo test -p culprit-core --lib i18n`). It checks that every
   message reads, that no key is one English lacks, and that each uses the same arguments as English.

To translate in batches, or to catch up after English gains messages, `dev/catalog.py` does the bookkeeping:

```sh
python3 dev/catalog.py status                 # how much of English each language says
python3 dev/catalog.py todo it ui.editor. > todo.json   # what Italian lacks (optionally under a prefix), as {key: English}
python3 dev/catalog.py add it todo.json       # after translating the values in place: into locales/it.json, sorted
python3 dev/catalog.py prune it               # drop messages English no longer has, or whose arguments English changed
python3 dev/catalog.py stale it               # translations whose reviewed English changed, even with the same arguments
python3 dev/catalog.py stale --check           # fail if any translation needs source review
```

A language's terminology (how it says "index", "wait", "scan", "finding"; formal or informal address) is kept in
`locales/glossary/<code>.md`, so every message says a thing the same way. Start one for a new language, and read it
before changing an existing one.

`todo` includes missing and stale messages. Its `_meta.source_hashes` records the English exported for that batch;
leave this metadata intact while translating. `add` refuses a batch if English changed after export. It updates only
the submitted keys' fingerprints in the language catalog. A manually authored batch without metadata records review
against the current English. `review <code>` records a review of the entire catalog; use it only after reviewing every
message. `track <code>` bootstraps older catalogs from Git history without marking old translations as current.

The build embeds every language. Rust includes the complete catalogs; the app includes UI/common messages and keys
referenced by frontend code or generated engine facts. Backend prose arrives already rendered by Rust. Contributor
fingerprints are excluded from runtime assets. Nothing loads a catalog from disk at runtime, and the bundle check
verifies each language's selected messages in the built app.

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

Cardinal cases are `zero`, `one`, `two`, `few`, `many` and `other`; include the categories your language needs and always
include `other`. Exact selectors such as `=0` take precedence. Rules use the absolute numeric value and its decimal
operands. Numeric arguments do not preserve visible trailing zeros (`1.0` is `1`) or compact-number exponents.
Both runtimes use the same pinned CLDR data in `locales/.cldr/`; `python3 dev/update-plurals.py` regenerates it and the
upstream sample vectors in `locales/.tests/plurals.json`. Review the data/version and license when updating it.

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
- **A phrase about a period** ("the last 7 days") is a whole message per unit with the number as a plural, never "the
  last" + a worded span: the words around a number agree with it. Rust has `culprit_core::last_span`, `log_span` and
  `since_reset_span`; a sentence takes the phrase as its `{window}`. The app chooses with `byPeriod`
  (`app/src/lib/format.ts`). A duration written with an abbreviated unit (`{span}` as "5.2 h") is the one exception:
  translate around it without agreeing with it ("el último periodo de {span}").
- **A number before a noun** is a plural on the number (`{n, plural, one {{count} run} other {{count} runs}}`, with `count`
  the number as written), even where English reads the same for every count: other languages agree with it.
- **A counted noun** is a plural message (`common.noun.time`: `{n, plural, one {time} other {times}}`), handed to helpers
  by key with `key!("common.noun.time")` and said with `support::counted`.
- **Engine vocabulary** (MySQL's "rows examined" against "pages") comes from the engine's facts, not ad-hoc strings.
- **No logic reads text**: decide on facts, then word them. Metrics carry their catalog key through `Metric::said` or
  `Metric::keyed`; de-duplication uses that identity, retaining unkeyed custom metrics. Impact labels that refinement
  updates carry a key through `Impact::set_keyed_label`. Use `Impact::set_label` for custom wording so it clears any
  previous label identity.
- **Render text as text**: the app shows messages as text nodes, never as HTML. A sentence with an element inside (a
  name in bold, a command in `<code>`) is still one message: `tx("ui.drift_panel.compare", {}, { connection: <b>{name}</b> })`
  (`app/src/lib/message-parts.tsx`) puts each element where its `{argument}` is.
- SQL a fix gives is never translated: it is an argument.
- A retained observation uses `message!("key", ...)` and `i18n::Message`, with unformatted `MessageValue` arguments,
  instead of storing the result of `t!`. Recorded event titles/details and collector notes render when read. Raw SQL,
  identifiers and user-authored marks remain text. Saved data uses the current format; older formats are not supported.
- Workload source identity stays in `source`; backend text uses `source_name()` and the app uses `workloadSource()`.
  The rendered `source_label` is refreshed for the reader. A scoped provider label retains its arguments in
  `source_message`, so translating a node/role label never changes the counters' identity.
- RTL catalogs set the page's direction. Use logical CSS properties for layout; SQL, identifiers and coordinate plots
  retain LTR isolation. English fallback messages are isolated within an RTL paragraph.

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
