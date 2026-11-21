import type { PoolSettingsReport } from "../gen/PoolSettingsReport";
import { t } from "../lib/i18n";
import { tableMemoryHealthReport } from "./table-memory";

/** Presentation-only counterpart of the six-field shared Rust corpus; no collection or native qualification. */
export function poolSettingsReport(): PoolSettingsReport {
  const target = { engine: "mssql" as const, kind: "pool" as const, namespace: { id: "pool-logical-server", qualifiers: { kind: "azure-sql-logical-server" } }, native: { id: "recorded-elastic-pool", qualifiers: {} } };
  const names = ["min_cpu", "max_cpu", "cap_cpu", "min_memory", "max_memory", "pool_max_io"] as const;
  const values = ["0", "80", "100", null, "75", "2048"];
  const labels = [t("view.pool_settings.field.min_cpu"), t("view.pool_settings.field.max_cpu"), t("view.pool_settings.field.cap_cpu"), t("view.pool_settings.field.min_memory"), t("view.pool_settings.field.max_memory"), t("view.pool_settings.field.pool_max_io")];
  const meanings = [t("view.pool_settings.meaning.min_cpu"), t("view.pool_settings.meaning.max_cpu"), t("view.pool_settings.meaning.cap_cpu"), t("view.pool_settings.meaning.min_memory"), t("view.pool_settings.meaning.max_memory"), t("view.pool_settings.meaning.pool_max_io")];
  return {
    target: { scope: "shared", identity: target }, elastic_pool: target, elastic_pool_error: null, member_database: "recorded-member", logical_server: "pool-logical-server", context: { kind: "native", context: { target: { scope: "shared", identity: target }, partition: { kind: "target" }, observed_from: "2026-11-01T00:00:01Z", observed_until: "2026-11-01T00:00:03Z", database_writable: null } },
    native_source: { source: "pool-resource-governance-settings", state: "collected", records: 6, complete_scope: true, error_code: null, stopped: null },
    source_state: "collected", native_rows: 6, observed_rows: 6, read_complete: true, observed_at: "2026-11-01T00:00:02Z",
    title: t("view.pool_settings.title"), summary: t("view.pool_settings.summary"),
    fields: names.map((key, index) => ({ key, label: labels[index]!, native_value: values[index] ?? null, unit: key === "pool_max_io" ? "io-operations-per-second" : "percent", value: values[index] == null ? null : key === "pool_max_io" ? t("view.pool_settings.iops", { value: values[index]! }) : `${values[index]}%`, explanation: meanings[index]! })),
    limitations: [t("view.pool_settings.limit.scope"), t("view.pool_settings.limit.meaning"), t("view.pool_settings.limit.membership"), t("view.pool_settings.limit.clock")],
    investigation: t("view.pool_settings.investigate"), verify: t("view.pool_settings.verify"),
  };
}
export function poolSettingsHealthReport(pool = poolSettingsReport()) {
  const report = tableMemoryHealthReport([]); report.pool_settings = pool; return report;
}

/** Same presentation values under a selected database; native scope stays pool regardless of membership. */
export function databasePoolSettingsReport(membership: "none" | "unavailable" = "none"): PoolSettingsReport {
  const report = poolSettingsReport();
  const identity = { engine: "mssql" as const, database: "recorded-member", location: { kind: "server" as const, host: "pool-logical-server", port: 1433, instance: null }, incarnation: "11111111-1111-1111-1111-111111111111" };
  const target = { scope: "database" as const, identity };
  report.target = target;
  report.elastic_pool = null;
  report.elastic_pool_error = membership === "unavailable" ? "permission_denied" : null;
  report.summary = membership === "unavailable" ? t("view.pool_settings.summary.membership_unavailable") : t("view.pool_settings.summary.unpooled");
  if (report.context?.kind === "native") report.context.context.target = target;
  return report;
}
