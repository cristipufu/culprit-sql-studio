import type { ActivityKey } from "../gen/ActivityKey";
import type { ObservedWaitResource } from "../gen/ObservedWaitResource";
import { nativeIdentityKey } from "./native-identity";

/** Exact local references locate the original affected request and resource without rebuilding a proof hash. */
export function pageWaitReferenceKey(request: ActivityKey, resource: ObservedWaitResource): string {
  return JSON.stringify([
    request.session.id, request.session.generation,
    request.request ? [request.request.id, request.request.generation] : null,
    request.transaction, resource.kind, nativeIdentityKey(resource.native),
  ]);
}
