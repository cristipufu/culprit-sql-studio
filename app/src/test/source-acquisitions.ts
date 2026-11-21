import type { HistoricalSourceOverhead } from "../gen/HistoricalSourceOverhead";
import { t } from "../lib/i18n";
export const ACQUISITION_TEMPLATE_ID = "a".repeat(64);
export function historicalAcquisition(index = 0): HistoricalSourceOverhead {
  return { kind: "available", manifest_sha256: "private-manifest-digest", report: {
    title: t("view.source_overhead.title"), summary: `original-metadata-${index}`,
    evidence: { capture: { observation_id: `original-capture-${index}`, revision: null }, engine: null,
      status: { state: "incomplete", reason: "source-unavailable" }, started_at: "2020-01-01T00:00:00Z",
      finished_at: "2020-01-01T00:00:01Z", source_queries: 7,
      read_metrics: { connection_work: null, native_session_work: null, active_tasks: 0, untracked_tasks: 0,
        templates: [{ template_id: ACQUISITION_TEMPLATE_ID,statement: { state: "unavailable",reason: "restricted" },
          completed_tasks: 1, failures: 1, timeouts: 0, cancellations: 0, unconfirmed_cancellations: 0,
          stop_causes: [], rows_decoded: "9007199254740993", charged_value_bytes: "18446744073709551615",client_elapsed_ms: 1.25 }] } },
    template_order: [0], limitations: [t("view.source_overhead.limit.historical"),t("view.source_overhead.limit.incomplete")],
    native_interval: null, connection_work: null,
  } };
}
