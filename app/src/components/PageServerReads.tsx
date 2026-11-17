import type { PageServerReadEntry } from "../gen/PageServerReadEntry";
import type { PageServerReadReport } from "../gen/PageServerReadReport";
import type { SourceOutcome } from "../gen/SourceOutcome";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeReference, NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";
import { QueryStoreStatement } from "./QueryStoreStatement";

/** Rust owns counter meaning and formatting. Native identities are historical text, never live lookup links. */
export function PageServerReads({ report }: { report: PageServerReadReport }) {
  return <ObservationList collapseEntries source={report.source} report={report} scope={t("ui.page_server_reads.scope")}
    time={<>{availability(report)}
      {report.definition_read && !report.definition_read.read_complete && <><br />{t("view.page_server_reads.statement.limit.partial")}</>}
      {report.presentation_limited && <><br />{t("view.page_server_reads.limit.presentation", { displayed: digits(String(report.entries.length)) })}</>}
    </>}
    renderEntry={(entry) => <article className="operational-card" key={JSON.stringify(entry.identity)} style={{ overflowWrap: "anywhere" }}>
      <h4>{entry.explanation.title}</h4><p>{entry.explanation.summary}</p>
      <dl>
        <NativeValue label={t("ui.page_server_reads.executions")} value={digits(entry.executions)} />
        {entry.explanation.metrics.map((metric, index) => <NativeValue key={metric.key ?? index} label={metric.label} value={metric.text} />)}
      </dl>
      {entry.statement && <QueryStoreStatement statement={entry.statement} />}
      <details><summary>{t("ui.page_server_reads.identities")}</summary>
        {!report.definition_read && <p className="muted">{t("view.page_server_reads.limit.definition_link")}</p>}
        <HistoricalIdentity entry={entry} />
      </details>
      <details><summary>{t("ui.memory_failures.limitations")}</summary>
        {entry.explanation.limitations.map((limitation, index) => <p className="muted" key={index}>{limitation}</p>)}
      </details>
      <details><summary>{t("ui.memory_failures.investigation")}</summary>
        <p>{entry.explanation.investigate}</p><p>{entry.explanation.verify}</p>
      </details>
    </article>} />;
}

function HistoricalIdentity({ entry }: { entry: PageServerReadEntry }) {
  const identity = entry.identity;
  if (identity.kind === "runtime-fragment") {
    return <>
      <dl>
        <NativeValue label={t("ui.page_server_reads.query")} value={identity.native.references.query && <code>{identity.native.references.query.native.id}</code>} />
        <NativeValue label={t("ui.page_server_reads.plan")} value={identity.native.references.plan && <code>{identity.native.references.plan.native.id}</code>} />
        {identity.reference.kind === "informational" && <NativeValue label={t("ui.page_server_reads.position")} value={digits(String(identity.reference.position.row_ordinal))} />}
      </dl>
      <NativeReference identity={identity.native.native} />
    </>;
  }
  return <>
    <dl>
      <NativeValue label={t("ui.page_server_reads.query_label")} value={<code>{identity.query_label}</code>} />
      <NativeValue label={t("ui.page_server_reads.plan_label")} value={<code>{identity.plan_label}</code>} />
      <NativeValue label={t("ui.page_server_reads.query")} value={identity.native_plan && <code>{identity.native_plan.query.query_id}</code>} />
      <NativeValue label={t("ui.page_server_reads.plan")} value={identity.native_plan && <code>{identity.native_plan.plan_id}</code>} />
    </dl>
    {identity.native_fragment && <NativeReference identity={identity.native_fragment} />}
  </>;
}

function availability(report: PageServerReadReport): string {
  switch (report.availability) {
    case "available": return t("ui.page_server_reads.available");
    case "not-applicable": return t("view.page_server_reads.not_applicable");
    case "unavailable": return t("view.page_server_reads.unavailable");
    default: return t("view.page_server_reads.profile_unknown");
  }
}

/** Only the typed invalid-evidence marker belongs to this view; unrelated Query Store gaps do not. */
export function pageServerReadIssue(sources: readonly SourceOutcome[]): SourceOutcome | undefined {
  return sources.find((source) => source.source === "query_store" && source.reason_message?.kind === "catalog"
    && source.reason_message.key === "view.page_server_reads.invalid" && Object.keys(source.reason_message.arguments).length === 0);
}
