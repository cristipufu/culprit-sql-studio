import type { Snapshot } from "../gen/Snapshot";
import type { StatementGrantReference } from "../gen/StatementGrantReference";
import type { StatementGrantSnapshot } from "../gen/StatementGrantSnapshot";
import type { StatementGrantStatistics } from "../gen/StatementGrantStatistics";
import { partial } from "./fixtures";

export const GRANT_SOURCE = "memory grants per query";
export const ORIGINAL_CACHED_SQL = "SELECT [<script>cached-grant-canary</script>] FROM dbo.grant_input";
export const OTHER_CACHED_SQL = "SELECT unrelated_same_hash FROM dbo.other_input";

export function statementGrant(plan = "0x0102", text = ORIGINAL_CACHED_SQL): StatementGrantStatistics {
  return {
    context: {
      identity: { plan_handle: plan, sql_handle: "0x0304", statement_start_bytes: 0, statement_end_bytes: -1, created_without_offset: "2026-11-01T04:00:00.003", plan_generation: "1" },
      database_id: 7, query_hash: "0x0123456789ABCDEF", origin: "application", monitoring_template_id: null,
      source_text_bytes: String(text.length * 2), source_text_complete: true,
    },
    statement: { state: "available", text, sha256: "a".repeat(64), redaction_version: 1 },
    available_columns: ["execution_count", "total_grant_kb", "total_used_grant_kb"],
    executions: "40", last_execution_without_offset: "2026-11-01T04:00:00.997",
    total_grant_kibibytes: "83886080", total_used_kibibytes: "1638400",
    last_grant_kibibytes: null, last_used_kibibytes: null, min_grant_kibibytes: null, min_used_kibibytes: null,
    max_grant_kibibytes: null, max_used_kibibytes: null, reset: { kind: "unknown" },
  };
}

export function statementGrantRead(observations = [statementGrant()]): StatementGrantSnapshot {
  return {
    capture: { observation_id: "cached-grant-read-a", revision: { outbox_id: "owned-outbox", owner_epoch: "7", sequence: "12" } },
    description: {
      target: { scope: "database", identity: { engine: "mssql", database: "fixture", location: { kind: "server", host: "fixture.invalid", port: 1433, instance: null }, incarnation: null } },
      partition: { kind: "instance-local", member: { engine: "mssql", kind: "instance", namespace: { id: "native-instance-namespace", qualifiers: {} }, native: { id: "member-a", qualifiers: {} } }, epoch: "process-generation-1" },
      source: GRANT_SOURCE, continuity: "unverified",
      observation: { kind: "snapshot", data: {
        dataset: "statement-grants", observed_from: "2026-11-01T06:00:00Z", observed_until: "2026-11-01T06:00:01Z", records: observations.length,
        sources: [{ source: GRANT_SOURCE, state: "partial", records: observations.length, complete_scope: false, error_code: null, stopped: null }],
      } },
    },
    observations,
  };
}

export function statementGrantReference(read: StatementGrantSnapshot, row = read.observations[0]!): StatementGrantReference {
  return { snapshot_key: "b".repeat(64), observation_key: "c".repeat(64), capture: structuredClone(read.capture), identity: structuredClone(row.context.identity) };
}

export function statementGrantSnapshot(reads = [statementGrantRead()]): Snapshot {
  const first = reads[0]?.description.target;
  return partial<Snapshot>({
    identity: first?.scope === "database" ? structuredClone(first.identity) : null,
    server: { engine: "mssql", database: "fixture" }, pressure: { grants: reads },
    sources: [{ source: GRANT_SOURCE, state: "partial", reason: null }],
  });
}
