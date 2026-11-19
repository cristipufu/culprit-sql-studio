import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { PageWaitTargets } from "../components/PageWaitTargets";
import type { Report } from "../gen/Report";
import { partial } from "../test/fixtures";
import { PAGE_AFTER, PAGE_BEFORE, PAGE_EARLIER, PAGE_FROM, PAGE_UNTIL, pageEvidence, pageReport, pageTaskWait, pageWait, recordedPageIncident, recordedPageRequest } from "../test/page-waits";
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

test("parallel task references distinguish workers on one request and page without borrowing coordinator waits", () => {
  const first = pageTaskWait(1);
  const second = pageTaskWait(2);
  const view = pageReport([first, second]);
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([second, first])} report={view} />);
  expect(html).not.toContain(t("ui.memory_failures.missing_row"));
  expect(html.match(/class="operational-card"/g)).toHaveLength(2);
  expect(html).toContain("0000000000000001");
  expect(html).toContain("0000000000000002");
  expect(html).toContain("PAGELATCH_EX");
  expect(html).toContain("CXCONSUMER");
  expect(html).toContain("9007199254740993 ms");
  expect(html).not.toContain("777 ms");
  expect(html).toContain(t("ui.page_waits.task.qualification"));
  expect(html).toContain(t("ui.memory_participants.observed_at_label"));
  expect(html).toContain("<dd>-5</dd>");
  expect(html).not.toContain("Open in Queries");
});

test("missing task duration does not borrow coordinator time and replaced workers cannot resolve an original reference", () => {
  const row = pageTaskWait();
  row.task_wait!.wait_duration_ms = null;
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={pageReport([row])} />);
  expect(html).not.toContain("777 ms");
  expect(html).toContain("Not reported");
  const replacement = pageTaskWait(2);
  const unavailable = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([replacement])} report={pageReport([row])} />);
  expect(unavailable).toContain(t("ui.memory_failures.missing_row"));
  expect(unavailable).not.toContain("orders_id");
});

test("ended task waits retain native evidence while withholding unqualified SQL and plan associations", () => {
  const row = pageTaskWait();
  row.after_check!.wait = "no-longer-observed";
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={pageReport([row])} />);
  expect(html).toContain("PAGELATCH_EX");
  expect(html).toContain("orders_id");
  expect(html).not.toContain("native-statement-handle");
  expect(html).not.toContain("native-plan-handle");
  expect(html).not.toContain("&lt;script&gt;");
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

test("the local tempdb primary-file basis shows native file ID 1 without inventing a file GUID or LSN", () => {
  const row = pageWait();
  row.resource.native.id = "2:1:1";
  row.request.wait!.identified_resources = [structuredClone(row.resource)];
  row.observed_address = { database_id: 2, file_id: 1, page_id: 1 };
  const target = row.target!;
  const shared = {
    engine: "mssql" as const, kind: "instance" as const,
    namespace: { id: "native-machine", qualifiers: {} }, native: { id: "native-instance", qualifiers: {} },
  };
  target.scope = { kind: "shared-resource", context: {
    target: { scope: "shared", identity: shared },
    partition: { kind: "instance-local", member: shared, epoch: "native-process-epoch" },
    observed_from: PAGE_BEFORE, observed_until: PAGE_AFTER, database_writable: null,
  } };
  target.identity_basis = "local-tempdb-primary-file-epoch";
  target.database = { id: "2", qualifiers: {
    kind: "sql-server-shared-tempdb", instance_epoch: "native-process-epoch",
    database_created_without_offset: "2026-11-13T09:00:00", database_guid: "native-tempdb-guid", recovery_fork_guid: "native-tempdb-fork",
  } };
  target.file = { id: "1", qualifiers: { kind: "sql-server-tempdb-primary-file", file_type: "0", file_state: "0" } };
  target.header.database_id = 2;
  target.header.page_id = 1;
  target.header.page_type = 11;
  target.header.object_id = null;
  target.header.index_id = null;
  target.header.partition_id = null;
  target.header.lsn = null;
  target.header.next_file_id = null;
  target.header.next_page_id = null;
  target.kind = "pfs";
  target.object = null;
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={pageReport([row])} />);
  expect(html).toContain(t("ui.page_waits.basis.tempdb_primary"));
  expect(html).toContain(`${t("ui.page_waits.file_identity")}</dt><dd><code>1</code>`);
  expect(html).toContain(`${t("ui.page_waits.header.lsn")}</dt><dd>${t("ui.memory_failures.not_reported")}`);
  expect(html).not.toContain("native-file-epoch");
  expect(html).not.toContain("create_lsn");
  expect(html).not.toContain("000001:000002:0003");
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

test("an empty current page read preserves the original recorded wait without inventing current metadata", () => {
  const view = pageReport([]);
  view.source_state = "empty";
  view.recorded_incident = recordedPageIncident();
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([])} report={view} />);
  expect(html).toContain(t("view.page_waits.recorded.title"));
  expect(html).toContain(t("view.page_waits.recorded.qualification"));
  expect(html).toContain(PAGE_FROM);
  expect(html).toContain("5:1:42");
  expect(html).toContain("observed-label");
  expect(html).toContain("&lt;script&gt;canary&lt;/script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("orders_id");
  expect(html).not.toContain("private-original");
  expect(html).not.toContain("a".repeat(64));
  expect(html).not.toContain(t("ui.page_waits.inspect"));
});

test("every recorded statement unavailability is explicit instead of borrowing a current statement", () => {
  for (const reason of ["missing", "restricted", "encrypted", "oversized", "redaction-failed", "not-collected", "source-unavailable"] as const) {
    const view = pageReport([]);
    view.recorded_incident = recordedPageIncident();
    view.recorded_incident.sql = { state: "unavailable", reason };
    const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence()} report={view} />);
    expect(html).toContain(t("ui.statement_grants.unavailable", { reason }));
    expect(html).not.toContain("SELECT");
    expect(html).not.toContain("<pre");
  }
});

