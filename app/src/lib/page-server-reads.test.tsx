import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { PageServerReads } from "../components/PageServerReads";
import { EXACT_PAGE_SERVER_PAGES, pageServerReadHealthReport, pageServerReadReport, unavailablePageServerReads } from "../test/page-server-reads";
import { t } from "./i18n";

test("page-server evidence folds and pages DTO entries with original counts and no current-pressure grading", () => {
  const report = pageServerReadReport();
  const html = renderToStaticMarkup(<PageServerReads report={report} />);
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain(report.summary);
  expect(html).toContain(EXACT_PAGE_SERVER_PAGES);
  expect(html).not.toContain("9007199254740992");
  expect(html).toContain(report.entries[9]!.explanation.title);
  expect(html).not.toContain(report.entries[10]!.explanation.title);
  expect(html).not.toContain("sig-dot");
  expect(html).not.toContain("href=");
});

test("a missing metric is unreported even when its generic numeric fallback is zero", () => {
  const report = pageServerReadReport(1);
  report.entries[0]!.explanation.metrics[0]!.text = null;
  const html = renderToStaticMarkup(<PageServerReads report={report} />);
  expect(html).toContain(`<dt>${t("view.page_server_reads.metric.last")}</dt><dd>${t("ui.memory_failures.not_reported")}</dd>`);
  expect(html).toContain(`<dt>${t("view.page_server_reads.metric.minimum")}</dt><dd>0</dd>`);
  expect(html).toContain(t("view.page_server_reads.unknown"));
});

test("not-applicable and unknown availability remain distinct from unavailable columns and measured reads", () => {
  for (const [availability, key] of [
    ["not-applicable", "view.page_server_reads.not_applicable"],
    ["unknown", "view.page_server_reads.profile_unknown"],
  ] as const) {
    const report = pageServerReadReport(1);
    report.availability = availability;
    report.entries[0]!.explanation.availability = availability;
    report.entries[0]!.explanation.summary = t(key);
    report.entries[0]!.explanation.metrics = [];
    const html = renderToStaticMarkup(<PageServerReads report={report} />);
    expect(html).toContain(t(key));
    expect(html).not.toContain(t("view.page_server_reads.observed"));
    expect(html).not.toContain(t("view.page_server_reads.metric.last"));
    expect(html).toContain(t("ui.common.read_partial"));
  }
});

test("Health renders admitted page-server evidence without an engine-name condition or a healthy empty fallback", () => {
  for (const view of [pageServerReadReport(1), unavailablePageServerReads(), null]) {
    const data = pageServerReadHealthReport(view);
    const html = renderToStaticMarkup(<HealthTab report={data} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
    expect(html).toContain(t("view.page_server_reads.title"));
    expect(html).not.toContain(t("ui.health.empty"));
    if (!view) expect(html).toContain(t("view.page_server_reads.invalid"));
    if (view?.availability === "unavailable") {
      expect(html).toContain(t("view.page_server_reads.unavailable"));
      expect(html).not.toContain("<article");
    }
  }
});

test("a bounded backend detail selection keeps its observed count and presentation qualification", () => {
  const report = pageServerReadReport(256);
  report.observed_rows = 257;
  report.presentation_limited = true;
  report.summary = t("view.page_server_reads.summary.archive", { observed: "257", selected: "259" });
  const html = renderToStaticMarkup(<PageServerReads report={report} />);
  expect(html).toContain(t("view.page_server_reads.limit.presentation", { displayed: "256" }));
  expect(html).toContain(report.summary);
  expect(html.match(/<article/g)).toHaveLength(10);
});

test("unrelated Query Store source failures do not pretend that page-server evidence was requested", () => {
  const data = pageServerReadHealthReport(null);
  const source = data.snapshot.sources?.[0];
  if (!source) throw new Error("The fixture requires its original Query Store source");
  source.reason_message = { kind: "catalog", key: "view.compilation.limit.source", arguments: {} };
  const html = renderToStaticMarkup(<HealthTab report={data} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).not.toContain(t("view.page_server_reads.title"));
});

test("historical identifiers and metric text render as text without executable markup or definition links", () => {
  const report = pageServerReadReport(1);
  const hostile = '<img src=x onerror="page_canary()"><script>page_canary()</script>https://fixture.invalid';
  report.entries[0]!.explanation.title = hostile;
  report.entries[0]!.explanation.metrics[0]!.text = hostile;
  const identity = report.entries[0]!.identity;
  if (identity.kind !== "recorded-plan") throw new Error("presentation identity");
  identity.plan_label = hostile;
  const html = renderToStaticMarkup(<PageServerReads report={report} />);
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("<script");
  expect(html).not.toContain("href=");
  expect(html).toContain(t("view.page_server_reads.limit.definition_link"));
});
