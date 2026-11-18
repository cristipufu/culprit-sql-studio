import type { HyperscaleLogComponentReport } from "../gen/HyperscaleLogComponentReport";
import type { Report } from "../gen/Report";
import { partial } from "./fixtures";
import { t } from "../lib/i18n";
export function hyperscaleLogComponentsReport(count = 11): HyperscaleLogComponentReport {
  return partial<HyperscaleLogComponentReport>({
    title: t("view.hyperscale_log_components.title"), summary: t("view.hyperscale_log_components.summary.observed", { count: String(count) }),
    limitations: [t("view.hyperscale_log_components.limit.values"), t("view.hyperscale_log_components.limit.clock")],
    context: partial<HyperscaleLogComponentReport["context"]>({ observed_from: "2026-11-18T12:00:00.000000001Z", observed_until: "2026-11-18T12:00:00.000000009Z", database_writable: null }),
    source_state: "partial", source_reason: null, native_rows: count, observed_rows: count, presented_rows: count, read_complete: true,
    entries: Array.from({ length: count }, (_, i) => ({
      observation_key: `native-component-${i}`, component_id: `component-${i}`, replica_id: null, file_id: null,
      role: 1, role_desc: null, title: t("view.hyperscale_log_components.role.storage"), explanation: t("view.hyperscale_log_components.entry"),
      current_max_log_rate_bps: "9007199254740993", catchup_rate_bps: "0", catchup_distance_bytes: "9223372036854775807", last_reported_time_without_offset: "2026-11-18T11:59:59.003",
      metrics: [partial({ label: t("view.hyperscale_log_components.field.limit"), text: "9007199254740993" }), partial({ label: t("view.hyperscale_log_components.field.rate"), text: "0" })],
    })),
  });
}
export function hyperscaleLogHealthReport(): Report {
  return partial<Report>({
    hyperscale_log_components: hyperscaleLogComponentsReport(),
    snapshot: partial<Report["snapshot"]>({ id: "native-hs-components", server: partial({ engine: "mssql" }), operations: partial({ spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null }), sources: [] }),
  });
}