test("recorded statement previews bound Unicode and leave the complete statement out of the initial DOM", () => {
  const view = pageReport([]);
  view.recorded_incident = recordedPageIncident();
  view.recorded_incident.sql = { state: "available", text: `SELECT ${"😀".repeat(600)} FROM hidden_original_tail`, sha256: "c".repeat(64), redaction_version: 1 };
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([])} report={view} />);
  expect(html).toContain(`SELECT ${"😀".repeat(505)}…`);
  expect(html).not.toContain("😀".repeat(506));
  expect(html).not.toContain("hidden_original_tail");
  expect(html).toContain(t("ui.page_waits.recorded.full_statement"));
  expect(html.match(/<pre\b/g)).toHaveLength(1);
});

test("a report without an authenticated original does not render recorded context", () => {
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([])} report={pageReport([])} />);
  expect(html).not.toContain(t("view.page_waits.recorded.title"));
  expect(html).not.toContain("observed-label");
  expect(html).not.toContain("<pre");
});

test("task waits keep earlier request SQL and time separate from changed current tokens and the later resource", () => {
  const row = pageTaskWait();
  const earlier = recordedPageRequest(row);
  earlier.sql = { state: "available", text: "SELECT earlier_request FROM dbo.orders WHERE tag=N'<script>canary</script>'", sha256: "c".repeat(64), redaction_version: 1 };
  earlier.client!.application = "earlier-application";
  row.request.statement = { state: "available", text: "private_current_statement", sha256: "d".repeat(64), redaction_version: 1 };
  row.after_check!.request = "changed";
  row.after_check!.wait = "unqualified";
  const report = pageReport([row]);
  report.entries[0]!.recorded_request = earlier;
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={report} />);
  expect(html).toContain(t("view.page_waits.recorded_request.title"));
  expect(html).toContain(t("view.page_waits.recorded_request.qualification"));
  expect(html).toContain(PAGE_EARLIER);
  expect(html).toContain(PAGE_FROM);
  expect(html).toContain("earlier-application");
  expect(html).toContain("earlier_request");
  expect(html).toContain("&lt;script&gt;canary&lt;/script&gt;");
  expect(html).toContain("5:1:42");
  expect(html).toContain("0000000000000001");
  expect(html).not.toContain("private_current_statement");
  expect(html).not.toContain("native-statement-handle");
  expect(html).not.toContain("native-plan-handle");
  expect(html).not.toContain("private-original");
  expect(html).not.toContain("a".repeat(64));
  expect(html).not.toContain("<script>");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
});

test("unavailable earlier task SQL never borrows the unqualified later statement", () => {
  for (const reason of ["missing", "restricted", "encrypted", "oversized", "redaction-failed", "not-collected", "source-unavailable"] as const) {
    const row = pageTaskWait();
    row.after_check!.request = "missing";
    row.after_check!.wait = "unqualified";
    row.request.statement = { state: "available", text: "SELECT private_current_sql_canary", sha256: "d".repeat(64), redaction_version: 1 };
    const report = pageReport([row]);
    report.entries[0]!.recorded_request = recordedPageRequest(row);
    report.entries[0]!.recorded_request!.sql = { state: "unavailable", reason };
    const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={report} />);
    expect(html).toContain(t("ui.statement_grants.unavailable", { reason }));
    expect(html).not.toContain("<pre");
    expect(html).not.toContain("private_current_sql_canary");
  }
});

test("earlier task statements use the same bounded lazy preview without promoting a recorded page incident", () => {
  const row = pageTaskWait();
  row.after_check!.request = "changed";
  row.after_check!.wait = "unqualified";
  const report = pageReport([row]);
  const earlier = recordedPageRequest(row);
  earlier.sql = { state: "available", text: "SELECT " + "😀".repeat(600) + " FROM hidden_task_tail", sha256: "c".repeat(64), redaction_version: 1 };
  report.entries[0]!.recorded_request = earlier;
  const html = renderToStaticMarkup(<PageWaitTargets evidence={pageEvidence([row])} report={report} />);
  expect(html).toContain("SELECT " + "😀".repeat(505) + "…");
  expect(html).not.toContain("😀".repeat(506));
  expect(html).not.toContain("hidden_task_tail");
  expect(html.match(/<pre\b/g)).toHaveLength(1);
  expect(html).not.toContain(t("view.page_waits.recorded.title"));
  expect(html).toContain(t("view.page_waits.recorded_request.title"));
});
