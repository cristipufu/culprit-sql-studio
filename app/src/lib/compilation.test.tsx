import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryCompilation } from "../components/QueryCompilation";
import { HealthTab } from "../components/Health";
import type { Report } from "../gen/Report";
import { compilationEvidence, compilationObservation, compilationReport } from "../test/compilation";
import { partial } from "../test/fixtures";
import { t } from "./i18n";

test("compilation records retain exact counts and native context inside collapsed, paged evidence", () => {
  const rows = Array.from({ length: 12 }, (_, index) => compilationObservation(index));
  const html = renderToStaticMarkup(<QueryCompilation evidence={compilationEvidence([...rows].reverse())} report={compilationReport(rows)} />);
  expect(html).toContain("9007199254740993");
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain("2026-11-13T10:00:01Z");
  expect(html).toContain("Some retained entries were not visible.");
  expect(html).not.toContain("Native query 13");
  expect(html).not.toContain("sig-dot");
});

test("reused numeric query IDs, duplicate rows and wrong sources cannot supply a compilation explanation", () => {
  const changed = compilationObservation();
  if (changed.fact.kind !== "mssql-query-compilation") throw new Error("fixture");
  changed.fact.data.identity.qualifiers.context_settings_id = "6";
  const wrongSource = compilationObservation();
  wrongSource.source = "unrelated source";
  for (const rows of [[changed], [compilationObservation(), compilationObservation()], [wrongSource]]) {
    const html = renderToStaticMarkup(<QueryCompilation evidence={compilationEvidence(rows)} report={compilationReport()} />);
    expect(html).toContain(t("ui.memory_failures.missing_row"));
    expect(html).not.toContain("9007199254740993");
  }
});

test("native context stays text and a failed read remains visible without an admitted report", () => {
  const row = compilationObservation();
  if (row.fact.kind !== "mssql-query-compilation") throw new Error("fixture");
  row.fact.data.identity.qualifiers.object_id = "<script>native-context</script>";
  const html = renderToStaticMarkup(<QueryCompilation evidence={compilationEvidence([row])} report={compilationReport([row])} />);
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("<script>");
  const report = partial<Report>({ snapshot: { server: { engine: "mssql" }, sources: [{ source: "query compilation", state: "unavailable", reason: "Native permission denied." }], operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } } });
  const failed = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(failed).toContain(t("view.compilation.title"));
  expect(failed).toContain("Native permission denied.");
  expect(failed).not.toContain(t("ui.health.empty"));
});
