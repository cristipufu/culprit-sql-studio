import type { WaitCounterReport } from "../gen/WaitCounterReport";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Native arithmetic, identity and read qualification remain in the shared Rust explanation. */
export function WaitCounters({ report }: { report: WaitCounterReport }) {
  const clock = report.context?.kind === "native" ? report.context.context : report.context;
  return <ObservationList collapseEntries source={t("view.wait_counters.source")} report={report}
    scope={t("view.wait_counters.scope")}
    time={clock ? t("view.wait_counters.bounds", { from: clock.observed_from, until: clock.observed_until }) : undefined}
    renderEntry={(entry) => <article className="operational-card" key={`${entry.observation.source}:${entry.observation.wait_type}`}>
      <h4>{entry.title}</h4><p>{entry.explanation}</p>
      <dl>{entry.metrics.map((metric) => <NativeValue key={metric.key} label={metric.label} value={metric.text} />)}</dl>
    </article>} />;
}
