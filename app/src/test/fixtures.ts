import { fromRust as rustJson } from "../lib/rust-json";

// Fixtures for the UI tests. Each helper holds the one cast its kind of fixture needs, so a test never writes
// `as unknown as` and the fields it does write stay checked against the types generated from the Rust model.

/** Every field optional at every depth, as a test fixture that sets only what its test reads. */
export type DeepPartial<T> = T extends (infer U)[] ? DeepPartial<U>[] : T extends object ? { [K in keyof T]?: DeepPartial<T[K]> } : T;

/**
 * A fixture with only the fields its test reads. The fields it sets are checked against `T`, so a renamed or retyped
 * field fails to compile; the ones it leaves out are undefined, which the code under test must not depend on.
 */
export function partial<T>(value: DeepPartial<T>): T {
  return value as unknown as T;
}

/** A copy of JSON the Rust model serialized from `T` (see lib/rust-json.ts), for a test to change freely. */
export function fromRust<T>(json: unknown): T {
  return structuredClone(rustJson<T>(json));
}
