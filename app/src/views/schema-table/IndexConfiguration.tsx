import type { IndexStat } from "../../gen/IndexStat";
import type { Report } from "../../gen/Report";
import type { TableStat } from "../../gen/TableStat";
import { IndexConfigurationDetails } from "../../components/IndexConfigurations";

/** Current report entries match the embedded native owner, never a historical capture with similar labels. */
export function IndexConfiguration({ report, index, table }: { report: Report; index?: IndexStat; table?: TableStat }) {
  const native = index?.mssql_configuration ?? table?.mssql_heap_configuration;
  if (!native || native.parent_kind !== "table" || report.snapshot.server.engine !== "mssql") return null;
  const expectedDataset = index ? "indexes" : "tables";
  if (report.snapshot.sources?.find((source) => source.source === expectedDataset)?.state === "unavailable") return null;
  const entries = report.index_configuration.filter((view) => view.context === null && view.dataset === expectedDataset && view.source_state !== "unavailable")
    .flatMap((view) => view.entries.map((entry) => ({ view, entry }))).filter(({ entry }) => entry.parent_kind === native.parent_kind && entry.parent_object.database_id === native.parent_object.database_id
      && entry.parent_object.object_id === native.parent_object.object_id
      && entry.parent_object.created_without_offset === native.parent_object.created_without_offset
      && entry.parent_modified_without_offset === native.parent_modified_without_offset
      && entry.index_id === native.index_id && entry.index_name === native.name
      && entry.parent_schema === native.parent_schema && entry.parent_name === native.parent_name);
  if (entries.length !== 1) return null;
  const { entry, view } = entries[0]!;
  const details = <IndexConfigurationDetails key={`${report.snapshot.id}:${entry.index_id}:${entry.parent_object.created_without_offset}`} entry={entry} qualifications={[view.qualification, ...view.limitations, ...(view.source_reason ? [view.source_reason] : [])]} />;
  return index ? <tr className="ix-notes"><td colSpan={5}>{details}</td></tr> : details;
}
