import { expect, test } from "bun:test";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { argumentsOf } from "./message-format";

// The catalogs contributors add: each must read, and say what English says with the same arguments used the same way.
const dir = join(import.meta.dir, "../../../locales");
const files = readdirSync(dir).filter((f) => f.endsWith(".json"));
const load = (file: string) => JSON.parse(readFileSync(join(dir, file), "utf8")) as Record<string, unknown>;
const english = load("en.json");

test("every language is a file named by its code, describing itself", () => {
  expect(files).toContain("en.json");
  for (const file of files) {
    expect(file).toMatch(/^[a-z]{2,3}(-[A-Z][A-Za-z]{1,3})?\.json$/);
    const meta = load(file)._meta as Record<string, string>;
    expect(meta?.name, file).toBeTruthy();
    expect(meta?.english_name, file).toBeTruthy();
    expect(["one_is_1", "one_below_2"], file).toContain(meta?.plural ?? "");
    expect(typeof meta?.decimal, file).toBe("string");
    expect(typeof meta?.group, file).toBe("string");
  }
});

test("every message reads, and a translation uses English's arguments the same way", () => {
  for (const file of files) {
    const catalog = load(file);
    for (const [key, message] of Object.entries(catalog)) {
      if (key === "_meta") continue;
      expect(typeof message, `${file} ${key}`).toBe("string");
      const english_message = english[key];
      expect(typeof english_message, `${file} ${key}: a key English doesn't have`).toBe("string");
      const ours = argumentsOf(message as string);
      const theirs = argumentsOf(english_message as string);
      expect([...ours.entries()].sort(), `${file} ${key}`).toEqual([...theirs.entries()].sort());
    }
  }
});
