import { KeyRound, Link2 } from "lucide-react";
import type { IndexStat } from "../../gen/IndexStat";
import type { Report } from "../../gen/Report";
import type { TableStat } from "../../gen/TableStat";
import { t } from "../../lib/i18n";
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
    return <div className="notice">{note?.message ?? t("ui.columns.no_metadata")}</div>;
  }
  return (
    // biome-ignore lint/a11y/noNoninteractiveTabindex: a scrolling region takes focus so the keyboard can scroll it
    <div className="td-grid-wrap" role="region" aria-label={t("ui.schema_table.columns")} tabIndex={0}>
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
            <th>{t("ui.columns.column")}</th>
            <th>{t("ui.columns.type")}</th>
            <th>{t("ui.columns.nullable")}</th>
            <th>{t("ui.columns.in_indexes")}</th>
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
                    {pk && <KeyRound size={12} className="pk-icon" aria-label={t("ui.columns.primary_key")} />}
                    <span className="mono" title={c.name}>
                      {c.name}
                    </span>
                    {c.generated && <span className="tag">{t("ui.columns.generated")}</span>}
                  </span>
                  {ref && (
                    <button type="button" className="fk-link" title={t("ui.columns.foreign_key", { target: `${ref.ref_table}.${ref.ref_columns[0]}`, name: ref.name })} onClick={() => onOpenTable(`${ref.ref_schema}.${ref.ref_table}`)}>
                      <Link2 size={11} />
                      <span>{ref.ref_table}.{ref.ref_columns[0]}</span>
                    </button>
                  )}
                </td>
                <td className="type-col" title={c.default ? `${c.data_type} DEFAULT ${c.default}` : c.data_type}>
                  <span className="mono muted">{c.data_type}</span>
                  {c.default && <span className="mono default">= {c.default}</span>}
                </td>
                <td className="muted">{c.nullable ? t("ui.common.yes") : t("ui.common.no")}</td>
                <td className="chips-col">
                  <div className="column-indexes">
                    {chips.slice(0, 3).map((m) => (
                      <span
                        key={m.index.name}
                        className={`chip-ix ${m.position === 0 ? "lead" : ""}`}
                        title={
                          m.included
                            ? t("ui.columns.included_title", { index: m.index.name })
                            : m.position === 0
                              ? t("ui.columns.leading_title", { index: m.index.name, column: c.name })
                              : t("ui.columns.key_title", { index: m.index.name, position: String(m.position! + 1), count: String(m.index.columns.length) })
                        }
                      >
                        <span className="nm">{m.index.name}</span>
                        <span className="pos">
                          {m.included ? t("ui.columns.included") : t("ui.columns.position", { position: String(m.position! + 1) })}
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
