import type { TableMemoryReport } from "../gen/TableMemoryReport";
import type { Report } from "../gen/Report";
import { t } from "../lib/i18n";
import { healthReportFixture } from "./health-report";

/** Presentation-only native facts; Rust owns all evidence and source admission. */
export function tableMemoryReport(index = 0): TableMemoryReport {
  return {
    original: {
      context: {
        target: { scope: "database", identity: { engine: "mssql", database: "original-database", incarnation: null, location: { kind: "server", host: "private-original-host", port: 1433, instance: null } } },
        partition: { kind: "instance-local", member: { engine: "mssql", kind: "instance", namespace: { id: "private-machine", qualifiers: {} }, native: { id: "private-member", qualifiers: {} } }, epoch: "private-process-and-database" },
        observed_from: "2020-01-01T00:00:00Z", observed_until: "2020-01-01T00:00:01Z", database_writable: true,
      },
      target: { database_id: 7, object_id: 100 + index, created_without_offset: "2019-01-01T00:00:00.0000000" },
      metadata: { observed_at: "2020-01-01T00:00:00.5Z", schema: "dbo", name: `original-table-${index}`, memory_optimized: true, xtp_supported: true, dmv_permission: true, metadata_visible: true, layout_available: true },
    },
    source: { source: "memory-optimized table memory", state: "partial", reason: t("view.table_memory.partial") },
    native_rows: 1, observed_rows: 1, read_complete: true,
    title: t("view.table_memory.title"), summary: t("view.table_memory.recorded"),
    metrics: [
      { key: "view.table_memory.allocated_table", label: t("view.table_memory.allocated_table"), value: 0, unit: "text", text: "9007199254740993" },
      { key: "view.table_memory.used_table", label: t("view.table_memory.used_table"), value: 0, unit: "text", text: "0" },
      { key: "view.table_memory.allocated_indexes", label: t("view.table_memory.allocated_indexes"), value: 0, unit: "text", text: t("view.table_memory.unknown") },
      { key: "view.table_memory.used_indexes", label: t("view.table_memory.used_indexes"), value: 0, unit: "text", text: "4" },
    ],
    limitations: [t("view.table_memory.limit.point"), t("view.table_memory.limit.historical"), t("view.table_memory.limit.versions"), t("view.table_memory.limit.population"), t("view.table_memory.limit.quantities")],
  };
}
export function tableMemoryHealthReport(reports: TableMemoryReport[]): Report {
  return healthReportFixture({ table_memory: reports, performance_counters: [],
    snapshot: { id: "selected-table-presentation", server: { engine: "mssql" }, pressure: null, resources: [], sources: [],
      operations: { table_memory: [], spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
  });
}
