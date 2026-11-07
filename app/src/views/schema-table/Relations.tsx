import { ArrowRight } from "lucide-react";
import type { TableRelations } from "../../lib/schema-model";

/** The foreign keys from this table, with whether an index backs each, and those that point to it. */
export function Relations({ links, onOpenTable }: { links: TableRelations; onOpenTable: (key: string) => void }) {
  if (links.outgoing.length === 0 && links.incoming.length === 0) return <div className="notice">No foreign keys point from or to this table.</div>;
  return (
    <div className="rel">
      <section>
        <h4>
          References <span className="muted">{links.outgoing.length}</span>
        </h4>
        {links.outgoing.length === 0 && <p className="muted">This table doesn't reference others.</p>}
        {links.outgoing.map(({ fk, indexed }) => (
          <div key={fk.name} className="rel-row">
            <div className="rel-path">
              <span className="mono rel-columns" title={fk.columns.join(", ")}>{fk.columns.join(", ")}</span>
              <ArrowRight size={14} className="muted" />
              <div className="rel-endpoint">
                <button className="rel-target" title={`${fk.ref_schema}.${fk.ref_table}`} onClick={() => onOpenTable(`${fk.ref_schema}.${fk.ref_table}`)}>
                  {fk.ref_schema}.{fk.ref_table}
                </button>
                <span className="mono muted rel-columns" title={fk.ref_columns.join(", ")}>({fk.ref_columns.join(", ")})</span>
              </div>
            </div>
            <div className="rel-meta">
              <span className="muted rel-name" title={fk.name}>{fk.name}</span>
              {indexed == null ? (
                <span className="tag" title="Native sharding metadata was unavailable; lookup support cannot be established">
                  index support unknown
                </span>
              ) : indexed ? (
                <span className="tag good" title="An index leads with these columns, so joins and parent deletes can seek">
                  indexed
                </span>
              ) : (
                <span className="tag medium" title={`Deleting or updating a ${fk.ref_table} row scans this table to check ${fk.columns.join(", ")}`}>
                  not indexed
                </span>
              )}
            </div>
          </div>
        ))}
      </section>
      <section>
        <h4>
          Referenced by <span className="muted">{links.incoming.length}</span>
        </h4>
        {links.incoming.length === 0 && <p className="muted">No other table references this one.</p>}
        {links.incoming.map((fk) => (
          <div key={`${fk.schema}.${fk.table}.${fk.name}`} className="rel-row">
            <div className="rel-path">
              <div className="rel-endpoint">
                <button className="rel-target" title={`${fk.schema}.${fk.table}`} onClick={() => onOpenTable(`${fk.schema}.${fk.table}`)}>
                  {fk.schema}.{fk.table}
                </button>
                <span className="mono muted rel-columns" title={fk.columns.join(", ")}>({fk.columns.join(", ")})</span>
              </div>
              <ArrowRight size={14} className="muted" />
              <span className="mono rel-columns" title={fk.ref_columns.join(", ")}>{fk.ref_columns.join(", ")}</span>
            </div>
            <div className="rel-meta">
              <span className="muted rel-name" title={fk.name}>{fk.name}</span>
            </div>
          </div>
        ))}
      </section>
    </div>
  );
}
