import type { GroupReplicationReport } from "../gen/GroupReplicationReport";
import { digits } from "../lib/format";
import { contextPreview } from "../lib/context-preview";
import { t } from "../lib/i18n";
import { ObservationList, ObservationReadQualifications, ObservationReadReasons } from "./ObservationList";
import { ReplicationEntryEvidence } from "./ReplicationEvidence";
import { NativeValue } from "./NativeObservationFields";

/** Each historical observer/group capture retains its own native bounds; no current-database association. */
export function GroupReplication({ reports }: { reports: GroupReplicationReport[] }) {
  if (reports.length === 0) return null;
  return <ObservationList collapseEntries source={t("provider.mysql.source.group_replication")} scope={t("view.group_replication.scope")}
    report={{ title: t("view.group_replication.title"), summary: t("view.group_replication.saved_summary"), sources: [], limitations: [], entries: reports }}
    renderEntry={(report) => <article className="operational-card" key={reports.indexOf(report)}>
      <h4>{report.title}</h4><p>{report.summary}</p>
      <ObservationReadQualifications sources={report.sources} />
      {report.original && <p className="muted">{t("ui.memory_failures.read_between", { from: report.original.context.observed_from, until: report.original.context.observed_until })}</p>}
      {report.original?.metadata.group_id != null && <dl><NativeValue label={t("view.group_replication.field.group")} value={contextPreview(report.original.metadata.group_id).text} /></dl>}
      {report.native_rows != null && <p className="muted">{t("ui.replication.native_rows")}: {digits(String(report.native_rows))}</p>}
      {!report.read_complete && <p>{t("ui.common.read_partial")}</p>}
      <ObservationList collapseEntries source={t("provider.mysql.source.group_replication")} scope={t("view.group_replication.scope")}
        report={{ title: report.title, summary: report.summary, sources: report.sources, limitations: report.limitations, entries: report.entries }}
        renderEntry={(entry) => <ReplicationEntryEvidence key={`${entry.reference.source}:${entry.reference.observation_key}`} entry={entry} />}>
        <ObservationReadReasons sources={report.sources} />
        {report.entries_limited && <p>{t("ui.replication.presentation_limit")}</p>}
      </ObservationList>
    </article>} />;
}
