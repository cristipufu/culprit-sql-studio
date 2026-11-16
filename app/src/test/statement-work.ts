import type { OperationalObservation } from "../gen/OperationalObservation";
import type { StatementCounterSnapshot } from "../gen/StatementCounterSnapshot";
import type { StatementWorkReport } from "../gen/StatementWorkReport";
import { partial } from "./fixtures";

export const STATEMENT_SOURCE = "performance_schema.events_statements_summary_by_digest";

export function statementPoint(index = 0): OperationalObservation {
  return partial<OperationalObservation>({
    source: STATEMENT_SOURCE, key: String(index).padStart(64, "a"),
    fact: { kind: "statement-counters", data: {
      identity: { query_id: "0123456789abcdef0123456789abcdef", qualifiers: { schema: `schema-${index}` } },
      scope: "database", observed_at: "2020-01-01T12:00:00.123456Z", reset: { kind: "unknown" },
      totals: [
        { key: { kind: "executions" }, unit: { kind: "count" }, total: { kind: "integer", value: "17" } },
        { key: { kind: "errors" }, unit: { kind: "count" }, total: { kind: "integer", value: "9007199254740993" } },
        { key: { kind: "warnings" }, unit: { kind: "count" }, total: { kind: "integer", value: "0" } },
      ], distributions: [],
    } },
  });
}

export function statementEvidence(observations = [statementPoint()]): StatementCounterSnapshot {
  return partial<StatementCounterSnapshot>({ source: STATEMENT_SOURCE,
    context: { observed_from: "2020-01-01T12:00:00Z", observed_until: "2020-01-01T12:00:01Z" },
    coverage: { source: STATEMENT_SOURCE, records: observations.length, state: "partial", complete_scope: false },
    observations,
  });
}

/** Presentation fixtures supply Rust-shaped outcomes; the browser does not interpret native counter keys. */
export function statementReport(observations = [statementPoint()]): StatementWorkReport {
  return partial<StatementWorkReport>({ source: STATEMENT_SOURCE, source_state: "partial", source_reason: null,
    title: "Recorded statement work", summary: "Native counter points, with unknown reset history.",
    limitations: ["Warning counts do not count statements with warnings."],
    entries: observations.map((row) => {
      if (row.fact.kind !== "statement-counters") throw new Error("Statement counter fixture required");
      return { source: row.source, observation_key: row.key, identity: row.fact.data.identity,
        scope: row.fact.data.scope, observed_at: row.fact.data.observed_at, reset: row.fact.data.reset, work: [],
        outcomes: { errors: "9007199254740993", warnings: "0" }, outcome_metrics: [
          { key: "view.statement_work.outcome.errors", label: "Native error count", value: 0, unit: "text", text: "9007199254740993" },
          { key: "view.statement_work.outcome.warnings", label: "Native warning count", value: 0, unit: "text", text: "0" },
        ],
      };
    }),
  });
}
