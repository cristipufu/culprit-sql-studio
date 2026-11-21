import type { GaleraReport } from "../gen/GaleraReport";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { ObservationList, ObservationReadQualifications, ObservationReadReasons } from "./ObservationList";
import { ReplicationEntryEvidence } from "./ReplicationEvidence";

/** Independent historical native node observations never grade the current scan database. */
export function Galera({ reports }: { reports: GaleraReport[] }) {
  if (reports.length === 0) return null;
  return <ObservationList collapseEntries source={t("provider.mysql.source.galera")} scope={t("view.galera.scope")}
    report={{ title: t("view.galera.title"), summary: t("view.galera.saved_summary"), sources: [], limitations: [], entries: reports }}
    renderEntry={(report) => <article className="operational-card" key={reports.indexOf(report)}>
      <h4>{report.title}</h4><p>{report.summary}</p>
      <ObservationReadQualifications sources={report.sources} />
      {report.original && <p className="muted">{t("ui.memory_failures.read_between", { from: report.original.context.observed_from, until: report.original.context.observed_until })}</p>}
      {report.native_rows != null && <p className="muted">{t("ui.replication.native_rows")}: {digits(String(report.native_rows))}</p>}
      {!report.read_complete && <p>{t("ui.common.read_partial")}</p>}
      <ObservationList collapseEntries source={t("provider.mysql.source.galera")} scope={t("view.galera.scope")}
        report={{ title: report.title, summary: report.summary, sources: report.sources, limitations: report.limitations, entries: report.entries }}
        renderEntry={(entry) => <ReplicationEntryEvidence key={`${entry.reference.source}:${entry.reference.observation_key}`} entry={entry} />}>
        <ObservationReadReasons sources={report.sources} />
      </ObservationList>
    </article>} />;
}
