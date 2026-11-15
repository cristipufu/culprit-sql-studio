import type { CollectionTarget } from "../gen/CollectionTarget";
import type { DatabaseLocation } from "../gen/DatabaseLocation";
import type { Session } from "../gen/Session";
import type { SessionAction } from "../gen/SessionAction";
import type { SessionActionRequest } from "../gen/SessionActionRequest";
import type { SessionActionRoute } from "../gen/SessionActionRoute";
import type { SharedResourceIdentity } from "../gen/SharedResourceIdentity";
import { nativeIdentityKey } from "./native-identity";
import { liveSessionId } from "./engines";

/** A local identity comparison, not a native proof hash. Observation clocks and display values establish no actor. */
export function sessionActionKey(request: SessionActionRequest): string {
  const { context, activity, statement } = request.target;
  const partition = context.partition;
  return JSON.stringify([
    request.route.connection, request.route.revision, request.action,
    targetKey(context.target),
    partition.kind === "instance-local" ? [partition.kind, partition.member ? resourceKey(partition.member) : null, partition.epoch] : [partition.kind],
    context.database_writable,
    [activity.session.id, activity.session.generation],
    ...(request.action === "cancel" ? [
      activity.request ? [activity.request.id, activity.request.generation] : null,
      statement ? nativeIdentityKey(statement) : null,
    ] : []),
  ]);
}

/** Only a source-issued target on the displayed route can become a confirmation candidate. */
export function sessionActionCandidate(
  connection: string,
  route: SessionActionRoute | null,
  session: Session,
  actions: readonly SessionAction[],
  action: SessionAction,
): SessionActionRequest | null {
  const target = session.action_target;
  if (!route || route.connection !== connection || !route.revision || !target || !actions.includes(action)) return null;
  if (target.context.target.scope !== "database"
    || liveSessionId(target.context.target.identity.engine, target.activity.session) !== session.id) return null;
  const partition = target.context.partition;
  if (partition.kind !== "instance-local" || !partition.member || !partition.epoch
    || !target.activity.session.id || !target.activity.session.generation) return null;
  if (action === "cancel" && (!target.activity.request?.id || !target.activity.request.generation)) return null;
  return { route, target, action };
}

/** A pending request cannot silently adopt a new route, actor, request or offered action. */
export function sessionActionIsCurrent(
  pending: SessionActionRequest,
  connection: string,
  route: SessionActionRoute | null,
  session: Session,
  actions: readonly SessionAction[],
): boolean {
  const current = sessionActionCandidate(connection, route, session, actions, pending.action);
  return current !== null && sessionActionKey(current) === sessionActionKey(pending);
}

function resourceKey(identity: SharedResourceIdentity): unknown[] {
  return [identity.engine, identity.kind, nativeIdentityKey(identity.namespace), nativeIdentityKey(identity.native)];
}

function targetKey(target: CollectionTarget): unknown[] {
  if (target.scope === "shared") return [target.scope, resourceKey(target.identity)];
  const database = target.identity;
  return [target.scope, database.engine, database.database, locationKey(database.location), database.incarnation];
}

function locationKey(location: DatabaseLocation): unknown[] {
  switch (location.kind) {
    case "server": return [location.kind, location.host, location.port, location.instance];
    case "socket": return [location.kind, location.directory, location.port];
    case "file": return [location.kind, location.path];
    case "unknown": return [location.kind];
  }
}
