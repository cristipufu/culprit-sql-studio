import type { DiagnosticEvidenceRef } from "../gen/DiagnosticEvidenceRef";
import type { QueryStoreStatementExplanation } from "../gen/QueryStoreStatementExplanation";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";
import { pageServerReadReport } from "./page-server-reads";

/** Presentation fixtures carry Rust-derived wording. They do not validate or synthesize native source proofs. */
export function queryStoreStatement(): QueryStoreStatementExplanation {
  return { binding: "matched", definition: partial<DiagnosticEvidenceRef>({ record_id: "opaque-original-definition" }),
    origin: "application", monitoring_template_id: null, preview: "SELECT recorded_column FROM dbo.recorded_table",
    preview_truncated: false, unavailable: null, unavailable_explanation: null,
    explanation: `${t("view.page_server_reads.statement.matched")} ${t("view.page_server_reads.statement.origin.application")}` };
}

export function pageServerStatementReport() {
  const report = pageServerReadReport(1);
  report.definition_read = { namespace: "matched", selected_rows: 2, observed_rows: 1, read_complete: false };
  report.entries[0]!.statement = queryStoreStatement();
  return report;
}
