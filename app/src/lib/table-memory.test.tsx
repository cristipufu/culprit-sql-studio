import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { TableMemory } from "../components/TableMemory";
import { tableMemoryHealthReport, tableMemoryReport } from "../test/table-memory";
import { contextPreview } from "./context-preview";
import { t } from "./i18n";

test("independent table captures are folded and paged with exact gauges and native text only", () => {
  const reports = Array.from({ length: 12 }, (_, index) => tableMemoryReport(index));
  reports[0]!.original!.metadata.schema = "<script>native()</script>";
  reports[0]!.original!.metadata.name = `<img src=x onerror=alert(1)>${"界".repeat(800)}`;
  const html = renderToStaticMarkup(<TableMemory reports={reports} />);
  expect(html.match(/<article/g)).toHaveLength(10); expect(html).not.toContain("<details open");
  expect(html).toContain("9007199254740993"); expect(html).toContain(">0<"); expect(html).toContain(t("view.table_memory.unknown"));
  expect(html).toContain(t("view.table_memory.limit.historical")); expect(html).toContain("2020-01-01T00:00:00Z");
  expect(html).toContain("&lt;script&gt;"); expect(html).toContain("&lt;img"); expect(html).not.toContain("<script>"); expect(html).not.toContain("<img");
  expect(html).toContain(contextPreview(reports[0]!.original!.metadata.name).text.replaceAll("<", "&lt;").replaceAll(">", "&gt;"));
  expect(html).not.toContain("original-table-10"); expect(html).not.toContain("original-table-11"); expect(html).not.toContain("sig-dot");
  for (const privateValue of ["private-original-host", "private-machine", "private-member", "private-process-and-database"]) expect(html).not.toContain(privateValue);
});

test("Health exposes historical selected-table memory without a current-table association or empty-state health", () => {
  const data = tableMemoryHealthReport([tableMemoryReport()]);
  const html = renderToStaticMarkup(<HealthTab report={data} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(t("view.table_memory.title")); expect(html).toContain("original-table-0"); expect(html).toContain("9007199254740993");
  expect(html).toContain(t("view.table_memory.saved_summary")); expect(html).not.toContain(t("ui.health.empty"));
});

test("unavailable and withdrawn originals remain separate and never acquire another capture's native identity or zero", () => {
  const unavailable = tableMemoryReport(); unavailable.original = null; unavailable.native_rows = null;
  unavailable.observed_rows = 0; unavailable.read_complete = false; unavailable.metrics = [];
  unavailable.source.state = "unavailable"; unavailable.summary = t("view.table_memory.sharing_withheld");
  unavailable.source.reason = unavailable.summary;
  const html = renderToStaticMarkup(<TableMemory reports={[unavailable]} />);
  expect(html).toContain(unavailable.summary); expect(html).toContain(t("ui.dashboard.status.unavailable"));
  expect(html).not.toContain("9007199254740993"); expect(html).not.toContain(">0<"); expect(html).not.toContain("original-table-0");
  const independent = renderToStaticMarkup(<TableMemory reports={[unavailable, tableMemoryReport(1)]} />);
  expect(independent).toContain(unavailable.summary); expect(independent).toContain("original-table-1"); expect(independent).toContain("9007199254740993");
});

test("an empty selected-table collection does not suppress Health's unknown empty state", () => {
  const html = renderToStaticMarkup(<HealthTab report={tableMemoryHealthReport([])} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(html).toContain(t("ui.health.empty")); expect(html).not.toContain(t("view.table_memory.title"));
});

test("a complete known source has one qualification disclosure without inventing unknown collection state", () => {
  const report = tableMemoryReport();
  report.source.state = "collected";
  report.source.reason = null;
  const html = renderToStaticMarkup(<TableMemory reports={[report]} />);
  expect(html).not.toContain(t("ui.observation_list.source_unknown"));
  expect(html.match(/<h3>/g)).toHaveLength(1);
  expect(html).toContain(t("view.table_memory.limit.point"));
});
