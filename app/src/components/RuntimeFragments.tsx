import type { QueryRuntimeFragmentReport } from "../gen/QueryRuntimeFragmentReport";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeReference, NativeValue, queryHistoryReadTimes } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";
export function RuntimeFragments({ report }: { report: QueryRuntimeFragmentReport }) {
  return <ObservationList collapseEntries source={report.source} scope={t("view.runtime_fragments.limit.scope")}
    report={{ ...report, sources: [{ source: report.source, state: report.source_state, reason: report.source_reason }] }}
    time={queryHistoryReadTimes(report)}
    renderEntry={(entry) => <article className="operational-card" key={JSON.stringify(entry.reference)} style={{ overflowWrap: "anywhere" }}>
      <h4>{t("view.runtime_fragments.entry", { executions: digits(entry.executions) })}</h4>
      <dl><NativeValue label={t("view.runtime_fragments.first_end")} value={entry.first_execution_at} /><NativeValue label={t("view.runtime_fragments.last_end")} value={entry.last_execution_at} /></dl>
      {entry.metrics.map((metric, index) => <details key={index}><summary>{metric.title}</summary><dl>{metric.statistics.map((stat) => <NativeValue key={stat.key} label={stat.label} value={stat.text} />)}</dl></details>)}
      {entry.page_server_explanation && <details><summary>{entry.page_server_explanation.title}</summary><p>{entry.page_server_explanation.summary}</p><dl>{entry.page_server_explanation.metrics.map((metric) => <NativeValue key={metric.key} label={metric.label} value={metric.text} />)}</dl>{entry.page_server_explanation.limitations.map((limit) => <p className="muted" key={limit}>{limit}</p>)}</details>}
      <details><summary>{t("ui.page_server_reads.identities")}</summary><dl>
        <NativeValue label={t("ui.page_server_reads.query")} value={entry.native.references.query?.native.id} />
        <NativeValue label={t("ui.page_server_reads.plan")} value={entry.native.references.plan?.native.id} />
      </dl><NativeReference identity={entry.native.native} /></details>
    </article>} />;
}
