import { useSyncExternalStore } from "react";
import english from "../../../locales/en.json";
import { type Args, type Locale, formatMessage } from "./message-format";

/** A language's own description, from its catalog's `_meta`. */
export type LanguageMeta = Locale & { name: string; english_name: string };

/** A language's catalog: `locales/<code>.json`, its keys and messages beside its `_meta`. */
export type Catalog = { _meta: LanguageMeta } & Record<string, unknown>;

/** A language the app can be shown in. */
export type Language = { code: string; name: string; englishName: string };

const KEY = "culprit.language";
const ENGLISH = english as unknown as Catalog;
const catalogs = new Map<string, Catalog>([["en", ENGLISH]]);
const listeners = new Set<() => void>();
let current = "en";

/**
 * Every catalog the build found (`locales/*.json`, see catalogs.ts): a language is one file, so adding one needs no
 * code. A file whose name isn't a language code (`en`, `pt-BR`) is not a catalog.
 */
export function registerCatalogs(found: Record<string, Catalog>): void {
  for (const [code, catalog] of Object.entries(found)) {
    if (/^[a-z]{2,3}(-[A-Z][A-Za-z]{1,3})?$/.test(code) && catalog?._meta) catalogs.set(code, catalog);
  }
  current = chosen();
  notify();
}

/** The languages the app has a catalog for, English first, then by their own names. */
export function languages(): Language[] {
  return [...catalogs.entries()]
    .map(([code, c]) => ({ code, name: c._meta.name, englishName: c._meta.english_name }))
    .sort((a, b) => (a.code === "en" ? -1 : b.code === "en" ? 1 : a.name.localeCompare(b.name)));
}

/** The language the app is shown in. */
export function language(): string {
  return current;
}

/** Show the app in `code` from now on, on this device. */
export function setLanguage(code: string): void {
  if (!catalogs.has(code) || code === current) return;
  current = code;
  try {
    localStorage.setItem(KEY, code);
  } catch {
    // Remembering is a convenience; the choice still applies now.
  }
  notify();
}

/**
 * The message `key` in the app's language, with `args` in it. A key the language hasn't translated reads in English;
 * one English doesn't have either reads as the key itself, so a missing message shows rather than vanishing.
 */
export function t(key: string, args: Args = {}): string {
  const catalog = catalogs.get(current) ?? ENGLISH;
  const own = catalog[key];
  const message = typeof own === "string" ? own : ENGLISH[key];
  if (typeof message !== "string") return key;
  const meta = typeof own === "string" ? catalog._meta : ENGLISH._meta;
  return formatMessage(message, args, meta);
}

/** The app's language, re-rendering the component when it changes. */
export function useLanguage(): string {
  return useSyncExternalStore(subscribe, language, language);
}

/** Be told when the language changes (to re-render, or tell the desktop app). */
export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function notify() {
  for (const listener of listeners) listener();
}

/** The remembered choice, else the system's language when the app has it, else English. */
function chosen(): string {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved && catalogs.has(saved)) return saved;
  } catch {
    // No storage: fall through to the system's language.
  }
  const system = typeof navigator === "undefined" ? [] : (navigator.languages ?? [navigator.language]);
  for (const tag of system) {
    if (catalogs.has(tag)) return tag;
    const base = tag.split("-")[0]!;
    if (catalogs.has(base)) return base;
  }
  return "en";
}
