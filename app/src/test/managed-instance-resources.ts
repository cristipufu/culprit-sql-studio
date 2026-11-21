import type { ManagedInstanceResourceReport } from "../gen/ManagedInstanceResourceReport";
import { t } from "../lib/i18n";
import { healthReportFixture } from "./health-report";

/** Presentation-only counterpart of the canonical Rust corpus, not native platform or acquisition proof. */
export function managedInstanceResourceReport(): ManagedInstanceResourceReport {
  return {
    title: t("view.mi_resources.title"), summary: t("view.mi_resources.summary", { n: 1, count: "1" }), source_state: "collected", read_complete: true,
    native_source: { source: "managed-instance-resource-history", state: "collected", records: 1, complete_scope: true, error_code: null, stopped: null },
    from: "2020-01-01T06:00:00Z", until: "2020-01-01T06:01:00Z", observed_at: "2020-01-01T06:10:00Z", native_rows: 1, recorded_rows: 1, observed_rows: 1,
    layout: { interval: "available", cpu: "available", virtual_core_count: "available", reserved_storage_mb: "available", storage_space_used_mb: "available", io_request: "available", io_bytes_read: "available", io_bytes_written: "available", resource_type: "available", resource_name: "available", sku: "available", hardware_generation: "unsupported" },
    intervals: [{ started_at: "2020-01-01T06:00:00.000000000Z", ended_at: "2020-01-01T06:00:15.000000000Z", fields: [
      { key: "avg_cpu_percent", label: t("view.mi_resources.field.cpu"), availability: "available", native_value: "12.34", value: "12.34", missing_reason: null },
      { key: "io_request", label: t("view.mi_resources.field.requests"), availability: "available", native_value: "9007199254740993", value: "9007199254740993", missing_reason: null },
      { key: "io_bytes_written", label: t("view.mi_resources.field.written"), availability: "available", native_value: "0", value: "0", missing_reason: null },
      { key: "hardware_generation", label: t("view.mi_resources.field.hardware"), availability: "unsupported", native_value: null, value: null, missing_reason: t("view.mi_resources.field_unsupported") },
    ] }],
    limitations: [t("view.mi_resources.limit.scope"), t("view.mi_resources.limit.publication"), t("view.mi_resources.limit.meaning"), t("view.mi_resources.limit.clock")], investigation: t("view.mi_resources.investigate"), verify: t("view.mi_resources.verify"),
  };
}
export function managedInstanceHistoryHealthReport(history = managedInstanceResourceReport()) {
  return healthReportFixture({ snapshot: { id: "mi-original-report", server: { engine: "postgres", database: "later-current-db" }, options: {}, operations: {} }, managed_instance_resources: history });
}
