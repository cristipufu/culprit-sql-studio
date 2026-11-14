import type { ReactNode } from "react";
import type { CleanupExplanation } from "../gen/CleanupExplanation";
import type { CleanupHorizon } from "../gen/CleanupHorizon";
import type { CleanupReport } from "../gen/CleanupReport";
import type { CleanupSnapshot } from "../gen/CleanupSnapshot";
import type { NativeActivityIdentity } from "../gen/NativeActivityIdentity";
import type { RetentionObservation } from "../gen/RetentionObservation";
import type { RetentionSnapshot } from "../gen/RetentionSnapshot";
import { findingMetric } from "../lib/format";
import { key, t } from "../lib/i18n";
import { nativeIdentityKey } from "../lib/native-identity";
import { observationRowsBySource } from "../lib/observation-rows";
import { NativeReference, NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

export function CleanupEvidence({ reads, retention, report }: {
  reads: CleanupSnapshot[];
  retention: RetentionSnapshot | null;
  report: CleanupReport;
}) {
  const native = observationRowsBySource(reads.flatMap((read) => read.observations
    .filter((row) => row.source === read.source)
    .map((row) => ({ ...row, read }))), (row) => row.key);
  const holders = observationRowsBySource(retention?.holders ?? [], (row) => nativeIdentityKey(row.identity));
  const presentation = { ...report, limitations: [...report.limitations, ...report.boundaries.map((boundary) =>
    t(boundary.complete ? key("ui.cleanup.boundary_qualified") : key("ui.cleanup.boundary_unqualified"), { horizon: horizonName(boundary.horizon) }))] };
  return <ObservationList collapseEntries source={t("ui.cleanup.source")} scope={t("ui.cleanup.scope")}
    report={presentation} entriesLabel={t("ui.cleanup.inspect")}
    renderEntry={(entry) => {
      const reference = entry.reference;
      const holder = reference.identity ? holders.get(reference.source)?.get(nativeIdentityKey(reference.identity)) : null;
      const row = reference.identity ? null : native.get(reference.source)?.get(reference.observation_key);
      const fact = row?.fact;
      const matched = holder || (fact?.kind === "mssql-persistent-version-store" || fact?.kind === "mssql-cleanup-horizon");
      const entryKey = JSON.stringify([reference.source, reference.observation_key]);
      return matched ? <CleanupEntry key={entryKey} entry={entry}>
        {holder && <RetainerContext holder={holder} />}
        {row && <p className="muted">{t("ui.memory_failures.read_between", { from: row.read.observed_from, until: row.read.observed_until })}</p>}
        {fact?.kind === "mssql-persistent-version-store" && <dl><NativeValue label={t("ui.cleanup.native_columns")} value={<code>{fact.data.available_columns.join(", ")}</code>} /></dl>}
        {fact?.kind === "mssql-cleanup-horizon" && <>
          {fact.data.oldest_confirmed && <TransactionContext session={fact.data.oldest_confirmed.session} transaction={fact.data.oldest_confirmed.transaction} />}
          {fact.data.oldest_unattributed_versioning && <TransactionContext session={fact.data.oldest_unattributed_versioning.session} transaction={fact.data.oldest_unattributed_versioning.transaction} />}
        </>}
      </CleanupEntry> : <article className="operational-card" key={entryKey}><p>{t("ui.memory_failures.missing_row")}</p></article>;
    }} />;
}

function CleanupEntry({ entry, children }: { entry: CleanupExplanation; children: ReactNode }) {
  return <article className="operational-card">
    <h4>{entry.title}</h4><p>{entry.explanation}</p>
    <dl>{entry.metrics.map((metric, index) => <NativeValue key={metric.key ?? index} label={metric.label} value={findingMetric(metric)} />)}</dl>
    <details>
      <summary>{t("ui.memory_failures.investigation")}</summary>
      <p>{entry.investigation}</p><p>{entry.verify}</p>
      {entry.reference.identity && <NativeReference identity={entry.reference.identity} />}
      {children}
    </details>
  </article>;
}

function RetainerContext({ holder }: { holder: RetentionObservation }) {
  return <>
    <dl>
      <NativeValue label={t("ui.cleanup.observed_at")} value={holder.observed_at} />
      <NativeValue label={t("ui.cleanup.native_session")} value={activityReference(holder.session)} />
      {holder.unresolved_session_id != null && <NativeValue label={t("ui.cleanup.unresolved_session")} value={holder.unresolved_session_id} />}
      <NativeValue label={t("ui.cleanup.native_transaction")} value={holder.transaction?.native_id} />
      <NativeValue label={t("ui.cleanup.transaction_started")} value={holder.transaction?.started_at} />
      <NativeValue label={t("ui.cleanup.transaction_prepared")} value={holder.transaction?.prepared_at} />
    </dl>
    {holder.query && <dl>
      <NativeValue label={t("ui.cleanup.query_relationship")} value={t(holder.query.relationship === "executing-at-observation" ? key("ui.cleanup.query_current") : key("ui.cleanup.query_previous"))} />
      <NativeValue label={t("ui.cleanup.native_query")} value={holder.query.identity ? <NativeReference identity={{ id: holder.query.identity.query_id, qualifiers: holder.query.identity.qualifiers }} /> : null} />
    </dl>}
  </>;
}

function TransactionContext({ session, transaction }: { session: NativeActivityIdentity | null; transaction: NativeActivityIdentity | null }) {
  return <dl>
    <NativeValue label={t("ui.cleanup.native_session")} value={activityReference(session)} />
    <NativeValue label={t("ui.cleanup.native_transaction")} value={activityReference(transaction)} />
  </dl>;
}

function activityReference(identity: NativeActivityIdentity | null): ReactNode {
  return identity && <NativeReference identity={{ id: identity.id, qualifiers: { generation: identity.generation } }} />;
}

function horizonName(horizon: CleanupHorizon): string {
  switch (horizon) {
    case "data": return t("ui.cleanup.horizon_data");
    case "catalog": return t("ui.cleanup.horizon_catalog");
    case "shared-catalog": return t("ui.cleanup.horizon_shared_catalog");
  }
}
