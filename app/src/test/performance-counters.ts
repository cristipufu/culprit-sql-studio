import type { PerformanceCounterEntry } from "../gen/PerformanceCounterEntry";
import type { PerformanceCounterReport } from "../gen/PerformanceCounterReport";
import type { Report } from "../gen/Report";
import type { Metric } from "../gen/Metric";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";
import { healthReportFixture } from "./health-report";

export const EXACT_PERFORMANCE_COUNTER = "9007199254740993 KiB";

/** Derived presentation data only; native meaning and source proof are verified in Rust. */
export function performanceCounterReport(count = 12): PerformanceCounterReport {
  return partial<PerformanceCounterReport>({
    population: "shared-server", context: null, native_source: null, database: null, native_database_id: null,
    source_state: "partial", source_reason: null, read_complete: false,
    native_observations: 1, observed_observations: 1, native_counter_rows: count,
    observed_counter_rows: count, presented_counter_rows: count, entries_limited: false, fields_withheld: false,
    title: t("view.performance_counters.title"), summary: t("view.performance_counters.summary.partial"),
    limitations: [t("view.performance_counters.limit.scope"), t("view.performance_counters.limit.lifetime"),
      t("view.performance_counters.limit.cause"), t("view.performance_counters.limit.overlap")],
    ratios: null,
    missing_fields: [partial<Metric>({ label: t("view.performance_counters.field.maximum_workspace"), text: t("view.performance_counters.unknown"), key: "maximum_workspace" })],
    entries: Array.from({ length: count }, (_, index) => partial<PerformanceCounterEntry>({
      resource_governor: null,
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
  return healthReportFixture({
    snapshot: { id: "counter-presentation", server: { engine: "mssql" }, resources: [], pressure: null,
      sources: [{ source: "performance counters", state: view ? "partial" : "unavailable", reason: null }],
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    performance_counters: view ? [view] : [],
  });
}

/** Canonical presentation fixture only; pairing/native semantics are exercised by the shared Rust corpus. */
export function commonPerformanceCounterReport(missingBase = false): PerformanceCounterReport {
  const report = performanceCounterReport(3);
  report.read_complete = true;
  report.entries = report.entries.map((entry, index) => {
    const id = ["tempdb_free", "page_reads", "errors"][index]!;
    const gauge = index === 0;
    return {
      ...entry,
      field: `common_${id}`,
      family: index === 0 ? "temporary-tables" : index === 1 ? "buffer-cache" : "errors",
      family_label: t(`view.performance_counters.family.${index === 0 ? "temporary_tables" : index === 1 ? "buffer_cache" : "errors"}`),
      native_object: `SQLServer:${index === 0 ? "Transactions" : index === 1 ? "Buffer Manager" : "SQL Errors"}`,
      native_name: ["Free Space in tempdb (KB)", "Page reads/sec", "Errors/sec"][index]!,
      native_instance: index === 2 ? "Info Errors" : null,
      nature: gauge ? "gauge" : "cumulative",
      native_type: gauge ? "65792" : "272696576",
      unit: gauge ? { kind: "kibibytes" } : index === 1 ? { kind: "pages", bytes_per_page: 8192 } : { kind: "count" },
      raw_value: "9007199254740993",
      metric: partial<Metric>({ label: t(`view.performance_counters.field.common_${id}`), key: `common_${id}`,
        text: index === 0 ? EXACT_PERFORMANCE_COUNTER : index === 1 ? "9007199254740993 pages" : "9007199254740993" }),
      explanation: t(`view.performance_counters.explain.${index === 0 ? "common_tempdb" : index === 1 ? "common_pages" : "errors"}`),
    } satisfies PerformanceCounterEntry;
  });
  report.ratios = {
    source_state: "partial", source_reason: null, read_complete: true, declared_ratios: 1, presented_ratios: 1,
    entries_limited: false, title: t("view.counter_ratios.title"), summary: t("view.counter_ratios.summary.partial"),
    limitations: [t("view.counter_ratios.limit.aggregate")],
    entries: [{
      observation_key: "common-buffer-ratio", kind: "buffer-cache-hit", scope: "server",
      observed_at: "2020-01-01T12:00:00.500000000Z", native_object: "SQLServer:Buffer Manager", native_instance: null,
      issues: missingBase ? ["missing-base"] : [],
      title: t("view.performance_counters.buffer_cache_ratio.title"),
      explanation: t("view.performance_counters.buffer_cache_ratio.explanation"),
      metrics: [
        partial<Metric>({ label: t("view.counter_ratios.metric.numerator"), key: "view.counter_ratios.metric.numerator", text: "3" }),
        partial<Metric>({ label: t("view.counter_ratios.metric.base"), key: "view.counter_ratios.metric.base", text: missingBase ? t("view.counter_ratios.unknown") : "4" }),
        ...(!missingBase ? [partial<Metric>({ label: t("view.counter_ratios.metric.percent"), key: "view.counter_ratios.metric.percent", text: t("view.counter_ratios.percent", { value: "75.00" }) })] : []),
      ],
    }],
  };
  return report;
}
