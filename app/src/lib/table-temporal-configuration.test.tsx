import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { TableTemporalReport } from "../gen/TableTemporalReport";
import type { TableStat } from "../gen/TableStat";
import type { SourceState } from "../gen/SourceState";
import { partial } from "../test/fixtures";
import { TemporalConfiguration } from "../views/schema-table/TemporalConfiguration";
import { t } from "./i18n";
const table = partial<TableStat>({ schema: "dbo", name: "Original", mssql_temporal_configuration: partial<NonNullable<TableStat["mssql_temporal_configuration"]>>({ temporal_type: 2 }) });
function report(): TableTemporalReport {
  return { title: t("view.table_temporal.title"), summary: "", source_state: "collected", source_reason: null, observed_from: null, observed_until: null, native_rows: null, observed_rows: 1, read_complete: null, qualification: t("view.table_temporal.qualification"), limitations: [t("view.table_temporal.limit.history")], entries: [{ schema: "dbo", table: "Original", observation_key: null, fields: [{ key: "native", label: t("view.table_temporal.field.type"), text: "2", availability: "available" }] }] };
}
function render(value = report(), state: SourceState | null = "collected", selected = table) {
  return renderToStaticMarkup(<TemporalConfiguration engine="mssql" sourceState={state} table={selected} report={value} />);
}
test("canonical configuration is a folded table disclosure and retains its non-causal qualification", () => {
  const html = render(); expect(html).toContain("<details"); expect(html).not.toContain(" open="); expect(html).toContain("<code>2</code>"); expect(html).toContain(t("view.table_temporal.qualification")); expect(html).toContain(t("view.table_temporal.limit.history")); expect(html).not.toContain('class="sev');
});
test("unavailable source and a foreign or ambiguous table never borrow another table's report", () => {
  expect(render(report(), "unavailable")).toBe(""); expect(render(report(), "collected", { ...table, name: "Other" })).toBe("");
  const value = report(); value.entries.push(value.entries[0]!); expect(render(value)).toBe("");
  expect(renderToStaticMarkup(<TemporalConfiguration engine="postgres" sourceState="collected" table={table} report={report()} />)).toBe("");
});
test("native NULL and column refusal keep their canonical distinct text while source partiality stays visible", () => {
  const value = report(); value.entries[0]!.fields = [
    { key: "null", label: "Period", availability: "available", text: t("view.table_temporal.native_null") },
    { key: "denied", label: "Unit", availability: "permission-denied", text: t("view.table_temporal.denied") },
  ];
  value.source_state = "partial"; value.limitations.push(t("view.table_catalog.partial"));
  const html = render(value, "partial"); expect(html).toContain(t("view.table_temporal.native_null")); expect(html).toContain(t("view.table_temporal.denied")); expect(html).toContain(t("view.table_catalog.partial")); value.source_state = null; value.limitations.push(t("view.table_catalog.unknown_source")); expect(render(value, null)).toContain(t("view.table_catalog.unknown_source"));
});
test("canonical native text remains escaped bounded text with no control sequences or history link", () => {
  const value = report(); value.entries[0]!.fields[0]!.text = "<script>\n\u001b[31m" + "界".repeat(300);
  const html = render(value); expect(html).toContain("&lt;script&gt;"); expect(html).not.toContain("<script>"); expect(html).not.toContain("\u001b"); expect(html).not.toContain("界".repeat(161)); expect(html).not.toContain("<a ");
});
