import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { StatementWork } from "../components/StatementWork";
import { HealthTab } from "../components/Health";
import type { Report } from "../gen/Report";
import { statementEvidence, statementPoint, statementReport } from "../test/statement-work";
import { t } from "./i18n";
import { healthReportFixture } from "../test/health-report";

test("statement points stay folded and paged with exact backend counts and complete native identities", () => {
  const rows = Array.from({ length: 12 }, (_, index) => statementPoint(index));
  const html = renderToStaticMarkup(<StatementWork evidence={statementEvidence([...rows].reverse())} report={statementReport(rows)} />);
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain("9007199254740993");
  expect(html).not.toContain("9,007,199,254,740,993");
  expect(html).toContain("2020-01-01T12:00:00.123456Z");
  expect(html).toContain("schema-9");
  expect(html).not.toContain("schema-11");
  expect(html).toContain("Warning counts do not count statements with warnings.");
  expect(html).not.toContain("sig-dot");
});

test("a digest cannot borrow another schema, clock, reset, source or repeated row", () => {
  const original = statementPoint();
  const schema = statementPoint();
  const clock = statementPoint();
  const reset = statementPoint();
  const source = statementPoint();
  for (const row of [schema, clock, reset]) {
    if (row.fact.kind !== "statement-counters") throw new Error("fixture");
    if (row === schema) row.fact.data.identity.qualifiers.schema = "another-schema";
    if (row === clock) row.fact.data.observed_at = "2020-01-01T12:00:00.999999Z";
    if (row === reset) row.fact.data.reset = { kind: "generation", value: "another-epoch" };
  }
  source.source = "another-native-source";
  for (const rows of [[schema], [clock], [reset], [source], [original, original]]) {
    const html = renderToStaticMarkup(<StatementWork evidence={statementEvidence(rows)} report={statementReport()} />);
    expect(html).toContain(t("ui.memory_failures.missing_row"));
    expect(html).not.toContain("9007199254740993");
  }
  const report = statementReport();
  report.entries[0]!.source = "another-native-source";
  expect(renderToStaticMarkup(<StatementWork evidence={statementEvidence()} report={report} />)).toContain(t("ui.memory_failures.missing_row"));
});

test("an unreported outcome remains unknown while a measured zero stays visible", () => {
  const report = statementReport();
  report.entries[0]!.outcomes.errors = null;
  report.entries[0]!.outcome_metrics.splice(0, 1);
  const html = renderToStaticMarkup(<StatementWork evidence={statementEvidence()} report={report} />);
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain("<dd>0</dd>");
  expect(html).not.toContain("9007199254740993");
});

test("native schema labels render as text without links or HTML", () => {
  const row = statementPoint();
  if (row.fact.kind !== "statement-counters") throw new Error("fixture");
  row.fact.data.identity.qualifiers.schema = '<script>alert("private")</script>https://fixture.invalid';
  const html = renderToStaticMarkup(<StatementWork evidence={statementEvidence([row])} report={statementReport([row])} />);
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("href=");
});

test("a failed source stays visible without a native context or a fabricated empty point", () => {
  const evidence = statementEvidence([]);
  evidence.context = null;
  evidence.coverage.state = "unavailable";
  const report = statementReport([]);
  report.source_state = "unavailable";
  report.source_reason = "The source could not be read.";
  const html = renderToStaticMarkup(<StatementWork evidence={evidence} report={report} />);
  expect(html).toContain("The source could not be read.");
  expect(html).not.toContain("<article");
  expect(html).not.toContain("9007199254740993");
});

function health(report: Report): string {
  return renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="MySQL" />);
}

function healthReport(): Report {
  return healthReportFixture({ snapshot: { id: "counter-snapshot", server: { engine: "mysql" }, sources: [],
    operations: { statement_work: statementEvidence(), spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    statement_work: statementReport(),
  });
}

test("Health exposes admitted native outcomes even with no other signals or pressure", () => {
  const html = health(healthReport());
  expect(html).toContain("Recorded statement work");
  expect(html).toContain("9007199254740993");
  expect(html).not.toContain(t("ui.health.empty"));
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
});

test("Health never hides retained native counters when their explanation was withheld", () => {
  for (const state of ["collected", "unavailable"] as const) {
    const report = healthReport();
    delete report.statement_work;
    report.snapshot.operations.statement_work!.coverage.state = state;
    if (state === "unavailable") {
      report.snapshot.operations.statement_work!.context = null;
      report.snapshot.operations.statement_work!.observations = [];
      report.snapshot.operations.statement_work!.coverage.records = 0;
      report.snapshot.operations.statement_work!.coverage.error_code = "permission_denied";
    }
    const html = health(report);
    expect(html).toContain(t("view.statement_work.title"));
    expect(html).toContain(t("view.statement_work.invalid"));
    expect(html).not.toContain(t("ui.health.empty"));
    expect(html).not.toContain("9007199254740993");
  }
});

test("Health preserves an unavailable native report without fabricating healthy empty counts", () => {
  const report = healthReport();
  report.snapshot.operations.statement_work = statementEvidence([]);
  report.snapshot.operations.statement_work.context = null;
  report.snapshot.operations.statement_work.coverage.state = "unavailable";
  report.snapshot.operations.statement_work.coverage.error_code = "permission_denied";
  report.statement_work = statementReport([]);
  report.statement_work.source_state = "unavailable";
  report.statement_work.source_reason = "The native counters were not readable.";
  const html = health(report);
  expect(html).toContain("The native counters were not readable.");
  expect(html).not.toContain(t("ui.health.empty"));
  expect(html).not.toContain("9007199254740993");
});
