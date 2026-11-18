import { useState } from "react";
import type { DatabasePropertiesReport } from "../gen/DatabasePropertiesReport";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationSourceIssue } from "./ObservationList";

/** The shared Rust view owns every native meaning; this disclosure only presents its original fields. */
export function DatabaseProperties({ report }: { report: DatabasePropertiesReport }) {
  const [expanded, setExpanded] = useState(false);
  const bounds = report.context?.kind === "native" ? report.context.context : report.context;
  return <section className="operational-health observation-list database-properties" aria-label={report.title}>
    <h3>{report.title}</h3><p>{report.summary}</p>
    {report.source_state && report.source_state !== "collected" && <ObservationSourceIssue title={t("view.database_properties.source")} source={{ source: t("view.database_properties.source"), state: report.source_state, reason: null }} />}
    <details onToggle={(event) => setExpanded(event.currentTarget.open)}>
      <summary>{t("view.database_properties.details")}</summary>
      {expanded && <>
        {bounds && <p className="muted">{t("ui.memory_failures.read_between", { from: bounds.observed_from, until: bounds.observed_until })}</p>}
        <dl><NativeValue label={t("view.database_properties.native_database_id")} value={report.native_database_id} />
          <NativeValue label={t("view.database_properties.native_clock")} value={report.observed_at} /></dl>
        <DatabasePropertyFields report={report} />
        <details><summary>{t("ui.memory_failures.limitations")}</summary>
          {report.limitations.map((limitation) => <p className="muted" key={limitation}>{limitation}</p>)}
        </details>
      </>}
    </details>
  </section>;
}

/** Shared report fields are already interpreted and formatted; native text remains text. */
export function DatabasePropertyFields({ report }: { report: DatabasePropertiesReport }) {
  return <div className="operational-grid observation-grid">{report.fields.map((field) => <article className="operational-card" key={field.key}>
    <dl><NativeValue label={field.label} value={field.value} />
      {field.native_code != null && <NativeValue label={t("view.database_properties.native_code")} value={field.native_code} />}
      {field.native_description != null && <NativeValue label={t("view.database_properties.native_description")} value={field.native_description} />}
    </dl>
  </article>)}</div>;
}
