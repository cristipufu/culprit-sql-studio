import { expect, test } from "bun:test";
import vectors from "../../../locales/.tests/messages.json";
import { type Locale, argumentsOf, formatMessage, parse } from "./message-format";

const locales = vectors.locales as Record<string, Locale>;

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
