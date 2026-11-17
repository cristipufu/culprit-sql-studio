import type { ConnectionOutcomeReport } from "../gen/ConnectionOutcomeReport";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Rust supplies meanings, values and scope. Rendering never derives rates or assigns a caller. */
export function ConnectionOutcomes({ report }: { report: ConnectionOutcomeReport }) {
  const context = report.context;
  const bounds = context.kind === "native" ? context.context : context;
  return <ObservationList collapseEntries source={report.sources.map((source) => source.source).join(", ")}
    report={report} scope={t("ui.connection_outcomes.scope")}
    time={<>{t("ui.memory_failures.read_between", { from: bounds.observed_from, until: bounds.observed_until })}<br />
      {t("ui.connection_outcomes.counts", { native: digits(String(report.native_rows)), read: digits(String(report.observed_rows)), mapped: digits(String(report.mapped_rows)) })}</>}
    renderEntry={(entry) => <article className="operational-card" key={`${entry.reference.source}:${entry.reference.observation_key}`}>
      <h4>{entry.title}</h4><p>{entry.explanation}</p>
      <dl>{entry.metrics.map((metric) => <NativeValue key={metric.kind} label={metric.label} value={metric.display_value} />)}</dl>
      <details>
        <summary>{t("ui.connection_outcomes.details")}</summary>
        {entry.metrics.map((metric) => <section key={metric.kind}>
          <h5>{metric.label}</h5><code>{metric.native_key}</code><p>{metric.explanation}</p>
          <dl>
            <NativeValue label={t("ui.connection_outcomes.raw_value")} value={metric.value?.value} />
            <NativeValue label={t("ui.statement_work.reset")} value={metric.reset.kind === "unknown"
              ? t("ui.statement_work.reset_unknown") : <code>{metric.reset.value}</code>} />
          </dl>
        </section>)}
      </details>
      <details><summary>{t("ui.memory_failures.investigation")}</summary><p>{entry.investigation}</p><p>{entry.verify}</p></details>
    </article>} />;
}
