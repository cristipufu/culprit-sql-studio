import type { Engine } from "../../gen/Engine";
import type { SourceState } from "../../gen/SourceState";
import type { TableStat } from "../../gen/TableStat";
import { t } from "../../lib/i18n";
import { MssqlTableCatalog } from "./MssqlTableCatalog";

export function CdcEnrollment({ engine, table, sourceState }: { engine: Engine; table: TableStat; sourceState: SourceState | null }) {
  const tracked = table.mssql_tracked_by_cdc;
  return <MssqlTableCatalog engine={engine} sourceState={sourceState} title={t("view.table_cdc.title")}>
    <dl>
      <dt>{t("view.table_cdc.tracked")}</dt>
      <dd>{tracked == null ? t("ui.indexes.columnstore.native_null") : t(tracked ? "ui.common.yes" : "ui.common.no")}</dd>
    </dl>
    <p className="muted">{t("view.table_cdc.qualification")}</p>
  </MssqlTableCatalog>;
}
