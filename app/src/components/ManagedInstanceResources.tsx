import { useState } from "react";
import type { ManagedInstanceResourceReport } from "../gen/ManagedInstanceResourceReport";
import { contextPreview } from "../lib/context-preview";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList, ObservationReadQualifications } from "./ObservationList";

const text = (value: string) => contextPreview(value).text;
const FIELD_LIMIT = 11;

/** The canonical report owns the original intervals; this disclosure never borrows the current scan's point. */
export function ManagedInstanceResources({ report }: { report: ManagedInstanceResourceReport }) {
  const [open, setOpen] = useState(false);
  const source = t("provider.mssql.source.managed_instance_resources");
  return <section className="operational-health observation-list" aria-label={text(report.title)}>
    <h3>{text(report.title)}</h3><p>{text(report.summary)}</p>
    <ObservationReadQualifications sources={[{ source, state: report.source_state }]} />
    <details onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>{t("view.mi_resources.details")}</summary>
      {open && <>
        {report.from && report.until && <p className="muted">{t("view.mi_resources.window", { from: text(report.from), until: text(report.until) })}</p>}
        {report.observed_at && <p className="muted">{t("view.mi_resources.observed", { at: text(report.observed_at) })}</p>}
        <ManagedInstanceResourceIntervals report={report} />
        {!report.read_complete && <p>{t("ui.common.read_partial")}</p>}
        <p>{text(report.investigation)}</p><p>{text(report.verify)}</p>
      </>}
    </details>
  </section>;
}

/** One bounded page of the canonical interval fields, also used to verify text-only presentation. */
export function ManagedInstanceResourceIntervals({ report }: { report: ManagedInstanceResourceReport }) {
  const source = t("provider.mssql.source.managed_instance_resources");
  const admitted = report.source_state === "collected" || report.source_state === "partial";
  return <ObservationList source={source} scope={t("view.mi_resources.scope")}
  report={{ title: text(report.title), summary: text(report.summary), source_state: report.source_state, source_reason: null, limitations: report.limitations.map(text), entries: admitted ? report.intervals : [] }}
          renderEntry={(interval) => <article className="operational-card" key={interval.ended_at}>
            <h4>{t("view.mi_resources.interval", { from: text(interval.started_at), until: text(interval.ended_at) })}</h4>
            <dl>{interval.fields.slice(0, FIELD_LIMIT).map((field) => <NativeValue key={field.key} label={text(field.label)} value={field.value == null ? (field.missing_reason ? text(field.missing_reason) : null) : text(field.value)} />)}</dl>
          </article>} />;
}
