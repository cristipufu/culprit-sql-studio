import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Engine } from "../gen/Engine";
import type { SourceState } from "../gen/SourceState";
import type { TableStat } from "../gen/TableStat";
import type { Report } from "../gen/Report";
import { fromRust, partial } from "../test/fixtures";
import demo from "../../../fixtures/mssql-demo.report.json";
import { Header } from "../views/schema-table/Header";
import { StorageConfiguration } from "../views/schema-table/StorageConfiguration";
import { t } from "./i18n";

function table(memory: boolean | null, code: number | null, description: string | null): TableStat {
  return partial<TableStat>({ mssql_storage_configuration: {
    memory_optimized: memory, durability: code, durability_desc: description,
  } });
}
function render(value: TableStat, sourceState: SourceState | null = "collected", engine: Engine = "mssql"): string {
  return renderToStaticMarkup(<StorageConfiguration engine={engine} table={value} sourceState={sourceState} />);
}

test("table configuration is folded context with exact native durability values and no health badge", () => {
  for (const [memory, code, description] of [[false, 0, "SCHEMA_AND_DATA"], [true, 0, "SCHEMA_AND_DATA"], [true, 1, "SCHEMA_ONLY"]] as const) {
    const html = render(table(memory, code, description));
    expect(html).toContain("<details"); expect(html).not.toContain(" open=");
    expect(html).toContain(`<dd>${t(memory ? "ui.common.yes" : "ui.common.no")}</dd>`);
    expect(html).toContain(`<dd>${code}</dd>`);
    expect(html).toContain(`<code>${description}</code>`);
    expect(html).toContain(t("view.table_storage.qualification"));
    expect(html).not.toContain('class="sev');
  }
});

test("native NULLs remain unknown rather than rowstore or default durability", () => {
  const html = render(table(null, null, null));
  expect(Array.from(html.match(/<dd>[^<]*<\/dd>/g) ?? [])).toEqual(Array(3).fill(`<dd>${t("ui.indexes.columnstore.native_null")}</dd>`));
});

test("source refusal withholds settings while partial and unknown scope keep their original qualification", () => {
  const value = table(true, 1, "SCHEMA_ONLY");
  for (const state of ["unavailable", "not-applicable", "empty"] as const) expect(render(value, state)).toBe("");
  expect(render(value, "partial")).toContain(t("view.table_storage.partial"));
  expect(render(value, null)).toContain(t("view.table_storage.unknown_source"));
  expect(render(value, "collected", "postgres")).toBe("");
  expect(render(partial<TableStat>({ mssql_storage_configuration: null }))).toBe("");
});

test("native description is bounded escaped text including control characters", () => {
  const html = render(table(true, null, "<script>\n\u001b[31m" + "界".repeat(100)));
  expect(html).toContain("&lt;script&gt;"); expect(html).not.toContain("<script>");
  expect(html).not.toContain("\u001b"); expect(html).not.toContain("界".repeat(61));
});


test("the existing table header exposes its captured child and never borrows another table’s configuration", () => {
  const report = fromRust<Report>(demo);
  const selected = report.snapshot.tables[0]!;
  selected.mssql_storage_configuration = table(true, 1, "SCHEMA_ONLY").mssql_storage_configuration;
  const html = renderToStaticMarkup(<Header report={report} table={selected} findings={[]} />);
  expect(html).toContain(t("view.table_storage.title"));
  expect(html).toContain("<code>SCHEMA_ONLY</code>");
  const other = { ...selected, name: "IndependentTable", mssql_storage_configuration: null };
  expect(renderToStaticMarkup(<Header report={report} table={other} findings={[]} />)).not.toContain(t("view.table_storage.title"));
});
