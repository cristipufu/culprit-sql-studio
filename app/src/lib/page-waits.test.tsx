import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { PageWaitTargets } from "../components/PageWaitTargets";
import type { Report } from "../gen/Report";
import { partial } from "../test/fixtures";
import { PAGE_AFTER, PAGE_BEFORE, PAGE_FROM, PAGE_UNTIL, pageEvidence, pageReport, pageWait } from "../test/page-waits";
import { t } from "./i18n";
import { pageWaitReferenceKey } from "./page-wait-reference";

test("page references preserve session and request generations, transactions, raw resources and wait kinds without order-sensitive qualifiers", () => {
  const row = pageWait();
  const key = pageWaitReferenceKey(row.request.key, row.resource);
  const reordered = structuredClone(row.resource);
  reordered.native.qualifiers = { namespace: "native", source: "sys.dm_exec_requests.wait_resource" };
  expect(pageWaitReferenceKey(row.request.key, reordered)).toBe(key);
  const variants = Array.from({ length: 5 }, () => pageWait());
  variants[0]!.request.key.session.generation = "reused-session";
  variants[1]!.request.key.request!.generation = "replacement-request";
  variants[2]!.request.key.transaction = "another-transaction";
  variants[3]!.resource.native.id = "5:1:43";
  variants[4]!.resource.kind = "page-io";
  for (const value of variants) expect(pageWaitReferenceKey(value.request.key, value.resource)).not.toBe(key);
});

test("page metadata is joined to the original exact native references instead of array order or an invented proof hash", () => {
  const wanted = pageWait();
  const other = pageWait("43", 43);
  const view = pageReport([wanted]);
  view.entries[0]!.resource.native.qualifiers = { namespace: "native", source: "sys.dm_exec_requests.wait_resource" };
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([other, wanted])} report={view} />);
  expect(html).toContain("5:1:42");
  expect(html).not.toContain("5:1:43");
  expect(html).not.toContain("opaque-backend-page-reference");
  expect(html).toContain("orders_id");
});

test("duplicate, source-mismatched and replaced native references remain unavailable instead of borrowing another request", () => {
  const wanted = pageWait();
  const changed = pageWait();
  changed.request.key.request!.generation = "replacement";
  const wrongSource = pageWait();
  wrongSource.source = "another native source";
  for (const observations of [[changed], [wanted, pageWait()], [wrongSource]]) {
    const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence(observations)} report={pageReport()} />);
    expect(html).toContain(t("ui.memory_failures.missing_row"));
    expect(html).not.toContain("orders_id");
  }
});

test("page memory contention and page IO preserve different native meanings and original independent capture bounds", () => {
  const memory = pageWait();
  const io = pageWait("43", 43);
  io.resource.kind = "page-io";
  io.request.wait!.native_type = "PAGEIOLATCH_SH";
  io.request.wait!.identified_resources = [structuredClone(io.resource)];
  const rows = [memory, io];
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence(rows)} report={pageReport(rows)} />);
  expect(html).toContain("In-memory page contention");
  expect(html).toContain("Page I/O wait");
  expect(html).toContain("PAGELATCH_EX");
  expect(html).toContain("PAGEIOLATCH_SH");
  expect(html).toContain(PAGE_FROM);
  expect(html).toContain(PAGE_UNTIL);
  expect(html).toContain("2026-11-13T10:00:00.100000Z");
  expect(html).toContain("2026-11-13T10:00:00.900000Z");
  expect(html).not.toContain("sig-dot");
  expect(html).not.toContain("Open in Queries");
});

test("unavailable metadata retains the original native wait and historical SQL without inventing a target", () => {
  const row = pageWait();
  row.resolution = "permission-denied";
  row.target = null;
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={pageReport([row])} />);
  expect(html).toContain("5:1:42");
  expect(html).toContain("PAGELATCH_EX");
  expect(html).toContain("Originally observed statement context");
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("Observed page and object metadata");
  expect(html).toContain("native-statement-handle");
  expect(html).not.toContain("<script>");
});

test("a completed or different short-lived wait does not rewrite the original request or discard matching page metadata", () => {
  const row = pageWait();
  row.before_check!.wait = "different";
  row.after_check!.wait = "no-longer-observed";
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={pageReport([row])} />);
  expect(html).toContain(PAGE_BEFORE);
  expect(html).toContain(PAGE_AFTER);
  expect(html).toContain(t("ui.page_waits.recheck.wait.different"));
  expect(html).toContain(t("ui.page_waits.recheck.wait.no_longer"));
  expect(html).toContain("PAGELATCH_EX");
  expect(html).toContain("5:1:42");
  expect(html).toContain("orders_id");
  expect(html).toContain("native-statement-handle");
  expect(html).toContain("native-plan-handle");
  expect(html).not.toContain("Request/statement changed");
});

test("changed, missing or unverified request checks preserve historical wait and independent mapping but never expose unqualified handles", () => {
  for (const state of ["changed", "missing", "unverified"] as const) {
    const row = pageWait();
    row.after_check = { observed_at: PAGE_AFTER, request: state, wait: "unqualified" };
    const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={pageReport([row])} />);
    expect(html).toContain("orders_id");
    expect(html).toContain("PAGELATCH_EX");
    expect(html).toContain("&lt;script&gt;");
    expect(html).not.toContain("native-statement-handle");
    expect(html).not.toContain("native-plan-handle");
  }
  for (const missing of ["before_check", "after_check"] as const) {
    const row = pageWait();
    row[missing] = undefined;
    const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={pageReport([row])} />);
    expect(html).toContain(t("ui.page_waits.recheck.missing"));
    expect(html).toContain("orders_id");
    expect(html).not.toContain("native-statement-handle");
    expect(html).not.toContain("native-plan-handle");
  }
});

test("native counters and signed metadata IDs stay exact while SQL NULL and zero remain distinct", () => {
  const row = pageWait();
  row.target!.header.partition_id = "-9223372036854775808";
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={pageReport([row])} />);
  expect(html).toContain("18446744073709551615");
  expect(html).toContain("9007199254740993 ms");
  expect(html).toContain("-9223372036854775808");
  expect(html).toContain("9223372036854775807");
  expect(html).toContain("<dd>0 ms</dd>");
  expect(html).toContain("<dd>Not reported</dd>");
  expect(html).toContain("000001:000002:0003");
});

test("page observations start collapsed and remain bounded by the shared ten-row page", () => {
  const rows = Array.from({ length: 12 }, (_, index) => pageWait(String(42 + index), 42 + index));
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence(rows)} report={pageReport(rows)} />);
  expect(html).toContain("<details><summary>Inspect retained page waits</summary>");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html.match(/class="operational-card"/g)).toHaveLength(10);
  expect(html).toContain("Observations 1–10 of 12");
  expect(html).not.toContain("Affected request 10");
});

test("a requested page profile with missing source coverage stays visible in Health without a fake empty result", () => {
  const report = partial<Report>({ snapshot: {
    server: { engine: "mssql" }, sources: [], options: { collect_page_wait_targets: true },
    operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null },
  } });
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(html).toContain(t("view.page_waits.title"));
  expect(html).toContain(t("ui.observation_list.source_unknown"));
  expect(html).not.toContain(t("ui.health.empty"));
});
