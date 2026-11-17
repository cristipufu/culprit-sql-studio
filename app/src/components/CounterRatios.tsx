import type { CounterRatioReport } from "../gen/CounterRatioReport";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** The shared Rust view owns pairing, integer arithmetic and qualifications. */
export function CounterRatios({ report }: { report: CounterRatioReport }) {
  return <ObservationList collapseEntries source={t("ui.counter_ratios.source")} report={report}
    scope={t("view.counter_ratios.scope")}
    renderEntry={(entry) => <article className="operational-card" key={`${entry.observation_key}:${entry.native_object}:${entry.native_instance}`}>
      <h4>{entry.title}</h4><p>{entry.explanation}</p>
      <dl>
        {entry.metrics.map((metric) => <NativeValue key={metric.key} label={metric.label} value={metric.text} />)}
      </dl>
      <details><summary>{t("ui.memory_failures.limitations")}</summary><dl>
        <NativeValue label={t("view.counter_ratios.metric.clock")} value={entry.observed_at} />
        <NativeValue label={t("view.counter_ratios.metric.object")} value={entry.native_object} />
        <NativeValue label={t("view.counter_ratios.metric.instance")} value={entry.native_instance ?? (entry.native_object ? t("view.counter_ratios.uninstanced") : null)} />
      </dl></details>
    </article>} />;
}
