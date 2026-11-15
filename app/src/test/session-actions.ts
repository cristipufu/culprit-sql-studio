import type { SessionActionRoute } from "../gen/SessionActionRoute";
import type { SessionActionTarget } from "../gen/SessionActionTarget";

/** Synthetic native-shaped control targets for pure/UI tests, never a native-cost or source-identity proof. */
export function sessionActionTarget(session = "30"): SessionActionTarget {
  return {
    context: {
      target: { scope: "database", identity: {
        engine: "postgres", database: "native-database", incarnation: "native-database-generation",
        location: { kind: "server", host: "native.fixture.invalid", port: 5432, instance: null },
      } },
      partition: {
        kind: "instance-local",
        member: {
          engine: "postgres", kind: "instance",
          namespace: { id: "synthetic-native-namespace", qualifiers: { cluster: "fixture", boundary: "test" } },
          native: { id: "synthetic-native-member", qualifiers: {} },
        },
        epoch: "synthetic-native-process-generation",
      },
      observed_from: "2026-11-15T10:00:00Z", observed_until: "2026-11-15T10:00:01Z", database_writable: true,
    },
    activity: {
      session: { id: session, generation: `native-session-generation-${session}` },
      request: { id: "native-request", generation: `native-request-generation-${session}` },
      transaction: null,
    },
    statement: { id: `native-statement-${session}`, qualifiers: { native: "fixture", scope: "request" } },
  };
}

export function sessionActionRoute(connection = "pg-demo"): SessionActionRoute {
  return { connection, revision: "a".repeat(64) };
}
