import type { QueryWaitFragmentReport } from "../gen/QueryWaitFragmentReport";
import { contextPreview } from "../lib/context-preview";
import { t } from "../lib/i18n";
import { NativeReference, NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

export function QueryWaitFragments({ report }: { report: QueryWaitFragmentReport }) {
  const history = report.description?.observation;
  const times: Array<{ key: string; text: string }> = [];
  if (history?.kind === "query-history") times.push({ key: "window", text: t("view.query_wait_fragments.window", { from: history.data.interval.from, until: history.data.interval.until }) });
  if (report.collected_from && report.collected_until) times.push({ key: "collection", text: t("ui.memory_failures.read_between", { from: report.collected_from, until: report.collected_until }) });
  return <ObservationList collapseEntries source={report.source} scope={t("view.query_wait_fragments.limit.scope")}
    report={{ ...report, sources: [{ source: report.source, state: report.source_state, reason: report.source_reason }] }}
    time={times.length > 0 ? times.map((line) => <span key={line.key} style={{ display: "block" }}>{line.text}</span>) : undefined}
    renderEntry={(entry) => <article className="operational-card" style={{ overflowWrap: "anywhere" }} key={JSON.stringify(entry.reference)}>
      <h4>{contextPreview(entry.category_description ?? entry.category, 256).text}</h4>
      <p>{contextPreview(entry.outcome_description ?? t("view.query_wait_fragments.unknown"), 256).text}</p>
      <dl>{entry.metrics.map((metric) => <NativeValue key={metric.key} label={metric.label} value={metric.text} />)}</dl>
      <details><summary>{t("ui.page_server_reads.identities")}</summary>
        <dl>
          <NativeValue label={t("ui.page_server_reads.query")} value={entry.native.references.query?.native.id} />
          <NativeValue label={t("ui.page_server_reads.plan")} value={entry.native.references.plan?.native.id} />
        </dl>
        <NativeReference identity={entry.native.native} />
      </details>
    </article>} />;
}
