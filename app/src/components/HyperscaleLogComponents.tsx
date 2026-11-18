import type { HyperscaleLogComponentReport } from "../gen/HyperscaleLogComponentReport";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Rust retains independent native component reports; rendering never computes a catch-up ETA. */
export function HyperscaleLogComponents({ report }: { report: HyperscaleLogComponentReport }) {
  return <ObservationList collapseEntries source={t("view.hyperscale_log_components.source")} report={report}
    scope={t("view.hyperscale_log_components.scope")}
    time={t("ui.memory_failures.read_between", { from: report.context.observed_from, until: report.context.observed_until })}
    renderEntry={(entry) => <article className="operational-card" key={entry.observation_key}>
      <h4>{entry.title}</h4><p>{entry.explanation}</p>
      <dl>
        <NativeValue label={t("view.hyperscale_log_components.field.component")} value={entry.component_id} />
        <NativeValue label={t("view.hyperscale_log_components.field.replica")} value={entry.replica_id} />
        <NativeValue label={t("view.hyperscale_log_components.field.file")} value={entry.file_id} />
        <NativeValue label={t("view.hyperscale_log_components.field.role")} value={String(entry.role)} />
        <NativeValue label={t("view.hyperscale_log_components.field.role_desc")} value={entry.role_desc} />
        <NativeValue label={t("view.hyperscale_log_components.field.clock")} value={entry.last_reported_time_without_offset} />
        {entry.metrics.map((metric) => <NativeValue key={metric.label} label={metric.label} value={metric.text} />)}
      </dl>
    </article>} />;
}
