// Controlled presentation data; native acquisition and original-record proofs are tested in Rust.
import type { QueryWaitFragmentReport } from "../gen/QueryWaitFragmentReport";
import { t } from "../lib/i18n";

export function queryWaitFragmentReport(count = 12): QueryWaitFragmentReport {
  return {
    source: "original-query-history", source_state: "partial", source_reason: null,
    title: t("view.query_wait_fragments.title"),
    summary: t("view.query_wait_fragments.summary", { observed: String(count), selected: String(count + 1) }),
    limitations: [t("view.query_wait_fragments.limit.fragments"), t("view.query_wait_fragments.limit.scope"), t("view.query_wait_fragments.limit.partial")],
    selected_rows: count + 1, observed_rows: count, read_complete: false, capture: "unavailable", interval_coverage: "unverified", references_complete: false, presentation_limited: false,
    read: null, description: null, collected_from: "2020-01-01T00:00:00Z", collected_until: "2020-01-01T00:00:01Z",
    entries: Array.from({ length: count }, (_, index) => ({
      reference: { kind: "informational", position: { stream: "query-waits", row_ordinal: index, payload_sha256: "a".repeat(64) } },
      native: { native: { id: `original-fragment-${index}`, qualifiers: { original_outcome: String(index % 2), original_replica: `member-${index}` } }, references: { query: null, plan: null }, cohort: null },
      category: "3", category_description: `Native category ${index}`, outcome_description: index % 2 === 0 ? "Regular" : "Aborted", unit: { kind: "milliseconds" },
      metrics: [{ key: "view.query_wait_fragments.metric.last", label: t("view.query_wait_fragments.metric.last"), value: 0, unit: "text", text: "9,007,199,254,740,993 ms" }],
    })),
  };
}
