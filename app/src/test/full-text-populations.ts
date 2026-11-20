import type { FullTextPopulationReport } from "../gen/FullTextPopulationReport";
import { t } from "../lib/i18n";
import { tableMemoryReport } from "./table-memory";

/** UI presentation fixture only; it does not establish native acquisition or original receipt admission. */
export function fullTextPopulationReport(): FullTextPopulationReport {
  return {
    original: { metadata: { observed_at: "2020-01-01T00:00:00.5Z", schema: "dbo", table: "OriginalDocuments" }, context: tableMemoryReport().original!.context,
      target: { table: { database_id: 7, object_id: 100, created_without_offset: "2019-01-01T00:00:00.000" },
        table_modified_without_offset: "2019-01-01T00:00:00.000", catalog_id: 2, unique_index_id: 1 } },
    source: { source: "full-text populations", state: "partial", reason: t("view.full_text_populations.partial") },
    read_complete: true, native_rows: 1, observed_rows: 1,
    title: t("view.full_text_populations.title"), summary: t("view.full_text_populations.partial_summary", { count: "1" }),
    limitations: [t("view.full_text_populations.limit.point"), t("view.full_text_populations.limit.identity"), t("view.full_text_populations.limit.time")],
    investigation: t("view.full_text_populations.investigate"), verify: t("view.full_text_populations.verify"),
    entries: [{ observation_key: "original-position-1", position: 1, observed_at: "2020-01-01T00:00:00.5Z", status: 7,
      summary: t("view.full_text_populations.status", { state: "stopped" }), metrics: [
        { key: "view.full_text_populations.outstanding_batches", label: t("view.full_text_populations.outstanding_batches"), value: 0, unit: "text", text: "0" },
        { key: "view.full_text_populations.started_local", label: t("view.full_text_populations.started_local"), value: 0, unit: "text", text: "2019-12-31T18:00:00.007" },
      ] }],
  };
}
