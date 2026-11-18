import type { PerformanceCounterReport } from "../gen/PerformanceCounterReport";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { CounterRatios } from "./CounterRatios";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Native semantics and exact formatting belong to the shared Rust view. */
export function PerformanceCounters({ report }: { report: PerformanceCounterReport }) {
  const bounds = report.context?.kind === "native" ? report.context.context : report.context;
  const count = (value: number | null) => value == null ? t("view.performance_counters.unknown") : digits(String(value));
  return <>
    <ObservationList collapseEntries source={t("view.performance_counters.source")}
      scope={t("view.performance_counters.scope")} report={report}
      time={<>{t("view.performance_counters.counts", { native_sets: count(report.native_observations), sets: count(report.observed_observations), native: count(report.native_counter_rows), observed: count(report.observed_counter_rows), presented: count(report.presented_counter_rows) })}
        {bounds && <span> · {t("ui.memory_failures.read_between", { from: bounds.observed_from, until: bounds.observed_until })}</span>}</>}
      renderEntry={(entry) => <article className="operational-card" key={`${entry.observation_key}:${entry.counter_key}`}>
        <h4>{entry.metric.label}</h4><p>{entry.explanation}</p>
        <dl><NativeValue label={entry.family_label} value={entry.metric.text} /></dl>
        <details><summary>{t("ui.memory_failures.limitations")}</summary><dl>
          <NativeValue label={t("view.counter_ratios.metric.object")} value={entry.native_object} />
          <NativeValue label={t("view.counter_ratios.metric.instance")} value={entry.native_instance ?? t("view.counter_ratios.uninstanced")} />
          <NativeValue label={t("view.performance_counters.native.type")} value={entry.native_type} />
          <NativeValue label={t("view.performance_counters.native.raw")} value={entry.raw_value} />
          <NativeValue label={t("view.counter_ratios.metric.clock")} value={entry.observed_at} />
        </dl></details>
      </article>} />
    {report.missing_fields.length > 0 && <details className="operational-health"><summary>{t("view.performance_counters.missing", { count: digits(String(report.missing_fields.length)) })}</summary><dl>
      {report.missing_fields.map((metric, index) => <NativeValue key={`${metric.key}:${index}`} label={metric.label} value={metric.text} />)}
    </dl></details>}
    {report.ratios && <CounterRatios report={report.ratios} />}
  </>;
}
