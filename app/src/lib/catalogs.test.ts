import { expect, test } from "bun:test";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { argumentsOf } from "./message-format";
import { pluralRule } from "./plural";
import { isLanguageCode } from "./language-code";
import languageTags from "../../../locales/.tests/language-tags.json";

// The catalogs contributors add: each must read, and say what English says with the same arguments used the same way.
const dir = join(import.meta.dir, "../../../locales");
const files = readdirSync(dir).filter((f) => f.endsWith(".json"));
const load = (file: string) => JSON.parse(readFileSync(join(dir, file), "utf8")) as Record<string, unknown>;
const english = load("en.json");

test("catalog tags allow scripts, regions and variants with canonical spelling", () => {
  for (const tag of languageTags.valid) expect(isLanguageCode(tag), tag).toBe(true);
  for (const tag of languageTags.invalid) expect(isLanguageCode(tag), tag).toBe(false);
});

test("every language is a file named by its code, describing itself", () => {
  expect(files).toContain("en.json");
  for (const file of files) {
    expect(isLanguageCode(file.replace(/\.json$/, "")), file).toBe(true);
    const meta = load(file)._meta as Record<string, string>;
    expect(meta?.name, file).toBeTruthy();
    expect(meta?.english_name, file).toBeTruthy();
    expect(pluralRule(meta?.plural ?? ""), file).toBeDefined();
    expect(typeof meta?.decimal, file).toBe("string");
    expect(typeof meta?.group, file).toBe("string");
    expect(["ltr", "rtl"], file).toContain(meta?.direction ?? "");
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
