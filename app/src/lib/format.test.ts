// Every value in gen/format-cases.json (written by `cargo test -p culprit-core` from culprit_core::format) must read
// the same here, so a metric in the app says what the finding's prose and brief say.
import { expect, test } from "bun:test";
import cases from "../gen/format-cases.json";
import { bytes, count, days, digits, duration, percent, percentValue, rowCount } from "./format";
import { registerCatalogs, setLanguage } from "./i18n";
import { fromRust } from "../test/fixtures";

test("format.ts writes digits with the language's separators and units in its words", () => {
  registerCatalogs({
    xx: {
      _meta: { name: "Test", english_name: "Test", plural: "fr", decimal: ",", group: "." },
      "ui.format.days": "{n, plural, one {{count} jour} other {{count} jours}}",
      "ui.format.count.thousands": "{value} k",
    },
  });
  try {
    setLanguage("xx");
    expect([duration(2590), bytes(1536), count(1234), days(1.5), days(12), percentValue(12.345), rowCount(1500), digits("1,234,567.5")])
      .toEqual(["2,6 s", "1,5 KB", "1,2 k", "1,5 jour", "12 jours", "12,35%", "1,5 k rows", "1.234.567,5"]);
  } finally {
    setLanguage("en");
    Reflect.deleteProperty(navigator, "languages");
  }
  expect([duration(2590), count(1234), days(1.5), rowCount(1), digits("1,234,567.5")]).toEqual(["2.6 s", "1.2K", "1.5 days", "1 row", "1,234,567.5"]);
});

test("format.ts reads every shared case as culprit_core::format does", () => {
  const formatters: Record<string, (value: number) => string> = { duration, bytes, count, percent, percent_value: percentValue, days };
  const failures: string[] = [];
  let checked = 0;
  for (const [name, list] of Object.entries(fromRust<Record<string, [number, string][]>>(cases))) {
    const format = formatters[name];
    if (!format) {
      failures.push(`format.ts has no ${name}`);
      continue;
    }
    for (const [value, rust] of list) {
      checked++;
      const app = format(value);
      if (app !== rust) failures.push(`${name}(${value}): format.ts says "${app}", culprit_core::format "${rust}"`);
    }
  }
  if (failures.length > 0) {
    throw new Error(`format.ts reads numbers differently from culprit_core::format:\n${failures.join("\n")}`);
  }
  if (checked === 0) throw new Error("gen/format-cases.json has no cases; regenerate it with `cargo test -p culprit-core`");
  if ([null, undefined, NaN, Infinity].some((value) => percentValue(value) !== "—")) throw new Error("Missing percentages must stay unavailable");
});
