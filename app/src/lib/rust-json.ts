// JSON the Rust model serialized from a type the app also has (generated into src/gen): the browser demo's sidecars in
// fixtures/ and the shared cases in src/gen. Importing JSON widens its string unions to `string`, so TypeScript can't
// see that a file is a `T`; what makes it one is the Rust side, and a Rust test reads every file back as its types
// (crates/culprit-engine/tests: invariants, share, plan_view, plan_samples, dashboard, demo_sidecars). This is the one
// place the app takes that on trust.

/** A JSON module's contents as the generated type the Rust model wrote it from. */
export function fromRust<T>(json: unknown): T {
  return json as T;
}
