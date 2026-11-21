import type { Engine } from "../../gen/Engine";
import type { SourceState } from "../../gen/SourceState";
import type { TableStat } from "../../gen/TableStat";
import type { TableTemporalReport } from "../../gen/TableTemporalReport";
import { contextPreview } from "../../lib/context-preview";
import { MssqlTableCatalog } from "./MssqlTableCatalog";

const MAX_FIELD_CHARACTERS = 160;
/** The report and selected table belong to this saved scan; no history-table or cross-capture name lookup occurs. */
export function TemporalConfiguration({ engine, sourceState, table, report }: {
  engine: Engine; sourceState: SourceState | null; table: TableStat; report: TableTemporalReport | null | undefined;
}) {
  if (!report || !table.mssql_temporal_configuration || report.source_state !== sourceState) return null;
  const entries = report.entries.filter((entry) => entry.schema === table.schema && entry.table === table.name);
  const entry = entries.length === 1 ? entries[0] : null;
  if (!entry) return null;
  return <MssqlTableCatalog engine={engine} sourceState={sourceState} title={report.title} sourceQualifiedElsewhere>
    <dl>{entry.fields.map((field) => <div key={field.key}>
      <dt>{field.label}</dt>
      <dd><code>{contextPreview(field.text.replace(/\p{Cc}/gu, " "), MAX_FIELD_CHARACTERS).text}</code></dd>
    </div>)}</dl>
    <p className="muted">{report.qualification}</p>
    {report.limitations.map((limitation) => <p className="muted" key={limitation}>{limitation}</p>)}
  </MssqlTableCatalog>;
}
