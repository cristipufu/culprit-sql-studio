import type { PerformanceCounterEntry } from "../gen/PerformanceCounterEntry";
import type { PerformanceCounterReport } from "../gen/PerformanceCounterReport";
import type { Report } from "../gen/Report";
import type { Metric } from "../gen/Metric";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";

export const EXACT_PERFORMANCE_COUNTER = "9007199254740993 KiB";

/** Derived presentation data only; native meaning and source proof are verified in Rust. */
export function performanceCounterReport(count = 12): PerformanceCounterReport {
  return partial<PerformanceCounterReport>({
    context: null, native_source: null, database: null, native_database_id: null,
    source_state: "partial", source_reason: null, read_complete: false,
    native_observations: 1, observed_observations: 1, native_counter_rows: count,
    observed_counter_rows: count, presented_counter_rows: count, entries_limited: false, fields_withheld: false,
    title: t("view.performance_counters.title"), summary: t("view.performance_counters.summary.partial"),
    limitations: [t("view.performance_counters.limit.scope"), t("view.performance_counters.limit.lifetime"),
      t("view.performance_counters.limit.cause"), t("view.performance_counters.limit.overlap")],
    ratios: null,
    missing_fields: [partial<Metric>({ label: t("view.performance_counters.field.maximum_workspace"), text: t("view.performance_counters.unknown"), key: "maximum_workspace" })],
    entries: Array.from({ length: count }, (_, index) => partial<PerformanceCounterEntry>({
      observation_key: "performance-observation", counter_key: `counter-${index}`, field: "granted_workspace", family: "workspace-memory",
      family_label: t("view.performance_counters.family.workspace_memory"), availability: "observed",
      native_object: "SQLServer:Memory Manager", native_name: `native-counter-${index}`, native_instance: null,
      native_type: "65792", observed_at: "2020-01-01T12:00:00.500000000Z", scope: "server", nature: "gauge", unit: { kind: "kibibytes" },
      reset: { kind: "unknown" }, raw_value: index === 1 ? "0" : "9007199254740993",
      metric: partial<Metric>({ key: "granted_workspace", label: `native-counter-${index}`, text: index === 1 ? "0 KiB" : EXACT_PERFORMANCE_COUNTER }),
      explanation: t("view.performance_counters.explain.workspace_memory"),
    })),
  });
}

export function performanceCounterHealthReport(view: PerformanceCounterReport | null = performanceCounterReport()): Report {
  return partial<Report>({
    snapshot: { id: "counter-presentation", server: { engine: "mssql" }, resources: [], pressure: null,
      sources: [{ source: "performance counters", state: view ? "partial" : "unavailable", reason: null }],
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    performance_counters: view ?? undefined,
  });
}
