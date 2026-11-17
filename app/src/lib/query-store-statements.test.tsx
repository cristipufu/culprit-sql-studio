import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { PageServerReads } from "../components/PageServerReads";
import { QueryCompilation } from "../components/QueryCompilation";
import { QueryStoreStatement } from "../components/QueryStoreStatement";
import { compilationEvidence, compilationObservation, compilationReport } from "../test/compilation";
import { pageServerStatementReport, queryStoreStatement } from "../test/query-store-statements";
import { t } from "./i18n";

test("the shared statement disclosure is folded, historical, text-only and explicit about shortening", () => {
  const statement = queryStoreStatement();
  statement.preview = '<script>statement_canary()</script><img src=x onerror="statement_canary()">';
  statement.preview_truncated = true;
  const html = renderToStaticMarkup(<QueryStoreStatement statement={statement} />);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("<script");
  expect(html).not.toContain("<img");
  expect(html).toContain(t("view.query_store_statement.shortened"));
  expect(html).toContain(t("ui.query_store_statement.reference", { reference: "opaque-original-definition" }));
  expect(html).not.toMatch(/<(?:a|button|input|textarea)(?:\s|>)/);
});

test("runtime SQL and an independent partial definition selection cannot hide original native counts or gaps", () => {
  const report = pageServerStatementReport();
  const html = renderToStaticMarkup(<PageServerReads report={report} />);
  expect(html).toContain("9007199254740993");
  expect(html).toContain(report.entries[0]!.statement!.explanation);
  expect(html).toContain(t("view.page_server_reads.statement.limit.partial"));
  expect(html).not.toContain(t("view.page_server_reads.limit.definition_link"));
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
});

test("unknown or unavailable SQL uses supplied Rust wording and has no numeric or statement placeholder", () => {
  for (const binding of ["identity-unavailable", "source-unverified", "source-changed", "not-observed"] as const) {
    const statement = queryStoreStatement();
    statement.binding = binding;
    statement.preview = null;
    statement.definition = null;
    statement.origin = "unknown";
    statement.unavailable = "not-collected";
    statement.unavailable_explanation = t("view.query_store_statement.unavailable", { reason: t("engine.cached_plans.reason.not_collected") });
    statement.explanation = t("view.page_server_reads.statement.identity_unavailable");
    const html = renderToStaticMarkup(<QueryStoreStatement statement={statement} />);
    expect(html).toContain(statement.explanation);
    expect(html).toContain(statement.unavailable_explanation);
    expect(html).not.toContain("<pre");
    expect(html).not.toContain("<code");
    expect(html).not.toContain("opaque-original-definition");
  }
});

test("compilation reuses the same disclosure only beside the exact native observation", () => {
  const row = compilationObservation();
  const report = compilationReport([row]);
  report.entries[0]!.statement = queryStoreStatement();
  const html = renderToStaticMarkup(<QueryCompilation evidence={compilationEvidence([row])} report={report} />);
  expect(html).toContain(report.entries[0]!.explanation);
  expect(html).toContain(report.entries[0]!.statement!.preview!);
  expect(html).toContain("9007199254740993");
  const changed = compilationObservation();
  if (changed.fact.kind !== "mssql-query-compilation") throw new Error("Compilation fixture required");
  changed.fact.data.identity.qualifiers.context_settings_id = "different-original-context";
  const mismatch = renderToStaticMarkup(<QueryCompilation evidence={compilationEvidence([changed])} report={report} />);
  expect(mismatch).toContain(t("ui.memory_failures.missing_row"));
  expect(mismatch).not.toContain(report.entries[0]!.statement!.preview!);
  expect(mismatch).not.toContain(report.entries[0]!.statement!.explanation);
});
