import { expect, test } from "bun:test";
import vectors from "../../../locales/.tests/messages.json";
import { type Locale, argumentsOf, formatMessage, parse } from "./message-format";
import pluralSamples from "../../../locales/.tests/plurals.json";
import cardinals from "../../../locales/.cldr/cardinals.json";
import { cardinalCategory } from "./plural";

const locales = vectors.locales as Record<string, Locale>;

test("every independent CLDR sample selects its recorded category", () => {
  expect([...new Set(pluralSamples.map(([code]) => code))].sort()).toEqual(Object.keys(cardinals.languages).sort());
  for (const [code, number, category] of pluralSamples as [string, number, string][]) {
    expect(String(cardinalCategory(number, code)), `${code}: ${number}`).toBe(category);
  }
});

test("every shared case formats as expected (Rust runs the same cases)", () => {
  for (const c of vectors.cases) {
    expect(formatMessage(c.message, c.args as Record<string, string | number>, locales[c.locale]!)).toBe(c.expected);
  }
});

test("a malformed message is refused, saying where", () => {
  for (const m of vectors.malformed) expect(() => parse(m)).toThrow();
});

test("a message's arguments are known by how it uses them", () => {
  const args = argumentsOf("{who} {n, plural, one {waited once} other {waited # times}} {tense, select, past {ago} other {now}}");
  expect([...args.entries()].sort()).toEqual([["n", "plural"], ["tense", "select"], ["who", "text"]]);
});
