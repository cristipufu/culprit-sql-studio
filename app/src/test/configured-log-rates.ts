import type { ConfiguredLogRateReport } from "../gen/ConfiguredLogRateReport";
import type { Report } from "../gen/Report";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";

/** Presentation data is already interpreted and formatted by the Rust report contract. */
export function configuredLogRateReport(): ConfiguredLogRateReport {
  return partial<ConfiguredLogRateReport>({
    context: { observed_from: "2020-01-01T12:00:00Z", observed_until: "2020-01-01T12:00:01Z" },
    source_state: "partial", source_reason: null, read_complete: true, native_rows: 1, observed_rows: 1,
    observed_at: "2020-01-01T12:00:00.000000000Z", workload_group_id: null,
    title: t("view.configured_log_rates.title"), summary: t("view.configured_log_rates.summary.observed"),
    limitations: [t("view.configured_log_rates.limit.configuration"), t("view.configured_log_rates.limit.components"), t("view.configured_log_rates.limit.identity")],
    entries: [
      { scope: "user-workload-group", bytes_per_second: "9007199254740993", title: t("view.configured_log_rates.scope.workload_group"), explanation: t("view.configured_log_rates.meaning.workload_group"), metric: { label: t("view.configured_log_rates.metric.limit"), value: 0, unit: "text", text: "9007199254740993 B/s", key: "view.configured_log_rates.metric.limit" } },
      { scope: "user-resource-pool", bytes_per_second: "0", title: t("view.configured_log_rates.scope.resource_pool"), explanation: t("view.configured_log_rates.meaning.resource_pool"), metric: { label: t("view.configured_log_rates.metric.limit"), value: 0, unit: "text", text: "0 B/s", key: "view.configured_log_rates.metric.limit" } },
      { scope: "database-engine-instance", bytes_per_second: null, title: t("view.configured_log_rates.scope.instance"), explanation: t("view.configured_log_rates.meaning.instance"), metric: { label: t("view.configured_log_rates.metric.limit"), value: 0, unit: "text", text: t("view.configured_log_rates.unknown"), key: "view.configured_log_rates.metric.limit" } },
    ],
  });
}

export function configuredLogRateHealthReport(view: ConfiguredLogRateReport | null = configuredLogRateReport()): Report {
  return partial<Report>({
    snapshot: { id: "configured-limit-presentation", server: { engine: "mssql" }, sources: view ? [] : [{ source: "configured-log-rates", state: "unavailable", reason: t("view.configured_log_rates.unavailable") }], resources: [], pressure: null,
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    configured_log_rates: view ?? undefined,
  });
}
