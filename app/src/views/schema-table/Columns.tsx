import { KeyRound, Link2 } from "lucide-react";
import type { IndexStat } from "../../gen/IndexStat";
import type { Report } from "../../gen/Report";
import type { TableStat } from "../../gen/TableStat";
import { columnMembership, referenceOf, tableKey, type TableRelations } from "../../lib/schema-model";

/** Each column with its type, the indexes it is a key of, and the foreign key it references. */
export function Columns({
  report,
  table,
  indexes,
  links,
  onOpenTable,
}: {
  report: Report;
  table: TableStat;
  indexes: IndexStat[];
  links: TableRelations;
  onOpenTable: (key: string) => void;
}) {
  const columns = report.snapshot.columns.filter((c) => c.schema === table.schema && c.table === table.name).sort((a, b) => a.ordinal - b.ordinal);
  if (columns.length === 0) {
    const note = report.snapshot.notes.find((n) => n.collector === "catalog" && n.message.startsWith(`Columns of ${tableKey(table)} are unavailable:`));
    return <div className="notice">{note?.message ?? "This scan has no column metadata. Re-scan to collect it."}</div>;
  }
  return (
    // biome-ignore lint/a11y/noNoninteractiveTabindex: a scrolling region takes focus so the keyboard can scroll it
    <div className="td-grid-wrap" role="region" aria-label="Columns" tabIndex={0}>
      <table className="grid td-grid columns-grid">
        <colgroup>
          <col className="num-col" />
          <col className="column-name-col" />
          <col className="column-type-col" />
          <col className="nullable-col" />
          <col />
        </colgroup>
        <thead>
          <tr>
            <th className="num-col">#</th>
            <th>Column</th>
            <th>Type</th>
            <th>Nullable</th>
            <th>In indexes</th>
          </tr>
        </thead>
        <tbody>
          {columns.map((c) => {
            const member = columnMembership(indexes, c.name);
            const pk = member.some((m) => m.index.is_primary_key && m.position != null);
            const ref = referenceOf(links.outgoing, c.name);
            const chips = member.filter((m) => !m.index.is_primary_key);
            return (
              <tr key={c.name} className="row">
                <td className="num-col muted">{c.ordinal}</td>
                <td className="col-name">
                  <span className="cn">
                    {pk && <KeyRound size={12} className="pk-icon" aria-label="Primary key" />}
                    <span className="mono" title={c.name}>
                      {c.name}
                    </span>
                    {c.generated && <span className="tag">auto</span>}
                  </span>
                  {ref && (
                    <button type="button" className="fk-link" title={`${ref.ref_table}.${ref.ref_columns[0]} · Foreign key ${ref.name}`} onClick={() => onOpenTable(`${ref.ref_schema}.${ref.ref_table}`)}>
                      <Link2 size={11} />
                      <span>{ref.ref_table}.{ref.ref_columns[0]}</span>
                    </button>
                  )}
                </td>
                <td className="type-col" title={c.default ? `${c.data_type} DEFAULT ${c.default}` : c.data_type}>
                  <span className="mono muted">{c.data_type}</span>
                  {c.default && <span className="mono default">= {c.default}</span>}
                </td>
                <td className="muted">{c.nullable ? "Yes" : "No"}</td>
                <td className="chips-col">
                  <div className="column-indexes">
                    {chips.slice(0, 3).map((m) => (
                      <span
                        key={m.index.name}
                        className={`chip-ix ${m.position === 0 ? "lead" : ""}`}
                        title={
                          m.included
                            ? `${m.index.name}: included column (stored, not searchable)`
                            : m.position === 0
                              ? `${m.index.name}: leading key, so searches on ${c.name} can seek`
                              : `${m.index.name}: key ${m.position! + 1} of ${m.index.columns.length}`
                        }
                      >
                        <span className="nm">{m.index.name}</span>
                        <span className="pos">
                          {m.included ? "incl" : m.position === 0 ? "1st" : `${m.position! + 1}${m.position === 1 ? "nd" : m.position === 2 ? "rd" : "th"}`}
                        </span>
                      </span>
                    ))}
                    {chips.length > 3 && (
                      <span
                        className="chip-ix more"
                        title={chips
                          .slice(3)
                          .map((m) => m.index.name)
                          .join(", ")}
                      >
                        +{chips.length - 3}
                      </span>
                    )}
                    {chips.length === 0 && <span className="muted">—</span>}
                  </div>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
