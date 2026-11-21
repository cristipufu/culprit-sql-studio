import type { ProviderAcquisitionReport } from "../gen/ProviderAcquisitionReport";
import type { ReadWorkSummary } from "../gen/ReadWorkSummary";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";
export const CURRENT_TEMPLATE_ID = "b".repeat(64);
export function currentAcquisition(readers = 1): ProviderAcquisitionReport {
  const work: ReadWorkSummary = {
    connection_work: null,
    native_session_work: null,
    active_tasks: 0,
    untracked_tasks: 0,
    templates: [
      {
        template_id: CURRENT_TEMPLATE_ID,
        statement: { state: "unavailable", reason: "restricted" },
        completed_tasks: 1,
        failures: 0,
        timeouts: 0,
        cancellations: 0,
        unconfirmed_cancellations: 0,
        stop_causes: [],
        rows_decoded: "9007199254740993",
        charged_value_bytes: "18446744073709551615",
        client_elapsed_ms: 1.25,
      },
    ],
  };
  return partial<ProviderAcquisitionReport>({
    kind: "available",
    title: t("view.provider_acquisition.title"),
    summary: "current-client-acquisition",
    settlement: t("view.provider_acquisition.native.quiescent"),
    coverage: t("view.provider_acquisition.coverage.partial"),
    total_admissions: null,
    evidence: {
      observation_id: "current-operation-only",
      client_started_at: "2026-11-21T00:00:00Z",
      client_finished_at: "2026-11-21T00:00:01Z",
      readers: Array.from({ length: readers }, (_, index) => ({
        reader_id: index + 1,
        queries: 1,
        setup_accounting: "included-in-reader-work",
        setup_queries: null,
        work,
        setup_work: null,
      })),
    },
    readers: Array.from({ length: readers }, (_, index) => ({
      reader_id: index + 1,
      work: {
        summary: "original-reader-work",
        template_order: [0],
        connection_work: null,
        native_interval: null,
        limitations: [],
      },
      setup_work: null,
    })),
    limitations: [t("view.provider_acquisition.limit.client")],
  });
}
