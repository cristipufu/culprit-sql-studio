import type { SuspectPageHistoryReport } from "../gen/SuspectPageHistoryReport";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationSourceIssue } from "./ObservationList";

/** The canonical interpreter owns history/active populations; this disclosure supplies no integrity verdict. */
export function SuspectPageHistory({ report }: { report: SuspectPageHistoryReport }) {
  const bounds = report.context?.kind === "native" ? report.context.context : report.context;
  return <section className="operational-health observation-list" aria-label={report.title}>
    <h3>{report.title}</h3>
    <p>{report.summary}</p>
    {report.source_state && report.source_state !== "collected" && <ObservationSourceIssue
      title={report.title}
      source={{ source: report.title, state: report.source_state, reason: report.source_reason }}
    />}
    <details>
      <summary>{t("view.suspect_pages.details")}</summary>
      {bounds && <p>{t("ui.memory_failures.read_between", { from: bounds.observed_from, until: bounds.observed_until })}</p>}
      <dl>
        <NativeValue label={t("view.suspect_pages.database")} value={report.database} />
        <NativeValue label={t("view.database_properties.native_database_id")} value={report.native_database_id} />
        <NativeValue label={t("view.database_properties.native_clock")} value={report.observed_at} />
        {report.metrics.map((metric) => <NativeValue
          key={metric.key ?? metric.label}
          label={metric.label}
          value={metric.text}
        />)}
        <NativeValue
          label={t("view.suspect_pages.last_active_time")}
          value={report.last_reported_time_without_offset}
        />
      </dl>
      {report.limitations.map((limitation) => <p className="muted" key={limitation}>{limitation}</p>)}
    </details>
  </section>;
}
