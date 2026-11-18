import type { ConfiguredLogRateReport } from "../gen/ConfiguredLogRateReport";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Native scopes and exact display values come from Rust; configuration is never rendered as utilization. */
export function ConfiguredLogRates({ report }: { report: ConfiguredLogRateReport }) {
  return <ObservationList collapseEntries source={t("provider.mssql.source.configured_log_rates")}
    report={report} scope={t("view.configured_log_rates.limit.scopes")}
    time={<>{t("ui.memory_failures.read_between", { from: report.context.observed_from, until: report.context.observed_until })}
      {report.observed_at && <><br />{t("view.counter_ratios.cli.clock", { at: report.observed_at })}</>}
    </>}
    renderEntry={(entry) => <article className="operational-card" key={entry.scope}>
      <h4>{entry.title}</h4><p>{entry.explanation}</p>
      <dl><NativeValue label={entry.metric.label} value={entry.metric.text} /></dl>
    </article>} />;
}
