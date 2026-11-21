import type { IndexStat } from "../gen/IndexStat";
import { partial } from "./fixtures";
import { healthReportFixture } from "./health-report";
import type { IndexConfigurationReport } from "../gen/IndexConfigurationReport";
import { t } from "../lib/i18n";

/** Presentation-only values; the original Rust interpreter owns evidence admission. */
export function indexConfigurationReport(): IndexConfigurationReport {
  return {
    dataset: "indexes", context: null, title: t("view.index_configuration.title"), summary: t("view.index_configuration.summary", { count: 1 }),
    qualification: t("view.index_configuration.qualification"), source_state: "partial", source_reason: null,
    read_complete: null, native_rows: null, observed_rows: 1, presented_rows: 1, entries_limited: false,
    limitations: [t("view.index_configuration.limit.scope"), t("view.index_configuration.partial")],
    entries: [{
      observation_key: null, parent_kind: "table", parent_schema: "dbo", parent_name: "Original", parent_object: { database_id: 7, object_id: 100, created_without_offset: "2019-01-01T00:00:00.0000000" },
      index_id: 2, index_name: "ix_original", type_code: 2, parent_modified_without_offset: "2019-01-02T00:00:00.0000000",
      fields: [{ key: "fill", label: t("view.index_configuration.field.fill_factor"), text: "80", availability: "available" }],
      partition_identity_availability: "available", partitions_complete: true, retained_partitions: 2, presented_partitions: 2, partitions_limited: false,
      partitions: [1, 2].map((number) => ({ partition_id: number === 1 ? "9007199254740993" : "9007199254740994", partition_number: number, hobt_id: number === 1 ? "9007199254741993" : "9007199254741994", fields: [{ key: "data", label: t("view.index_configuration.field.data_compression"), text: number === 1 ? "ROW (1)" : "PAGE (2)", availability: "available" }] })),
      limitations: [t("view.index_configuration.limit.configuration")],
    }],
  };
}

export function selectedIndexConfiguration() {
  const view = indexConfigurationReport(); const entry = view.entries[0]!;
  const index = partial<IndexStat>({ schema: entry.parent_schema, table: entry.parent_name, name: entry.index_name!, mssql_configuration: {
    parent_kind: "table", parent_object: structuredClone(entry.parent_object), parent_schema: entry.parent_schema, parent_name: entry.parent_name, index_id: entry.index_id, name: entry.index_name, type_code: entry.type_code,
    parent_modified_without_offset: entry.parent_modified_without_offset,
  } });
  const report = healthReportFixture({ index_configuration: [view], snapshot: { id: "native-owner", server: { engine: "mssql" }, sources: [{ source: "indexes", state: "partial" }] } });
  return { report, index };
}
