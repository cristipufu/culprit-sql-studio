import type { HistoricalSourceOverhead } from "../gen/HistoricalSourceOverhead";
import { contextPreview } from "../lib/context-preview";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { ObservationList } from "./ObservationList";
import { ConnectionWorkDetails, NativeReadDetails, ReadTemplateValues } from "./ReadWorkDetails";

/** Historical capture acquisition only. No current database, native owner or request scope is inferred. */
export function SourceAcquisitions({ reports }: { reports: HistoricalSourceOverhead[] }) {
  if (reports.length === 0) return null;
  const entries = reports.map((entry, index) => ({
    entry,
    identity: entry.kind === "available"
      ? `${entry.report.evidence.capture.observation_id}:${entry.manifest_sha256}`
      : `unavailable:${index}`,
  }));
  return <ObservationList collapseEntries source={t("view.source_overhead.title")} scope={t("ui.source_acquisitions.scope")}
    report={{ title: t("view.source_overhead.title"), summary: t("view.source_overhead.saved_summary"),
      sources: [], limitations: [t("view.source_overhead.limit.historical")], entries }} renderEntry={({ entry, identity }) => {
      if (entry.kind === "unavailable") return <article className="operational-card" key={identity}><p>{entry.reason}</p></article>;
      const report = entry.report;
      const evidence = report.evidence;
      return <article className="operational-card" key={identity}>
        <h4><code>{contextPreview(evidence.capture.observation_id).text}</code></h4>
        <p>{report.summary}</p>
        <p className="muted">{evidence.finished_at == null ? t("view.source_overhead.cli.unfinished", { from: evidence.started_at })
          : t("view.source_overhead.cli.bounds", { from: evidence.started_at, until: evidence.finished_at })}</p>
        {evidence.source_queries != null && <p>{t("view.source_overhead.cli.total", { count: digits(String(evidence.source_queries)) })}</p>}
        {report.connection_work && <ConnectionWorkDetails report={report.connection_work}/>}
        {report.native_interval && <NativeReadDetails report={report.native_interval}/>}
        {evidence.read_metrics && <details><summary>{t("ui.retained_failures.observations")}</summary>
          <ReadTemplateValues metrics={evidence.read_metrics} order={report.template_order}/>
        </details>}
        <details><summary>{t("ui.memory_failures.limitations")}</summary>
          {report.limitations.map((text, index) => <p className="muted" key={index}>{text}</p>)}
        </details>
      </article>;
    }} />;
}
