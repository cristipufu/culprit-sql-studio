import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryCompilation } from "../components/QueryCompilation";
import { HealthTab } from "../components/Health";
import type { Report } from "../gen/Report";
import { compilationComparisonReport, compilationEvidence, compilationObservation, compilationReport, unavailableCompilationComparisonReport } from "../test/compilation";
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


test("native comparisons keep exact supplied counts and clocks, folded limits, and no duplicated baseline payload", () => {
  const rows = [compilationObservation()];
  const report = compilationComparisonReport(rows);
  const html = renderToStaticMarkup(<QueryCompilation evidence={compilationEvidence(rows)} report={report} />);
  expect(html).toContain("Recorded compilation count increase: 18446744073709551615.");
  expect(html).toContain("2026-11-13T09:59:00.000000001Z");
  expect(html).toContain("2026-11-13T10:00:01.000000002Z");
  expect(html).toContain(report.comparison?.summary ?? "missing comparison");
  expect(html).toContain("A missing entry supplies no zero or recovery claim.");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  for (const privateField of ["original-private-report-reference", "private-native-incarnation", "private-original-native-host"]) {
    expect(html).not.toContain(privateField);
  }
});

test("a changed native entry or missing raw observation cannot lend an interval explanation by numeric query ID", () => {
  for (const rows of [[compilationObservation()], []]) {
    const report = compilationComparisonReport();
    const change = report.comparison?.entries[0];
    if (!change) throw new Error("Comparison fixture required");
    change.identity.qualifiers.context_settings_id = "another-native-context";
    const html = renderToStaticMarkup(<QueryCompilation evidence={compilationEvidence(rows)} report={report} />);
    expect(html).not.toContain(change.explanation);
    expect(html).not.toContain(change.investigation);
    if (rows.length === 0) expect(html).toContain(t("ui.memory_failures.missing_row"));
    else expect(html).toContain(report.entries[0]?.explanation ?? "missing entry");
  }
});

test("an unavailable comparison remains explicit without inventing a counter row or observation interval", () => {
  const report = unavailableCompilationComparisonReport([]);
  if (!report.comparison) throw new Error("Comparison fixture required");
  const html = renderToStaticMarkup(<QueryCompilation evidence={compilationEvidence([])} report={report} />);
  expect(html).toContain(report.comparison.summary);
  expect(html).toContain(report.comparison.limitations[0] ?? "missing limitation");
  expect(html).not.toContain("18446744073709551615");
  expect(html).not.toContain("2026-11-13T09:59:00.000000001Z");
});
