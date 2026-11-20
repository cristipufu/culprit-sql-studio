import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Report } from "../gen/Report";
import type { TableFullTextReport } from "../gen/TableFullTextReport";
import { FullText } from "../views/schema-table/FullText";
import { Header } from "../views/schema-table/Header";
import { fromRust } from "../test/fixtures";
import demo from "../../../fixtures/mssql-demo.report.json";
import { t } from "./i18n";

function details(): TableFullTextReport {
  return {
    title: t("view.table_full_text.title"), summary: t("view.table_full_text.summary", { count: "1" }),
    source_state: "collected", source_reason: null, observed_from: null, observed_until: null,
    native_rows: null, observed_rows: 1, read_complete: null, limitations: [],
    entries: [{ schema: "dbo", table: "Documents", observation_key: null,
      summary: t("view.table_full_text.present"), qualification: t("view.table_full_text.limit.catalog"), verify: t("view.table_full_text.verify"),
      metrics: [{ key: "view.table_full_text.field.enabled", label: t("view.table_full_text.field.enabled"), value: 0, unit: "text", text: "<script>native & text</script>" }],
    }],
  };
}

test("full-text catalog details are folded plain text with their canonical qualification", () => {
  const report = details();
  const html = renderToStaticMarkup(<FullText report={report} entry={report.entries[0]!} />);
  expect(html).toContain("<details"); expect(html).not.toContain(" open");
  expect(html).toContain("&lt;script&gt;native &amp; text&lt;/script&gt;"); expect(html).not.toContain("<script>");
  expect(html).toContain(report.entries[0]!.qualification); expect(html).toContain(report.entries[0]!.verify);
  expect(html).not.toContain("class=\"sev");
});

test("unavailable and no-index summaries retain honest empty fields instead of a healthy measurement", () => {
  for (const key of ["view.table_full_text.absent", "view.table_full_text.denied", "view.table_full_text.unsupported", "view.table_full_text.unknown"]) {
    const report = details(); report.entries[0]!.summary = t(key); report.entries[0]!.metrics = [];
    const html = renderToStaticMarkup(<FullText report={report} entry={report.entries[0]!} />);
    expect(html).toContain(t(key)); expect(html).not.toContain("<dd>0</dd>");
  }
});

test("the selected table cannot borrow another schema or a duplicate catalog explanation", () => {
  const report = fromRust<Report>(demo); const table = report.snapshot.tables[0]!;
  const catalog = details(); catalog.entries[0]!.schema = table.schema; catalog.entries[0]!.table = table.name;
  report.table_full_text = catalog;
  expect(renderToStaticMarkup(<Header report={report} table={table} findings={[]} />)).toContain(catalog.entries[0]!.summary);
  catalog.entries[0]!.schema = "unrelated_catalog_schema";
  expect(renderToStaticMarkup(<Header report={report} table={table} findings={[]} />)).not.toContain(catalog.entries[0]!.summary);
  catalog.entries[0]!.schema = table.schema; catalog.entries.push({ ...catalog.entries[0]! });
  expect(renderToStaticMarkup(<Header report={report} table={table} findings={[]} />)).not.toContain(catalog.entries[0]!.summary);
});
