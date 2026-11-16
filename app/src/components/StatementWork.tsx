import type { MeasurementScope } from "../gen/MeasurementScope";
import type { ResetMarker } from "../gen/ResetMarker";
import type { StatementCounterSnapshot } from "../gen/StatementCounterSnapshot";
import type { StatementWorkEntry } from "../gen/StatementWorkEntry";
import type { StatementWorkReport } from "../gen/StatementWorkReport";
import { findingMetric } from "../lib/format";
import { key, t } from "../lib/i18n";
import { nativeIdentityKey } from "../lib/native-identity";
import { observationRows } from "../lib/observation-rows";
import { NativeReference, NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

const OUTCOMES = [key("view.statement_work.outcome.errors"), key("view.statement_work.outcome.warnings")];

/** Counts and explanations come from Rust. Only the exact native point can supply its displayed context. */
export function StatementWork({ evidence, report }: { evidence: StatementCounterSnapshot; report: StatementWorkReport }) {
  const rows = observationRows(evidence.observations, evidence.source, (row) => row.key);
  return <ObservationList collapseEntries source={evidence.source} report={report} scope={t("ui.statement_work.scope")}
    entriesLabel={t("ui.statement_work.inspect")}
    time={evidence.context && t("ui.memory_failures.read_between", { from: evidence.context.observed_from, until: evidence.context.observed_until })}
    renderEntry={(entry) => {
      const row = rows.get(entry.observation_key);
      const native = row?.fact.kind === "statement-counters" ? row.fact.data : null;
      const matches = evidence.source === report.source && entry.source === evidence.source && native
        && queryKey(native.identity) === queryKey(entry.identity)
        && native.scope === entry.scope && native.observed_at === entry.observed_at && sameReset(native.reset, entry.reset);
      return matches ? <StatementPoint key={entry.observation_key} entry={entry} />
        : <div className="operational-card" key={entry.observation_key}><p>{t("ui.memory_failures.missing_row")}</p></div>;
    }} />;
}

function queryKey(identity: StatementWorkEntry["identity"]): string {
  return nativeIdentityKey({ id: identity.query_id, qualifiers: identity.qualifiers });
}

function sameReset(left: ResetMarker, right: ResetMarker): boolean {
  return left.kind === right.kind && (left.kind === "unknown" || (right.kind !== "unknown" && left.value === right.value));
}

function scopeLabel(scope: MeasurementScope): string {
  switch (scope) {
    case "database": return t("ui.replication.scope.database");
    case "node": return t("ui.replication.scope.node");
    default: return t("ui.statement_work.scope_unknown");
  }
}

function StatementPoint({ entry }: { entry: StatementWorkEntry }) {
  return <article className="operational-card">
    <NativeReference identity={{ id: entry.identity.query_id, qualifiers: entry.identity.qualifiers }} />
    <dl>
      <NativeValue label={t("ui.statement_work.point_scope")} value={scopeLabel(entry.scope)} />
      <NativeValue label={t("ui.memory_participants.observed_at_label")} value={entry.observed_at} />
      <NativeValue label={t("ui.statement_work.reset")} value={entry.reset.kind === "unknown"
        ? t("ui.statement_work.reset_unknown") : <code>{entry.reset.value}</code>} />
      {OUTCOMES.map((key) => <NativeValue key={key} label={t(key)} value={entry.outcome_metrics.find((metric) => metric.key === key)?.text} />)}
    </dl>
    {entry.work.map((work, index) => <details key={index}>
      <summary>{work.title}</summary><p>{work.summary}</p>
      <dl>{work.metrics.map((metric, index) => <NativeValue key={metric.key ?? index} label={metric.label} value={findingMetric(metric)} />)}</dl>
      {work.limitations.map((limit, index) => <p className="muted" key={index}>{limit}</p>)}
      <p>{work.investigation}</p><p>{work.verify}</p>
    </details>)}
  </article>;
}
