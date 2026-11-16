import type { CurrentRequestWorkReport } from "../gen/CurrentRequestWorkReport";
import { findingMetric } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";
import { Sql } from "./Sql";

/** The shared Rust report owns interpretation. No browser-side counter, scope or cause classification runs here. */
export function CurrentRequestWork({ report }: { report: CurrentRequestWorkReport }) {
  return <details className="qp-current-work">
    <summary>{report.title}</summary>
    <ObservationList source={report.source} report={report} scope={t("ui.query_progress.work_scope")}
      time={t("ui.memory_failures.read_between", { from: report.context.observed_from, until: report.context.observed_until })}
      renderEntry={(entry) => <article className="operational-card" key={entry.observation_key}>
        <h4>{entry.title}</h4><p>{entry.explanation}</p>
        <dl>{entry.metrics.map((metric, index) => <NativeValue key={metric.key ?? index} label={metric.label} value={findingMetric(metric)} />)}</dl>
        <details>
          <summary>{t("ui.memory_failures.investigation")}</summary>
          <p>{entry.investigation}</p><p>{entry.verify}</p>
          <dl>
            <NativeValue label={t("ui.memory_participants.observed_at_label")} value={entry.observed_at} />
            <NativeValue label={t("ui.memory_participants.session")} value={<code>{entry.actor.session.id}</code>} />
            <NativeValue label={t("ui.memory_participants.generation")} value={<code>{entry.actor.session.generation}</code>} />
            <NativeValue label={t("ui.memory_participants.request")} value={entry.actor.request?.id} />
            <NativeValue label={t("ui.memory_participants.request_generation")} value={entry.actor.request?.generation} />
          </dl>
          {entry.statement.state === "available" ? <div className="codeblock"><Sql text={entry.statement.text} pretty={false} /></div>
            : <p className="muted">{t("ui.statement_grants.unavailable", { reason: entry.statement.reason })}</p>}
        </details>
      </article>} />
  </details>;
}
