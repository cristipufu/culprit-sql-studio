import nativeCases from "../../../fixtures/session-census.sample.json" with { type: "json" };
import type { ActivityCensusRead } from "../gen/ActivityCensusRead";
import type { LiveState } from "../gen/LiveState";
import type { Session } from "../gen/Session";
import type { SessionCensusReport } from "../gen/SessionCensusReport";
import { fromRust, partial } from "./fixtures";

/** Rust's real pure-view output; fixture projection supplies no action owner or native admission. */
export function sessionCensusCase(name: keyof typeof nativeCases = "idle") {
  return fromRust<{ read: ActivityCensusRead; report: SessionCensusReport }>(nativeCases[name]);
}
export function sessionCensusLive(name: keyof typeof nativeCases = "idle"): LiveState {
  const { read, report } = sessionCensusCase(name);
  return partial<LiveState>({
    taken_at: report.context.observed_until,
    activity_census: read,
    session_census: report,
    sessions: read.observations.map((row) => partial<Session>({
      activity_key: row.key,
      id: row.key.session.id,
      state: row.state,
      command: row.command,
      database: row.database,
      application: row.client?.application,
      sql: null,
    })),
    waits: [],
    notes: [],
    session_actions: [],
    blocking: null,
  });
}
