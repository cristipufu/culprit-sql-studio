import type { DatabasePropertiesReport } from "../gen/DatabasePropertiesReport";
import type { Report } from "../gen/Report";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";

/** Controlled display values only; native code interpretation and validation stay in the Rust view. */
export function databasePropertiesReport(): DatabasePropertiesReport {
  return partial<DatabasePropertiesReport>({
    title: t("view.database_properties.title"), summary: t("view.database_properties.summary.recorded"),
    context: { kind: "collector", observed_from: "2020-01-01T12:00:00Z", observed_until: "2020-01-01T12:00:01Z" },
    observed_at: "2020-01-01T12:00:00.0000001Z", native_database_id: "5", source_state: "partial", read_complete: false,
    fields: [
      { key: "catalog_state", label: t("view.database_properties.field.catalog_state"), value: null, native_code: "255", native_description: "ONLINE" },
      { key: "updateability", label: t("view.database_properties.field.updateability"), value: t("view.database_properties.read_only"), native_code: null, native_description: "READ_ONLY" },
      { key: "catalog_read_only", label: t("view.database_properties.field.catalog_read_only"), value: t("view.database_properties.no"), native_code: null, native_description: null },
      { key: "target_recovery_time_seconds", label: t("view.database_properties.field.target_recovery_time_seconds"), value: "0", native_code: null, native_description: null },
    ], limitations: [t("view.database_properties.limit.scope"), t("view.database_properties.limit.state"), t("view.database_properties.limit.read")],
  });
}
export function databasePropertiesHealthReport(view: DatabasePropertiesReport | null = databasePropertiesReport()): Report {
  return partial<Report>({ snapshot: { id: "database-properties-test", server: { engine: "mssql" }, resources: [], pressure: null,
    sources: [{ source: "database-properties", state: view ? "partial" : "unavailable", reason: null }],
    operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    database_properties: view ?? undefined,
  });
}
