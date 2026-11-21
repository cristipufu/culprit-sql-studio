import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { TaskSpace } from "../components/TaskSpace";
import { HealthTab } from "../components/Health";
import { taskSpace, taskSpaceEvidence, taskSpaceReport } from "../test/task-space";
import { t } from "./i18n";
import { healthReportFixture } from "../test/health-report";

test("task rows join exact capture-local tuples despite native order and preserve exact counts, zero and unknown", () => {
  const wanted = taskSpace();
  const other = taskSpace("1");
  other.user_allocated_pages = "12345";
  const html = renderToStaticMarkup(<TaskSpace evidence={taskSpaceEvidence([other, wanted])} report={taskSpaceReport([wanted])} />);
  expect(html).toContain("9223372036854775807");
  expect(html).toContain("9007199254740993");
  expect(html).toContain("<dd>0</dd>");
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).not.toContain("12345");
  expect(html).not.toContain("opaque-task-");
  expect(html).not.toContain("sig-dot");
});

test("missing, replaced, duplicated and wrong-source task references cannot borrow another task's measurements", () => {
  const replaced = taskSpace("1");
  const wrong = taskSpace();
  wrong.source = "another-source";
  for (const observations of [[replaced], [taskSpace(), taskSpace()], [wrong]]) {
    const html = renderToStaticMarkup(<TaskSpace evidence={taskSpaceEvidence(observations)} report={taskSpaceReport()} />);
    expect(html).toContain(t("ui.memory_failures.missing_row"));
    expect(html).not.toContain("9223372036854775807");
  }
});

test("request checks keep their independent UTC clocks and cannot expose stale SQL or handles", () => {
  for (const state of ["changed", "missing", "unverified"] as const) {
    const row = taskSpace();
    row.after_check!.state = state;
    const report = taskSpaceReport([row]);
    report.entries[0]!.current_request = null;
    const html = renderToStaticMarkup(<TaskSpace evidence={taskSpaceEvidence([row])} report={report} />);
    expect(html).toContain("2026-11-13T10:00:00.200000Z");
    expect(html).toContain("2026-11-13T10:00:00.300000Z");
    expect(html).toContain("2026-11-13T10:00:00.700000Z");
    expect(html).not.toContain("task-native-statement");
    expect(html).not.toContain("task-native-plan");
    expect(html).not.toContain("<pre");
  }
  const html = renderToStaticMarkup(<TaskSpace evidence={taskSpaceEvidence()} report={taskSpaceReport()} />);
  expect(html).toContain("task-native-statement");
  expect(html).toContain("task-native-plan");
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("<a ");
});

test("task observations stay collapsed and paged rather than filling Health with native details", () => {
  const rows = Array.from({ length: 12 }, (_, index) => taskSpace(String(index)));
  const html = renderToStaticMarkup(<TaskSpace evidence={taskSpaceEvidence(rows)} report={taskSpaceReport(rows)} />);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html.match(/class="operational-card"/g)).toHaveLength(10);
  expect(html).toContain("Observations 1–10 of 12");
  expect(html).not.toContain("Native task 10");
});

test("requested missing or unsupported task profiles stay visible without an invented healthy empty result", () => {
  for (const sources of [[], [{ source: "task space", state: "unavailable" as const, reason: "This native source is unsupported here." }]]) {
    const report = healthReportFixture({ snapshot: { server: { engine: "mssql" }, sources, options: { collect_task_space: true }, operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } } });
    const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
    expect(html).toContain(t("view.task_space.title"));
    expect(html).not.toContain(t("ui.health.empty"));
    if (sources.length === 0) expect(html).toContain(t("ui.observation_list.source_unknown"));
    else expect(html).toContain("This native source is unsupported here.");
  }
});


test("current request SQL comes only from the canonical report child and keeps its own interval", () => {
  const row = taskSpace();
  const report = taskSpaceReport([row]);
  const context = report.entries[0]!.current_request!;
  context.statement = { state: "available", text: "SELECT canonical_current_context", sha256: "a".repeat(64), redaction_version: 1 };
  row.request!.database = "raw-current-database-marker";
  row.request!.statement = { state: "available", text: "private_raw_current_context", sha256: "b".repeat(64), redaction_version: 1 };
  const html = renderToStaticMarkup(<TaskSpace evidence={taskSpaceEvidence([row])} report={report} />);
  expect(html).toContain("canonical_current_context");
  expect(html).toContain("raw-current-database-marker");
  expect(html).not.toContain("private_raw_current_context");
  expect(html).toContain(t("view.task_space.current_request.interval", { from: context.observed_from, until: context.observed_until }));
  expect(html).toContain(t("view.task_space.current_request.qualification"));
  expect(html).toContain(row.observed_at);
  report.entries[0]!.current_request = null;
  row.after_check!.state = "changed";
  const withheld = renderToStaticMarkup(<TaskSpace evidence={taskSpaceEvidence([row])} report={report} />);
  expect(withheld).not.toContain("private_raw_current_context");
  expect(withheld).not.toContain("raw-current-database-marker");
  expect(withheld).toContain(`<code>${row.native_database_id}</code>`);
  expect(withheld).toContain("9223372036854775807");
  expect(withheld).toContain("9007199254740993");
  expect(withheld).not.toContain("task-native-statement");
  expect(withheld).not.toContain(t("view.task_space.current_request.title"));
});

test("unavailable current SQL keeps its recorded reason without falling back to raw SQL", () => {
  for (const reason of ["missing", "restricted", "encrypted", "oversized", "redaction-failed", "not-collected", "source-unavailable"] as const) {
    const row = taskSpace();
    const report = taskSpaceReport([row]);
    report.entries[0]!.current_request!.statement = { state: "unavailable", reason };
    const html = renderToStaticMarkup(<TaskSpace evidence={taskSpaceEvidence([row])} report={report} />);
    expect(html).toContain(t("ui.statement_grants.unavailable", { reason }));
    expect(html).not.toContain("<pre");
    expect(html).toContain(t("view.task_space.current_request.qualification"));
  }
});

test("current request previews stay collapsed, Unicode bounded and text only without hidden full SQL", () => {
  const row = taskSpace();
  const report = taskSpaceReport([row]);
  const context = report.entries[0]!.current_request!;
  context.statement = { state: "available", text: `<script>${"列".repeat(1000)}hidden-statement-tail`, sha256: "a".repeat(64), redaction_version: 1 };
  context.client = { application: `${"界".repeat(1000)}hidden-client-tail`, user: null, host: null };
  const html = renderToStaticMarkup(<TaskSpace evidence={taskSpaceEvidence([row])} report={report} />);
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("hidden-statement-tail");
  expect(html).not.toContain("hidden-client-tail");
  expect([...html].filter((character) => character === "列").length).toBeLessThanOrEqual(512);
  expect([...html].filter((character) => character === "界")).toHaveLength(512);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
});
