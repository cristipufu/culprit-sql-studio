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
      { key: "last_good_checkdb_without_offset", label: t("view.database_properties.field.last_good_checkdb"), value: "2019-12-28T23:10:00.003", native_code: null, native_description: null },
      { key: "target_recovery_time_seconds", label: t("view.database_properties.field.target_recovery_time_seconds"), value: "0", native_code: null, native_description: null },
      { key: "auto_shrink", label: t("view.database_properties.field.auto_shrink"), value: t("view.database_properties.disabled"), native_code: null, native_description: null },
      { key: "in_standby", label: t("view.database_properties.field.in_standby"), value: t("view.database_properties.no"), native_code: null, native_description: null },
      { key: "delayed_durability", label: t("view.database_properties.field.delayed_durability"), value: t("view.database_properties.disabled"), native_code: "0", native_description: "DISABLED" },
      { key: "broker_enabled", label: t("view.database_properties.field.broker_enabled"), value: t("view.database_properties.disabled"), native_code: null, native_description: null },
      { key: "containment", label: t("view.database_properties.field.containment"), value: t("view.database_properties.state.not_contained"), native_code: "0", native_description: "NONE" },
      { key: "encrypted", label: t("view.database_properties.field.encrypted"), value: t("view.database_properties.disabled"), native_code: null, native_description: null },
      { key: "ledger_enabled", label: t("view.database_properties.field.ledger_enabled"), value: t("view.database_properties.no"), native_code: null, native_description: null },
      { key: "change_feed_enabled", label: t("view.database_properties.field.change_feed_enabled"), value: t("view.database_properties.disabled"), native_code: null, native_description: null },
      { key: "published", label: t("view.database_properties.field.published"), value: t("view.database_properties.no"), native_code: null, native_description: null },
      { key: "subscribed", label: t("view.database_properties.field.subscribed"), value: t("view.database_properties.no"), native_code: null, native_description: null },
      { key: "merge_published", label: t("view.database_properties.field.merge_published"), value: t("view.database_properties.no"), native_code: null, native_description: null },
      { key: "distributor", label: t("view.database_properties.field.distributor"), value: t("view.database_properties.no"), native_code: null, native_description: null },
      { key: "accelerated_recovery_enabled", label: t("view.database_properties.field.accelerated_recovery_enabled"), value: t("view.database_properties.disabled"), native_code: null, native_description: null },
    ], limitations: [t("view.database_properties.limit.scope"), t("view.database_properties.limit.state"), t("view.database_properties.limit.read"), t("view.database_properties.limit.features"), t("core.engines.mssql.integrity_history")],
  });
}
export function databasePropertiesHealthReport(view: DatabasePropertiesReport | null = databasePropertiesReport()): Report {
  return partial<Report>({ snapshot: { id: "database-properties-test", server: { engine: "mssql" }, resources: [], pressure: null,
    sources: [{ source: "database-properties", state: view ? "partial" : "unavailable", reason: null }],
    operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    database_properties: view ?? undefined,
  });
}
