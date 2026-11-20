import type { Engine } from "../../gen/Engine";
import type { SourceState } from "../../gen/SourceState";
import type { TableStat } from "../../gen/TableStat";
import { contextPreview } from "../../lib/context-preview";
import { digits } from "../../lib/format";
import { t } from "../../lib/i18n";

const MAX_NATIVE_DESCRIPTION = 60;

/** The original table's native catalog fields are context, without a health or data-loss interpretation. */
export function StorageConfiguration({ engine, table, sourceState }: {
  engine: Engine; table: TableStat; sourceState: SourceState | null;
}) {
  const configuration = table.mssql_storage_configuration;
  if (engine !== "mssql" || !configuration || (sourceState !== null && sourceState !== "collected" && sourceState !== "partial")) return null;
  const unknown = t("ui.indexes.columnstore.native_null");
  const description = configuration.durability_desc === null
    ? null
    : contextPreview(configuration.durability_desc.replace(/\p{Cc}/gu, " "), MAX_NATIVE_DESCRIPTION).text;
  return <details className="td-chunks td-catalog">
    <summary>{t("view.table_storage.title")}</summary>
    <dl>
      <dt>{t("view.table_storage.field.memory_optimized")}</dt>
      <dd>{configuration.memory_optimized === null ? unknown : t(configuration.memory_optimized ? "ui.common.yes" : "ui.common.no")}</dd>
      <dt>{t("view.table_storage.field.durability")}</dt>
      <dd>{configuration.durability === null ? unknown : digits(String(configuration.durability))}</dd>
      <dt>{t("view.table_storage.field.durability_desc")}</dt>
      <dd>{description === null ? unknown : <code>{description}</code>}</dd>
    </dl>
    <p className="muted">{t("view.table_storage.qualification")}</p>
    {sourceState === "partial" && <p className="muted">{t("view.table_storage.partial")}</p>}
    {sourceState === null && <p className="muted">{t("view.table_storage.unknown_source")}</p>}
  </details>;
}
