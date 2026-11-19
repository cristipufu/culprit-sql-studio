import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { DatabaseProperties, DatabasePropertyFields } from "../components/DatabaseProperties";
import { HealthTab } from "../components/Health";
import { databasePropertiesHealthReport, databasePropertiesReport } from "../test/database-properties";
import { t } from "./i18n";

test("properties fold native fields without mounting hidden rows", () => {
  const html = renderToStaticMarkup(<DatabaseProperties report={databasePropertiesReport()} />);
  expect(html).toContain(t("view.database_properties.title"));
  expect(html).not.toContain("<details open");
  expect(html).not.toContain("operational-card");
  expect(html).not.toContain("READ_ONLY");
  expect(html).not.toContain("2019-12-28T23:10:00.003");
});
test("native code, unknown, zero and independent catalog flags remain text-only display facts", () => {
  const report = databasePropertiesReport();
  report.fields[0]!.native_description = "<img src=x onerror=alert(1)>";
  const html = renderToStaticMarkup(<DatabasePropertyFields report={report} />);
  expect(html).toContain(">255<"); expect(html).toContain(">0<");
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain(t("view.database_properties.read_only"));
  expect(html).toContain(t("view.database_properties.no"));
  expect(html).not.toContain("<img"); expect(html).toContain("&lt;img");
  expect(html).not.toContain("sig-dot");
});
test("Health shows property-only partial evidence or an explicit missing explanation without an engine branch", () => {
  const data = databasePropertiesHealthReport();
  const render = () => renderToStaticMarkup(<HealthTab report={data} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(render()).toContain(t("view.database_properties.title"));
  data.database_properties = undefined;
  expect(render()).toContain(t("ui.common.read_partial"));
  data.snapshot.sources = [{ source: "database-properties", state: "collected", reason: null }];
  expect(render()).toContain(t("view.database_properties.invalid"));
  expect(render()).not.toContain(t("ui.health.empty"));
});

test("recorded check metadata, no recorded success and unknown remain informational display values", () => {
  for (const value of ["2019-12-28T23:10:00.003", t("view.database_properties.checkdb_not_recorded"), null]) {
    const report = databasePropertiesReport();
    const field = report.fields.find((field) => field.key === "last_good_checkdb_without_offset")!;
    field.value = value;
    const html = renderToStaticMarkup(<DatabasePropertyFields report={report} />);
    expect(html).toContain(t("view.database_properties.field.last_good_checkdb"));
    expect(html).toContain(value ?? t("ui.memory_failures.not_reported"));
    expect(html).not.toContain("sig-dot");
    expect(html).not.toContain("DBCC CHECKDB");
    expect(html).not.toContain("1900-01-01");
  }
});
