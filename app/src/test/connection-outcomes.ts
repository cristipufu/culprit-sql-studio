import type { ConnectionOutcomeEntry } from "../gen/ConnectionOutcomeEntry";
import type { ConnectionOutcomeReport } from "../gen/ConnectionOutcomeReport";
import type { ConnectionOutcomeSnapshot } from "../gen/ConnectionOutcomeSnapshot";
import type { Report } from "../gen/Report";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";

export const CONNECTION_OUTCOME_SOURCE = "pg_stat_database";
export const EXACT_SESSION_COUNT = "9007199254740993";
export const DISPLAY_SESSION_COUNT = "9,007,199,254,740,993";

/** Presentation fixtures supply the derived Rust DTO; no browser fixture classifies native counters. */
export function connectionOutcomeEntry(index = 0): ConnectionOutcomeEntry {
  return {
    reference: { source: CONNECTION_OUTCOME_SOURCE, observation_key: `outcome-row-${index}` },
    scope: "database",
    title: `${t("view.connection_outcomes.entry.database")} ${index + 1}`,
    explanation: t("view.connection_outcomes.population.database"),
    investigation: t("view.connection_outcomes.investigation"),
    verify: t("view.connection_outcomes.verify"),
    metrics: [
      {
        kind: "sessions-established", native_key: "sessions",
        label: t("view.connection_outcomes.metric.sessions_established"), availability: "supported",
        value: { kind: "integer", value: EXACT_SESSION_COUNT }, display_value: DISPLAY_SESSION_COUNT,
        unit: { kind: "count" }, nature: "cumulative", reset: { kind: "unknown" }, rate: null,
        explanation: t("view.connection_outcomes.meaning.sessions_established"),
      },
      {
        kind: "sessions-lost-client", native_key: "sessions_abandoned",
        label: t("view.connection_outcomes.metric.sessions_lost_client"), availability: "supported",
        value: { kind: "integer", value: "0" }, display_value: "0",
        unit: { kind: "count" }, nature: "cumulative", reset: { kind: "unknown" }, rate: null,
        explanation: t("view.connection_outcomes.meaning.sessions_lost_client"),
      },
      {
        kind: "sessions-fatal", native_key: "sessions_fatal",
        label: t("view.connection_outcomes.metric.sessions_fatal"), availability: "unknown",
        value: null, display_value: null, unit: null, nature: null, reset: { kind: "unknown" }, rate: null,
        explanation: t("view.connection_outcomes.meaning.sessions_fatal"),
      },
      {
        kind: "session-time", native_key: "session_time",
        label: t("view.connection_outcomes.metric.session_time"), availability: "supported",
        value: { kind: "real", value: 123456.125 }, display_value: "2 min 3 s",
        unit: { kind: "milliseconds" }, nature: "cumulative", reset: { kind: "unknown" }, rate: null,
        explanation: t("view.connection_outcomes.meaning.session_time"),
      },
    ],
  };
}

export function connectionOutcomeReport(entryCount = 1): ConnectionOutcomeReport {
  return {
    target: { scope: "database", identity: { engine: "postgres", database: "private-database-canary",
      location: { kind: "server", host: "private-host-canary.invalid", port: 5432, instance: null }, incarnation: null } },
    context: { kind: "collector", observed_from: "2026-11-17T12:00:00.123456Z", observed_until: "2026-11-17T12:00:01.123456Z" },
    sources: [{ source: CONNECTION_OUTCOME_SOURCE, state: "partial", reason: null }],
    read_complete: false,
    // Include original unmapped rows and a read gap so the three counts cannot be conflated.
    native_rows: entryCount + 5, observed_rows: entryCount + 2, mapped_rows: entryCount,
    entries_limited: false,
    title: t("view.connection_outcomes.title"),
    summary: t("view.connection_outcomes.summary", { count: String(entryCount), shown: String(entryCount) }),
    limitations: [t("view.connection_outcomes.limit.current"), t("view.connection_outcomes.limit.lifetime"),
      t("view.connection_outcomes.limit.read"), t("view.connection_outcomes.limit.overlap")],
    entries: Array.from({ length: entryCount }, (_, index) => connectionOutcomeEntry(index)),
  };
}

export function unavailableConnectionOutcomes(): ConnectionOutcomeReport {
  const report = connectionOutcomeReport(0);
  report.sources[0] = { source: CONNECTION_OUTCOME_SOURCE, state: "unavailable", reason: t("view.connection_outcomes.limit.source") };
  report.native_rows = 0;
  report.observed_rows = 0;
  report.read_complete = true;
  report.limitations = [t("view.connection_outcomes.limit.no_facts")];
  return report;
}

/** An admitted source gap can be retained even when its explanation was withheld. */
export function unavailableConnectionOutcomeRead(): ConnectionOutcomeSnapshot {
  const report = unavailableConnectionOutcomes();
  return { target: report.target, context: report.context,
    sources: [{ source: CONNECTION_OUTCOME_SOURCE, state: "unavailable", records: 0, complete_scope: false,
      error_code: "permission_denied", stopped: null }], observations: [] };
}

export function connectionOutcomeHealthReport(view: ConnectionOutcomeReport | null = connectionOutcomeReport()): Report {
  return partial<Report>({
    snapshot: { id: "connection-outcome-presentation", server: { engine: "postgres" }, sources: [], resources: [],
      pressure: view ? null : { signals: [], files: [], replicas: [], connection_outcomes: unavailableConnectionOutcomeRead() },
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    connection_outcomes: view ?? undefined,
  });
}
