import type { IndexedViewsReport } from "../gen/IndexedViewsReport";
import { t } from "../lib/i18n";
import { indexConfigurationReport } from "./index-configuration";

/** Presentation-only canonical report shape; native admission is tested through the original Rust corpus. */
export function indexedViewsReport(): IndexedViewsReport {
  const configuration = structuredClone(indexConfigurationReport().entries[0]!);
  configuration.parent_kind = "indexed-view";
  configuration.parent_schema = "reporting";
  configuration.parent_name = "Revenue";
  return {
    context: null, source: { source: "indexes", state: "partial", reason: null }, read_complete: null,
    native_index_rows: 3, observed_index_rows: 3, observed_view_indexes: 1, entries_limited: false,
    title: t("view.indexed_views.title"), summary: t("view.indexed_views.summary", { count: 1 }),
    qualification: t("view.indexed_views.qualification"),
    limitations: [t("view.indexed_views.limit.definition"), t("view.indexed_views.limit.usage"), t("view.indexed_views.limit.visibility"), t("view.indexed_views.partial")],
    entries: [{ observation_key: null, parent_object: configuration.parent_object,
      parent_schema: "reporting", parent_name: "Revenue", parent_modified_without_offset: configuration.parent_modified_without_offset,
      index_id: 2, index_name: "ix_revenue", type_code: 2, native_type: "NONCLUSTERED", unique: false, disabled: false,
      reserved_bytes: "9007199254740993", native_columns: 1, columns_limited: false,
      columns: [{ index_column_id: 1, column_id: 2, name: "customer_id", key_ordinal: 1, descending: false, included: false }],
      usage: { kind: "observed", data: { row_present: true, user_seeks: "9007199254740993", user_scans: "0", user_lookups: "0", user_updates: "3",
        last_user_seek_without_offset: "2026-11-10T07:00:10.123", last_user_scan_without_offset: null, last_user_lookup_without_offset: null, last_user_update_without_offset: null, reset: { kind: "unknown" } } },
      configuration,
    }],
  };
}
