import type { Engine } from "../../gen/Engine";
import type { IndexStat } from "../../gen/IndexStat";
import type { MssqlCatalogAvailability } from "../../gen/MssqlCatalogAvailability";
import type { SourceState } from "../../gen/SourceState";
import { digits } from "../../lib/format";
import { contextPreview } from "../../lib/context-preview";
import { t } from "../../lib/i18n";

const MAX_COLUMNS = 16;
const MAX_COLUMN_NAME = 256;

/** Render the captured child in its original index; labels only refuse contradictory attachment. */
export function ColumnstoreConfiguration({ engine, index, sourceState }: { engine: Engine; index: IndexStat; sourceState: SourceState | null }) {
  const configuration = index.mssql_columnstore;
  if (engine !== "mssql" || sourceState === "unavailable" || !configuration || configuration.schema !== index.schema
    || configuration.table_name !== index.table || configuration.name !== index.name) return null;
  const columns = configuration.columns.slice(0, MAX_COLUMNS);
  return <tr className="ix-notes"><td colSpan={5}>
    <details className="td-chunks index-columnstore">
      <summary>{t("ui.indexes.columnstore.title")}</summary>
      <p className="muted">{t("ui.indexes.columnstore.description")}</p>
      <dl>
        <dt>{t("ui.indexes.columnstore.compression_delay")}</dt>
        <dd>{nativeValue(configuration.compression_delay_availability, configuration.compression_delay_minutes)}</dd>
        <dt>{t("ui.indexes.columnstore.column_order")}</dt>
        <dd>{availability(configuration.column_store_order_availability)}</dd>
        <dt>{t("ui.indexes.columnstore.data_clustering")}</dt>
        <dd>{availability(configuration.data_clustering_availability)}</dd>
      </dl>
      {columns.length > 0 && <table><thead><tr>
        <th scope="col">{t("ui.plan_type_schema.column")}</th>
        <th scope="col">{t("ui.indexes.columnstore.column_order")}</th>
        <th scope="col">{t("ui.indexes.columnstore.data_clustering")}</th>
      </tr></thead><tbody>{columns.map((column, position) => <tr key={`${column.index_column_id}:${position}`}>
        <td><code>{contextPreview(column.name, MAX_COLUMN_NAME).text}</code></td>
        <td>{nativeValue(configuration.column_store_order_availability, column.column_store_order_ordinal)}</td>
        <td>{nativeValue(configuration.data_clustering_availability, column.data_clustering_ordinal)}</td>
      </tr>)}</tbody></table>}
      {configuration.columns.length > columns.length && <p className="muted">{t("ui.indexes.columnstore.limited", {
        shown: digits(String(columns.length)), total: digits(String(configuration.columns.length)),
      })}</p>}
    </details>
  </td></tr>;
}

function availability(state: MssqlCatalogAvailability): string {
  return t("ui.indexes.columnstore.availability", { state });
}

function nativeValue(state: MssqlCatalogAvailability, value: number | null): string {
  if (state !== "available") return availability(state);
  return value === null ? t("ui.indexes.columnstore.native_null") : digits(String(value));
}
