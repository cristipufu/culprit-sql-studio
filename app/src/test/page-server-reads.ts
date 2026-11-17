import type { PageServerReadEntry } from "../gen/PageServerReadEntry";
import type { PageServerReadReport } from "../gen/PageServerReadReport";
import type { Report } from "../gen/Report";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";

export const EXACT_PAGE_SERVER_PAGES = "9007199254740993";

/** Presentation DTOs model Rust output; the browser never decodes or interprets native runtime rows. */
export function pageServerReadReport(count = 12): PageServerReadReport {
  const entries: PageServerReadEntry[] = Array.from({ length: count }, (_, index) => ({
    identity: { kind: "recorded-plan", query_label: `display-query-${index}`, plan_label: `display-plan-${index}`,
      native_plan: { plan_id: String(index + 4), query: { query_id: "42", qualifiers: {} }, qualifiers: {} },
      native_fragment: { id: String(index + 17), qualifiers: {} }, native_fragments: "1", execution_type: "0" },
    executions: EXACT_PAGE_SERVER_PAGES,
    explanation: { availability: "available", title: `${t("view.page_server_reads.title")} ${index + 1}`,
      summary: t("view.page_server_reads.observed"),
      metrics: [
        { label: t("view.page_server_reads.metric.last"), value: 0, unit: "text", key: "view.page_server_reads.metric.last", text: EXACT_PAGE_SERVER_PAGES },
        { label: t("view.page_server_reads.metric.minimum"), value: 0, unit: "text", key: "view.page_server_reads.metric.minimum", text: "0" },
        { label: t("view.page_server_reads.metric.maximum"), value: 0, unit: "text", key: "view.page_server_reads.metric.maximum", text: t("view.page_server_reads.unknown") },
      ],
      limitations: [t("view.page_server_reads.limit.origin"), t("view.page_server_reads.limit.population")],
      investigate: t("view.page_server_reads.investigate"), verify: t("view.page_server_reads.verify") },
  }));
  return { source: "Query Store", availability: "available", read_complete: false, selected_rows: count + 3,
    observed_rows: count, presentation_limited: false, source_state: "partial", source_reason: null,
    title: t("view.page_server_reads.title"), summary: t("view.page_server_reads.summary.archive", { observed: String(count), selected: String(count + 3) }),
    limitations: [t("view.page_server_reads.limit.lifetime"), t("view.page_server_reads.limit.read"), t("view.page_server_reads.limit.storage")], entries };
}

export function unavailablePageServerReads(): PageServerReadReport {
  const report = pageServerReadReport(0);
  report.availability = "unavailable";
  report.selected_rows = 0;
  report.observed_rows = 0;
  report.read_complete = true;
  report.presentation_limited = false;
  report.source_state = "empty";
  report.summary = t("view.page_server_reads.summary.archive", { observed: "0", selected: "0" });
  report.limitations = [t("view.page_server_reads.unavailable"), t("view.page_server_reads.limit.lifetime")];
  return report;
}

export function pageServerReadHealthReport(view: PageServerReadReport | null = pageServerReadReport()): Report {
  return partial<Report>({ snapshot: { id: "page-server-presentation", server: { engine: "postgres" }, pressure: null,
    sources: view ? [] : [{ source: "query_store", state: "unavailable", reason: t("view.page_server_reads.invalid"),
      reason_message: { kind: "catalog", key: "view.page_server_reads.invalid", arguments: {} } }],
    resources: [], operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    page_server_reads: view ?? undefined });
}
