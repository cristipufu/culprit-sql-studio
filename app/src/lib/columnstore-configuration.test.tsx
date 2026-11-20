import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import english from "../../../locales/en.json";
import type { Engine } from "../gen/Engine";
import type { IndexStat } from "../gen/IndexStat";
import type { Report } from "../gen/Report";
import type { TableStat } from "../gen/TableStat";
import { partial } from "../test/fixtures";
import { ColumnstoreConfiguration } from "../views/schema-table/ColumnstoreConfiguration";
import { Indexes } from "../views/schema-table/Indexes";
import { t } from "./i18n";

function configured(): IndexStat {
  return partial<IndexStat>({
    schema: "dbo", table: "Readings", name: "cci_readings", columns: ["id", "payload"], include: [],
    mssql_columnstore: {
      table: { database_id: 7, object_id: 100, created_without_offset: "2026-01-02T03:04:05.000" },
      index_id: 1, schema: "dbo", table_name: "Readings", name: "cci_readings",
      table_modified_without_offset: "2026-01-02T03:04:05.000",
      compression_delay_availability: "available", compression_delay_minutes: 15,
      column_store_order_availability: "available", data_clustering_availability: "unsupported",
      columns: [
        { index_column_id: 1, column_id: 1, name: "id", column_store_order_ordinal: 1, data_clustering_ordinal: null },
        { index_column_id: 2, column_id: 2, name: "payload", column_store_order_ordinal: 0, data_clustering_ordinal: null },
      ],
    },
  });
}

function render(index = configured(), engine: Engine = "mssql"): string {
  return renderToStaticMarkup(<table><tbody><ColumnstoreConfiguration engine={engine} index={index} sourceState={null} /></tbody></table>);
}

test("schema index details disclose captured configuration folded without a query or health verdict", () => {
  const index = configured();
  const report = partial<Report>({ snapshot: { server: { engine: "mssql" }, sources: [] }, summary: { index_notes: [], index_counting: "" } });
  const table = partial<TableStat>({ schema: index.schema, name: index.table });
  const html = renderToStaticMarkup(<Indexes report={report} table={table} indexes={[index]} />);
  expect(html).toContain('<details class="td-chunks index-columnstore">');
  expect(html).not.toContain(" open=");
  expect(html).toContain(english["ui.indexes.columnstore.title"]);
  expect(html).toContain(english["ui.indexes.columnstore.description"]);
  expect(html).toContain(`<dt>${english["ui.indexes.columnstore.compression_delay"]}</dt><dd>15</dd>`);
  expect(html).toContain("<td><code>id</code></td><td>1</td><td>Not supported</td>");
  expect(html).toContain("<td><code>payload</code></td><td>0</td><td>Not supported</td>");
});

test("schema index details withhold unavailable source facts and preserve partial or missing coverage", () => {
  const index = configured();
  const table = partial<TableStat>({ schema: index.schema, name: index.table });
  for (const state of ["collected", "partial", "unavailable", null, undefined] as const) {
    const report = partial<Report>({
      snapshot: { server: { engine: "mssql" }, sources: state === undefined ? undefined : state === null ? [] : [{ source: "indexes", state }] },
      summary: { index_notes: [], index_counting: "" },
    });
    const html = renderToStaticMarkup(<Indexes report={report} table={table} indexes={[index]} />);
    expect(html.includes('<details class="td-chunks index-columnstore">')).toBe(state !== "unavailable");
  }
});

test("native compression NULL, zero and configured minutes remain three distinct values", () => {
  for (const [value, expected] of [[null, "Native NULL"], [0, "0"], [15, "15"]] as const) {
    const index = configured();
    index.mssql_columnstore!.compression_delay_minutes = value;
    const html = render(index);
    expect(html.match(/<dd>[^<]*<\/dd>/g)).toEqual([
      `<dd>${expected}</dd>`, "<dd>Available</dd>", "<dd>Not supported</dd>",
    ]);
    expect(html).toContain("<td><code>payload</code></td><td>0</td><td>Not supported</td>");
  }
});

test("unsupported, denied and unknown fields retain availability independently of known native ordinals", () => {
  for (const state of ["unsupported", "permission-denied", "unknown"] as const) {
    const index = configured();
    const configuration = index.mssql_columnstore!;
    configuration.compression_delay_availability = state;
    configuration.compression_delay_minutes = null;
    configuration.data_clustering_availability = "available";
    configuration.columns[0]!.data_clustering_ordinal = 1;
    configuration.columns[1]!.data_clustering_ordinal = 0;
    const html = render(index);
    expect(html.match(/<dd>[^<]*<\/dd>/g)).toEqual([
      `<dd>${t("ui.indexes.columnstore.availability", { state })}</dd>`, "<dd>Available</dd>", "<dd>Available</dd>",
    ]);
    expect(html).toContain("<td><code>payload</code></td><td>0</td><td>0</td>");
  }
});

test("column details are bounded, preserve Unicode and render native names as text", () => {
  const index = configured();
  const configuration = index.mssql_columnstore!;
  configuration.columns = Array.from({ length: 17 }, (_, position) => ({
    index_column_id: position + 1, column_id: position + 1,
    name: position === 16 ? "hidden-column-canary" : `column_${position}`,
    column_store_order_ordinal: 0, data_clustering_ordinal: null,
  }));
  configuration.columns[0]!.name = "🧪".repeat(300);
  configuration.columns[1]!.name = '<script>native & "column"</script>';
  const html = render(index);
  expect(html).toContain(`${"🧪".repeat(256)}…`);
  expect(html).toContain("&lt;script&gt;native &amp; &quot;column&quot;&lt;/script&gt;");
  expect(html.match(/<td><code>/g)).toHaveLength(16);
  expect(html).toContain(t("ui.indexes.columnstore.limited", { shown: "16", total: "17" }));
  expect(html).not.toContain("hidden-column-canary");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain(configuration.table.created_without_offset);
});

test("missing or contradictory attached metadata and other engines do not acquire a disclosure", () => {
  const missing = configured();
  missing.mssql_columnstore = null;
  expect(render(missing)).toBe("<table><tbody></tbody></table>");
  for (const field of ["schema", "table_name", "name"] as const) {
    const index = configured();
    index.mssql_columnstore![field] = "another native owner";
    expect(render(index)).toBe("<table><tbody></tbody></table>");
  }
  for (const engine of ["postgres", "mysql", "sqlite", "yugabyte"] as const) {
    expect(render(configured(), engine)).toBe("<table><tbody></tbody></table>");
  }
});
