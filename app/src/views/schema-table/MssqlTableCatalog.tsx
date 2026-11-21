import type { ReactNode } from "react";
import type { Engine } from "../../gen/Engine";
import type { SourceState } from "../../gen/SourceState";
import { t } from "../../lib/i18n";

/** Native catalog details keep source availability separate from the selected table's values. */
export function MssqlTableCatalog({ engine, sourceState, title, children, sourceQualifiedElsewhere = false }: {
  engine: Engine; sourceState: SourceState | null; title: string; children: ReactNode; sourceQualifiedElsewhere?: boolean;
}) {
  if (engine !== "mssql" || (sourceState !== null && sourceState !== "collected" && sourceState !== "partial")) return null;
  return <details className="td-chunks td-catalog">
    <summary>{title}</summary>
    {children}
    {!sourceQualifiedElsewhere && sourceState === "partial" && <p className="muted">{t("view.table_catalog.partial")}</p>}
    {!sourceQualifiedElsewhere && sourceState === null && <p className="muted">{t("view.table_catalog.unknown_source")}</p>}
  </details>;
}
