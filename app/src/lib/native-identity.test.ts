import { expect, test } from "bun:test";
import { nativeIdentityKey } from "./native-identity";
import type { NativeIdentity } from "../gen/NativeIdentity";

test("native references agree independently of qualifier insertion order without changing the source", () => {
  const identity = { id: "thread:18446744073709551615", qualifiers: { generation: "9007199254740993", source: "native" } };
  const original = structuredClone(identity);
  expect(nativeIdentityKey(identity)).toBe(nativeIdentityKey({ id: identity.id, qualifiers: { source: "native", generation: "9007199254740993" } }));
  expect(identity).toEqual(original);
});

test("generation, native namespace and exact identifiers remain distinct without delimiter collisions", () => {
  const identities: NativeIdentity[] = [
    { id: "01", qualifiers: { generation: "1", source: "native" } },
    { id: "1", qualifiers: { generation: "1", source: "native" } },
    { id: "01", qualifiers: { generation: "2", source: "native" } },
    { id: "01", qualifiers: { generation: "1", source: "another" } },
    { id: "01|generation:1|source:native", qualifiers: {} },
  ];
  expect(new Set(identities.map(nativeIdentityKey)).size).toBe(identities.length);
});
