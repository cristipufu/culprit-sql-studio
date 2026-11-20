import type { SuspectPageHistoryReport } from "../gen/SuspectPageHistoryReport";
import type { Report } from "../gen/Report";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";

export function suspectPageHistoryReport(unavailable = false): SuspectPageHistoryReport {
  return partial<SuspectPageHistoryReport>({
    context: {
      kind: "collector", observed_from: "2020-01-01T00:00:00Z", observed_until: "2020-01-01T00:00:01Z",
    },
    native_source: null,
    source_state: unavailable ? "unavailable" : "collected",
    source_reason: unavailable ? t("view.suspect_pages.permission") : null,
    read_complete: true,
    database: unavailable ? null : "shop",
    native_database_id: unavailable ? null : "7",
    observed_at: unavailable ? null : "2020-01-01T00:00:00.5Z",
    active_rows: unavailable ? null : "0",
    retained_rows: unavailable ? null : "3",
    last_reported_time_without_offset: null,
    title: t("view.suspect_pages.title"),
    summary: unavailable
      ? t("view.suspect_pages.unavailable")
      : t("view.suspect_pages.recorded", { retained: "3", active: "0" }),
    metrics: unavailable ? [] : [
      { key: "retained", label: t("view.suspect_pages.retained"), text: "3" },
      { key: "active", label: t("view.suspect_pages.active"), text: "0" },
    ],
    limitations: [
      t("view.suspect_pages.limit.scope"), t("view.suspect_pages.limit.incarnation"),
      t("view.suspect_pages.limit.editable"), t("view.suspect_pages.limit.capacity"),
      t("view.suspect_pages.limit.zero"), t("view.suspect_pages.limit.time"),
    ],
  });
}
export function suspectPageHealthReport(unavailable = false): Report {
  return partial<Report>({
    snapshot: { id: "suspect-history", server: { engine: "mssql" }, resources: [], pressure: null, sources: [], operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    performance_counters: [],
    suspect_pages: suspectPageHistoryReport(unavailable),
  });
}
