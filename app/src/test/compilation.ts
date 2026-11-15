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
