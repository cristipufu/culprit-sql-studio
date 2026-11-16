import english from "../../../locales/en.json" with { type: "json" };
import type { CompilationReport } from "../gen/CompilationReport";
import type { CompilationSnapshot } from "../gen/CompilationSnapshot";
import type { OperationalObservation } from "../gen/OperationalObservation";

export const COMPILATION_SOURCE = "query compilation";

export function compilationObservation(index = 0): OperationalObservation {
  return {
    source: COMPILATION_SOURCE,
    key: String(index).padStart(64, "a"),
    fact: { kind: "mssql-query-compilation", data: {
      statement: { state: "unavailable", reason: "not-collected" },
      origin: "unknown",
      identity: { query_id: String(index + 3), qualifiers: {
        context_settings_id: "5", object_id: "1999", query_hash: "0123456789ABCDEF",
        initial_compile_at: "2020-01-01T06:00:00Z",
      } },
      last_compile_at: "2020-01-01T06:01:00Z",
      compilations: "9007199254740993", average_duration_us: 1500.5,
      memory: { average_kb: 4096.5, last_kb: 8192, maximum_kb: 16384 },
      seconds_since_first_compile: -1n, reset: { kind: "unknown" },
    } },
  };
}

export function compilationEvidence(observations = [compilationObservation()]): CompilationSnapshot {
  return { source: COMPILATION_SOURCE, observed_from: "2026-11-13T10:00:00Z", observed_until: "2026-11-13T10:00:01Z", observations };
}

export function compilationReport(observations = [compilationObservation()]): CompilationReport {
  return {
    source_state: "partial", source_reason: "Some retained entries were not visible.",
    title: english["view.compilation.title"], summary: "Retained compilation observations.",
    limitations: ["Retained memory is independent of current allocation."],
    entries: observations.map((row) => {
      if (row.fact.kind !== "mssql-query-compilation") throw new Error("Compilation fixture required");
      return {
        observation_key: row.key, identity: row.fact.data.identity,
        title: `Native query ${row.fact.data.identity.query_id}`,
        explanation: "Retained compilation count; individual reasons are unresolved.",
        metrics: [{ key: "view.compilation.metric.count", label: english["view.compilation.metric.count"], value: 0, unit: "text", text: row.fact.data.compilations }],
        investigation: "Inspect this exact native statement context.", verify: "Compare identity-matched observations.",
      };
    }),
  };
}

export function compilationComparisonReport(rows = [compilationObservation()]): CompilationReport {
  const report = compilationReport(rows);
  report.comparison = {
    baseline: { id: "original-private-report-reference", taken_at: "2026-11-13T09:59:00Z",
      database: { engine: "mssql", database: "db", incarnation: "private-native-incarnation",
        location: { kind: "server", host: "private-original-native-host", port: 1433, instance: null } } },
    observed_from: "2026-11-13T09:59:00.000000001Z", observed_until: "2026-11-13T10:00:01.000000002Z",
    missing_before: 0, missing_after: 1, issues: ["partial-read", "unmatched-entries"],
    title: "Recorded compilation changes", summary: "One native entry compared; the read is partial.",
    limitations: ["A missing entry supplies no zero or recovery claim."],
    entries: report.entries.map((entry) => ({
      observation_key: entry.observation_key, baseline_observation_key: entry.observation_key,
      identity: { ...entry.identity, qualifiers: { ...entry.identity.qualifiers } },
      compilations: "18446744073709551615", last_compile_at: "2026-11-13T10:00:00.000000001Z",
      explanation: "Recorded compilation count increase: 18446744073709551615.",
      investigation: "Inspect the two original native query entries.",
      verify: "Re-read this native entry without replaying application SQL.",
    })),
  };
  return report;
}

export function unavailableCompilationComparisonReport(observations = [compilationObservation()]): CompilationReport {
  const report = compilationComparisonReport(observations);
  if (!report.comparison) throw new Error("Comparison fixture required");
  report.comparison.observed_from = null;
  report.comparison.observed_until = null;
  report.comparison.summary = "Compilation counts could not be compared between these reads.";
  report.comparison.issues = ["native-namespace-unavailable"];
  report.comparison.limitations = ["A native logical database incarnation was not recorded."];
  report.comparison.entries = [];
  return report;
}
