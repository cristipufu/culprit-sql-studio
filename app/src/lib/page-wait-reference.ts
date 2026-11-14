import type { ActivityKey } from "../gen/ActivityKey";
import type { ObservedWaitResource } from "../gen/ObservedWaitResource";
import type { PageWaitTaskReference } from "../gen/PageWaitTaskReference";
import { nativeIdentityKey } from "./native-identity";

/** Exact local references locate the original affected request and resource without rebuilding a proof hash. */
export function pageWaitReferenceKey(request: ActivityKey, resource: ObservedWaitResource, task?: PageWaitTaskReference | null): string {
  return JSON.stringify([
    request.session.id, request.session.generation,
    request.request ? [request.request.id, request.request.generation] : null,
    request.transaction, resource.kind, nativeIdentityKey(resource.native),
    task ? [task.task_address, task.execution_context_id] : null,
  ]);
}
