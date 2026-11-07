// Preferences this device remembers: a tab, a width, a switch. Storage can be missing or refuse (a private window,
// blocked site data, a full quota), so a read falls back to its default and a failed write is dropped; a preference
// is never worth an error.

function storage(): Storage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

/** The remembered text for `key`, or null. */
export function remembered(key: string): string | null {
  try {
    return storage()?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

/** The remembered value for `key` when it is one of `allowed`, otherwise `fallback`. */
export function rememberedChoice<T extends string>(key: string, fallback: T, allowed: readonly T[]): T {
  const value = remembered(key);
  return value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;
}

/** The remembered value for `key` read by `parse` (null when it doesn't parse), otherwise `fallback`. */
export function rememberedAs<T>(key: string, parse: (value: string) => T | null, fallback: T): T {
  const value = remembered(key);
  return (value === null ? null : parse(value)) ?? fallback;
}

/** Remember `value` for `key`; `null` forgets it. */
export function remember(key: string, value: string | null): void {
  try {
    const store = storage();
    if (value === null) store?.removeItem(key);
    else store?.setItem(key, value);
  } catch {}
}
