import type { IndexStat } from "../../gen/IndexStat";
import type { Report } from "../../gen/Report";
import type { TableStat } from "../../gen/TableStat";
import { IndexConfigurationDetails } from "../../components/IndexConfigurations";

/** Current report entries match the embedded native owner, never a historical capture with similar labels. */
export function IndexConfiguration({ report, index, table }: { report: Report; index?: IndexStat; table?: TableStat }) {
  const native = index?.mssql_configuration ?? table?.mssql_heap_configuration;
  if (!native || report.snapshot.server.engine !== "mssql") return null;
  const expectedDataset = index ? "indexes" : "tables";
  if (report.snapshot.sources?.find((source) => source.source === expectedDataset)?.state === "unavailable") return null;
  const entries = report.index_configuration.filter((view) => view.context === null && view.dataset === expectedDataset && view.source_state !== "unavailable")
    .flatMap((view) => view.entries.map((entry) => ({ view, entry }))).filter(({ entry }) => entry.native_table.database_id === native.table.database_id
      && entry.native_table.object_id === native.table.object_id
      && entry.native_table.created_without_offset === native.table.created_without_offset
      && entry.table_modified_without_offset === native.table_modified_without_offset
      && entry.index_id === native.index_id && entry.index_name === native.name
      && entry.schema === native.schema && entry.table === native.table_name);
  if (entries.length !== 1) return null;
  const { entry, view } = entries[0]!;
  const details = <IndexConfigurationDetails key={`${report.snapshot.id}:${entry.index_id}:${entry.native_table.created_without_offset}`} entry={entry} qualifications={[view.qualification, ...view.limitations, ...(view.source_reason ? [view.source_reason] : [])]} />;
  return index ? <tr className="ix-notes"><td colSpan={5}>{details}</td></tr> : details;
}
