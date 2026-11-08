import { ArrowRight } from "lucide-react";
import { t } from "../../lib/i18n";
import type { TableRelations } from "../../lib/schema-model";

/** The foreign keys from this table, with whether an index backs each, and those that point to it. */
export function Relations({ links, onOpenTable }: { links: TableRelations; onOpenTable: (key: string) => void }) {
  if (links.outgoing.length === 0 && links.incoming.length === 0) return <div className="notice">{t("ui.relations.none")}</div>;
  return (
    <div className="rel">
      <section>
        <h4>
          {t("ui.relations.references")} <span className="muted">{links.outgoing.length}</span>
        </h4>
        {links.outgoing.length === 0 && <p className="muted">{t("ui.relations.references_none")}</p>}
        {links.outgoing.map(({ fk, indexed }) => (
          <div key={fk.name} className="rel-row">
            <div className="rel-path">
              <span className="mono rel-columns" title={fk.columns.join(", ")}>{fk.columns.join(", ")}</span>
              <ArrowRight size={14} className="muted" />
              <div className="rel-endpoint">
                <button type="button" className="rel-target" title={`${fk.ref_schema}.${fk.ref_table}`} onClick={() => onOpenTable(`${fk.ref_schema}.${fk.ref_table}`)}>
                  {fk.ref_schema}.{fk.ref_table}
                </button>
                <span className="mono muted rel-columns" title={fk.ref_columns.join(", ")}>({fk.ref_columns.join(", ")})</span>
              </div>
            </div>
            <div className="rel-meta">
              <span className="muted rel-name" title={fk.name}>{fk.name}</span>
              {indexed == null ? (
                <span className="tag" title={t("ui.relations.support_unknown_title")}>
                  {t("ui.relations.support_unknown")}
                </span>
              ) : indexed ? (
                <span className="tag good" title={t("ui.relations.indexed_title")}>
                  {t("ui.relations.indexed")}
                </span>
              ) : (
                <span className="tag medium" title={t("ui.relations.not_indexed_title", { table: fk.ref_table, columns: fk.columns.join(", ") })}>
                  {t("ui.relations.not_indexed")}
                </span>
              )}
            </div>
          </div>
        ))}
      </section>
      <section>
        <h4>
          {t("ui.relations.referenced_by")} <span className="muted">{links.incoming.length}</span>
        </h4>
        {links.incoming.length === 0 && <p className="muted">{t("ui.relations.referenced_by_none")}</p>}
        {links.incoming.map((fk) => (
          <div key={`${fk.schema}.${fk.table}.${fk.name}`} className="rel-row">
            <div className="rel-path">
              <div className="rel-endpoint">
                <button type="button" className="rel-target" title={`${fk.schema}.${fk.table}`} onClick={() => onOpenTable(`${fk.schema}.${fk.table}`)}>
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
