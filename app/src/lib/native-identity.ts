import type { NativeIdentity } from "../gen/NativeIdentity";

/** A local lookup key for exact native references; this neither recreates a proof hash nor infers an owner. */
export function nativeIdentityKey(identity: NativeIdentity): string {
  const qualifiers = Object.entries(identity.qualifiers).sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0);
  return JSON.stringify([identity.id, qualifiers]);
}
