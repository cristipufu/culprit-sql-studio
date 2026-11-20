import type { TableMemoryReport } from "../gen/TableMemoryReport";
import { contextPreview } from "../lib/context-preview";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList, ObservationReadQualifications, ObservationReadReasons } from "./ObservationList";

/** Independent original captures remain separate; current table names and scan clocks supply no association. */
export function TableMemory({ reports }: { reports: TableMemoryReport[] }) {
  if (reports.length === 0) return null;
  return <ObservationList collapseEntries source={t("provider.mssql.source.table_memory")} scope={t("view.table_memory.scope")}
    report={{ title: t("view.table_memory.title"), summary: t("view.table_memory.saved_summary"), sources: [], limitations: [], entries: reports }}
    renderEntry={(report) => {
      const original = report.original;
      return <article className="operational-card" key={reports.indexOf(report)}>
        <h4>{original ? <code>{contextPreview(original.metadata.schema).text}.{contextPreview(original.metadata.name).text}</code> : report.title}</h4>
        <p>{report.summary}</p>
        <ObservationReadQualifications sources={[report.source]} />
        {original && <>
          <p className="muted">{t("ui.memory_failures.read_between", { from: original.context.observed_from, until: original.context.observed_until })}</p>
          <dl>
            <NativeValue label={t("view.database_properties.native_database_id")} value={digits(String(original.target.database_id))} />
            <NativeValue label={t("view.table_memory.object_id")} value={digits(String(original.target.object_id))} />
            <NativeValue label={t("view.storage.field.observed_at")} value={original.metadata.observed_at} />
          </dl>
        </>}
        <dl>{report.metrics.map((metric) => <NativeValue key={metric.key} label={metric.label} value={metric.text} />)}</dl>
        <details><summary>{t("ui.memory_failures.limitations")}</summary>
          <ObservationReadReasons sources={[report.source]} />
          {report.limitations.map((limitation) => <p className="muted" key={limitation}>{limitation}</p>)}
        </details>
      </article>;
    }} />;
}
