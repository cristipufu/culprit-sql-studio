import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { CompletedSpills } from "../components/CompletedSpills";
import { HealthTab } from "../components/Health";
import type { Report } from "../gen/Report";
import { COMPLETED_SPILL_SOURCE, EXACT_SPILL_COUNT, SPILL_FROM, SPILL_UNTIL, completedSpillFixture, partialCompletedSpillFixture } from "../test/completed-spills";
import { partial } from "../test/fixtures";
import { t } from "./i18n";

function health(input: Partial<Report> = {}, sources: Report["snapshot"]["sources"] = []) {
  return partial<Report>({ ...input, snapshot: { ...input.snapshot, server: { engine: "mssql" }, sources,
    operations: { ...input.snapshot?.operations, spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } } });
}

function renderHealth(report: Report) {
  return renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
}

test("completed spills keep exact text counters and nanosecond bounds behind bounded folded records", () => {
  const fixture = completedSpillFixture();
  const html = renderToStaticMarkup(<CompletedSpills report={fixture.report} />);
  expect(html).toContain(EXACT_SPILL_COUNT);
  expect(html).toContain(SPILL_FROM);
  expect(html).toContain(SPILL_UNTIL);
  expect(html).toContain("2026-11-13T09:59:00.003");
  expect(html).not.toContain("2026-11-13T09:59:00.003Z");
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toContain("Completed statement 11");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  for (const privateValue of ["private-native-", "private-database-", "0xaabbccdd", "0xeeff0011"]) expect(html).not.toContain(privateValue);
  expect(html).not.toContain("sig-dot");
});

test("incomplete runtime and independent definition reads retain separate bounds and explanation gaps", () => {
  const fixture = partialCompletedSpillFixture();
  const html = renderToStaticMarkup(<CompletedSpills report={fixture.report} />);
  expect(html).toContain(t("ui.common.read_partial"));
  expect(html).toContain(t("ui.completed_spills.counts", { native: "3", observed: "1" }));
  expect(html).toContain("2026-11-13T10:01:00Z");
  expect(html).toContain("Some definition bodies were not read.");
  expect(html).toContain(t("view.completed_spills.binding.not_observed"));
  expect(html).toContain(t("ui.completed_spills.statement_unavailable"));
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
});

test("empty and unsupported native counters stay explicit without a healthy grade or measured zero", () => {
  const empty = completedSpillFixture(0).report;
  const html = renderToStaticMarkup(<CompletedSpills report={empty} />);
  expect(html).toContain(t("view.completed_spills.summary.missing"));
  expect(html).toContain(t("ui.completed_spills.counts", { native: "0", observed: "0" }));
  expect(html).not.toContain("<article");
  const fixture = completedSpillFixture(1);
  const entry = fixture.report.entries[0]!;
  entry.state = "unavailable";
  entry.title = t("view.completed_spills.entry.unavailable");
  entry.explanation = t("view.completed_spills.entry.unavailable_detail");
  entry.metrics = [];
  const unavailable = renderToStaticMarkup(<CompletedSpills report={fixture.report} />);
  expect(unavailable).toContain(entry.explanation);
  expect(unavailable).not.toContain(EXACT_SPILL_COUNT);
  expect(unavailable).not.toContain("sig-dot");
});

test("statement previews remain inert text and bounded display truncation stays explicit", () => {
  const fixture = completedSpillFixture(1);
  const entry = fixture.report.entries[0]!;
  entry.statement_preview = "SELECT '<script>spill-preview-canary</script>'";
  entry.statement_preview_truncated = true;
  const html = renderToStaticMarkup(<CompletedSpills report={fixture.report} />);
  expect(html).toContain("&lt;script&gt;spill-preview-canary&lt;/script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).toContain(t("ui.completed_spills.statement_truncated"));
  expect(html).not.toContain("query-detail");
});

test("captured positions render opaque row identifiers without dumping their native receipt", () => {
  const fixture = completedSpillFixture(1);
  const entry = fixture.report.entries[0]!;
  entry.observation = { kind: "captured", reference: {
    capture: { observation_id: "a".repeat(32), revision: { outbox_id: "b".repeat(32), owner_epoch: "1", sequence: "1" } },
    manifest_sha256: "c".repeat(64), source_scope_id: "d".repeat(64), stream: "operational-samples",
    native_key: { kind: "operational", data: { source: "private-captured-native-source", key: "e".repeat(64) } },
    batch_id: "f".repeat(64), record_id: "1".repeat(64), payload_sha256: "2".repeat(64), representation_version: 1, row_ordinal: 0,
  } };
  const html = renderToStaticMarkup(<CompletedSpills report={fixture.report} />);
  expect(html).toContain(t("ui.completed_spills.capture_reference"));
  expect(html).toContain("1".repeat(64));
  expect(html).not.toContain("private-captured-native-source");
  expect(html).not.toContain("manifest_sha256");
  expect(html).not.toContain("outbox_id");
});

test("failed acquisition, raw-only data and stale derived-only data cannot become an empty healthy view", () => {
  const fixture = completedSpillFixture(1);
  const gap = renderHealth(health({}, [{ source: COMPLETED_SPILL_SOURCE, state: "unavailable", reason: "Native source permission denied." }]));
  expect(gap).toContain(t("view.completed_spills.title"));
  expect(gap).toContain("Native source permission denied.");
  expect(gap).not.toContain(t("ui.health.empty"));
  const rawOnly = partial<Report>({ snapshot: { operations: { cached_runtime: fixture.read } } });
  for (const report of [health(rawOnly), health({ completed_spills: fixture.report })]) {
    const html = renderHealth(report);
    expect(html).toContain(t("view.completed_spills.invalid"));
    expect(html).not.toContain(EXACT_SPILL_COUNT);
    expect(html).not.toContain(t("ui.health.empty"));
  }
});
