import type { FullTextPopulationReport } from "../gen/FullTextPopulationReport";
import { contextPreview } from "../lib/context-preview";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Each entry describes its own authenticated original; current table names and scan clocks supply no join. */
export function FullTextPopulations({ reports }: { reports: FullTextPopulationReport[] }) {
  if (reports.length === 0) return null;
  return <ObservationList collapseEntries source={t("provider.mssql.source.full_text_populations")} scope={t("view.full_text_populations.scope")}
    report={{ title: t("view.full_text_populations.title"), summary: t("view.full_text_populations.saved_summary"), sources: [], limitations: [], entries: reports }}
    renderEntry={(report) => {
      const original = report.original;
      return <article className="operational-card" key={reports.indexOf(report)}>
        {original && <h4><code>{contextPreview((original.metadata.schema ?? t("view.full_text_populations.unknown")).replace(/\p{Cc}/gu," "),256).text}.{contextPreview((original.metadata.table ?? t("view.full_text_populations.unknown")).replace(/\p{Cc}/gu," "),256).text}</code></h4>}
        {original && <p>{t("view.full_text_populations.target", {
          database: digits(String(original.target.table.database_id)), table: digits(String(original.target.table.object_id)),
          catalog: digits(String(original.target.catalog_id)), index: digits(String(original.target.unique_index_id)),
        })}</p>}
        {original && <dl><NativeValue label={t("view.storage.field.observed_at")} value={original.metadata.observed_at} /></dl>}
        <ObservationList collapseEntries source={t("provider.mssql.source.full_text_populations")} scope={t("view.full_text_populations.scope")}
          time={original ? t("ui.memory_failures.read_between", { from: original.context.observed_from, until: original.context.observed_until }) : null}
          report={{ title: report.title, summary: report.summary, sources: [report.source], limitations: report.limitations, entries: report.entries }}
          renderEntry={(entry) => <div className="operational-card" key={entry.observation_key}>
            <h4>{entry.summary}</h4>
            <dl><NativeValue label={t("view.storage.field.observed_at")} value={entry.observed_at} />{entry.metrics.map((metric) => <NativeValue key={metric.key} label={metric.label}
              value={contextPreview((metric.text ?? t("view.full_text_populations.unknown")).replace(/\p{Cc}/gu, " "), 256).text} />)}</dl>
          </div>} />
        <p>{report.investigation}</p><p>{report.verify}</p>
      </article>;
    }} />;
}
