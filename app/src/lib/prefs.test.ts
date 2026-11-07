import { afterEach, expect, test } from "bun:test";
import { remember, remembered, rememberedAs, rememberedChoice } from "./prefs";

const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");

function install(storage: Partial<Storage> | undefined): void {
  Object.defineProperty(globalThis, "localStorage", { value: storage, configurable: true, writable: true });
}

function memory(): Storage {
  const values = new Map<string, string>();
  return {
    get length() { return values.size; },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => { values.delete(key); },
    setItem: (key, value) => { values.set(key, value); },
  };
}

afterEach(() => {
  if (original) Object.defineProperty(globalThis, "localStorage", original);
  else delete (globalThis as { localStorage?: Storage }).localStorage;
});

test("preferences round-trip, validate what they read and forget on null", () => {
  install(memory());
  remember("tab", "plan");
  expect(remembered("tab")).toBe("plan");
  expect(rememberedChoice("tab", "summary", ["summary", "plan"] as const)).toBe("plan");
  remember("tab", "deleted-tab");
  expect(rememberedChoice("tab", "summary", ["summary", "plan"] as const)).toBe("summary");
  remember("width", "320");
  expect(rememberedAs("width", (v) => Number(v) || null, 290)).toBe(320);
  expect(rememberedAs("missing", (v) => Number(v) || null, 290)).toBe(290);
  remember("tab", null);
  expect(remembered("tab")).toBeNull();
});

test("unavailable or refusing storage falls back and drops writes", () => {
  install(undefined);
  expect(remembered("tab")).toBeNull();
  expect(() => remember("tab", "plan")).not.toThrow();
  install({
    getItem: () => { throw new Error("SecurityError"); },
    setItem: () => { throw new Error("QuotaExceededError"); },
    removeItem: () => { throw new Error("SecurityError"); },
  });
  expect(rememberedChoice("tab", "summary", ["summary", "plan"] as const)).toBe("summary");
  expect(() => remember("tab", "plan")).not.toThrow();
  expect(() => remember("tab", null)).not.toThrow();
});
