import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { PageWaitReport } from "../src/gen/PageWaitReport";
import type { PageWaitSnapshot } from "../src/gen/PageWaitSnapshot";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import { PAGE_AFTER, PAGE_BEFORE, PAGE_FROM, PAGE_SOURCE, PAGE_UNTIL, pageEvidence, pageReport, pageTaskWait, pageWait, recordedPageIncident } from "../src/test/page-waits";
import { openConnection, waitForReport } from "./demo";

const TITLE = english["view.page_waits.title"];

function fixture(): { evidence: PageWaitSnapshot; report: PageWaitReport } {
  const observations = Array.from({ length: 12 }, (_, index) => pageWait(String(42 + index), 42 + index));
  observations[0]!.target!.header.partition_id = "-9223372036854775808";
  observations[0]!.after_check!.wait = "no-longer-observed";
  const unresolved = observations[1]!;
  unresolved.resource.kind = "page-io";
  unresolved.request.wait!.native_type = "PAGEIOLATCH_SH";
  unresolved.request.wait!.identified_resources = [structuredClone(unresolved.resource)];
  unresolved.resolution = "permission-denied";
  unresolved.target = null;
  const report = pageReport(observations);
  report.title = TITLE;
  // Native read order and qualifier insertion order are independent of the explanation's references.
  const evidence = pageEvidence([...observations].reverse());
  for (const entry of report.entries) {
    entry.resource.native.qualifiers = { namespace: "native", source: "sys.dm_exec_requests.wait_resource" };
  }
  return { evidence, report };
}

async function inject(page: Page, evidence: PageWaitSnapshot | null, view: PageWaitReport | null, source: SourceOutcome) {
  await waitForReport(page);
  await page.evaluate(async ({ path, evidence, view, source }) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A ready fixture report is required");
    const report = structuredClone(state.report.report);
    if (!report.snapshot.options) throw new Error("The fixture scan options must be present");
    report.snapshot.options.collect_page_wait_targets = true;
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.operations = {
      ...report.snapshot.operations, spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null,
      memory_failures: undefined, native_failures: undefined, termination_history: undefined, node_resources: undefined,
      session_memory: undefined, page_waits: evidence ?? undefined,
    };
    report.snapshot.sources = [source];
    report.memory_failures = undefined;
    report.native_failures = undefined;
    report.request_memory = undefined;
    report.session_memory = undefined;
    report.page_waits = view ?? undefined;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", evidence, view, source });
}

test("recorded waits survive an empty current read with bounded lazy SQL and a fresh disclosure per payload", async ({ page }, info) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await openConnection(page, "mssql-demo");
  const report = pageReport([]);
  report.title = TITLE;
  report.source_state = "empty";
  const original = recordedPageIncident();
  const sql = `SELECT ${"😀".repeat(600)} FROM recorded_tail WHERE label=N'<script>canary</script>'`;
  original.sql = { state: "available", text: sql, sha256: "c".repeat(64), redaction_version: 1 };
  report.recorded_incident = original;
  const source = { source: PAGE_SOURCE, state: "empty", reason: null } as const;
  await inject(page, pageEvidence([]), report, source);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  const recorded = region.locator(".recorded-page-wait");
  await expect(recorded).toBeVisible();
  await expect(recorded).toContainText(english["view.page_waits.recorded.qualification"]);
  await expect(recorded).toContainText("observed-label");
  await expect(recorded.locator("pre")).toHaveCount(1);
  await expect(recorded).not.toContainText("recorded_tail");
  await expect(recorded).not.toContainText("private-original");
  const toggle = recorded.getByText(english["ui.page_waits.recorded.full_statement"], { exact: true });
  await toggle.click();
  await expect(recorded.locator("pre")).toHaveCount(2);
  await expect(recorded.locator("pre").last()).toHaveText(sql);
  await expect(recorded.locator("script")).toHaveCount(0);
  await toggle.click();
  await expect(recorded.locator("pre")).toHaveCount(1);
  await toggle.click();
  await expect(recorded.locator("pre")).toHaveCount(2);
  report.recorded_incident.original.payload_sha256 = "d".repeat(64);
  await inject(page, pageEvidence([]), report, source);
  await expect(recorded.locator("pre")).toHaveCount(1);
  await expect(recorded.locator("details[open]")).toHaveCount(0);
  await page.setViewportSize({ width: 520, height: 900 });
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await recorded.screenshot({ path: info.outputPath("recorded-page-wait-empty-current.png") });
  report.recorded_incident.sql = { state: "unavailable", reason: "not-collected" };
  report.recorded_incident.original.payload_sha256 = "e".repeat(64);
  await inject(page, pageEvidence([]), report, source);
  await expect(recorded.locator("pre")).toHaveCount(0);
  report.recorded_incident.sql = { state: "available", text: sql, sha256: "c".repeat(64), redaction_version: 1 };
  report.recorded_incident.original.payload_sha256 = "f".repeat(64);
  await inject(page, pageEvidence([]), report, source);
  await expect(recorded.locator("pre")).toHaveCount(1);
  await expect(recorded.locator("details[open]")).toHaveCount(0);
});

test("parallel workers retain separate task waits and unverified owner fields behind disclosure", async ({ page }, info) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await openConnection(page, "mssql-demo");
  const observations = [pageTaskWait(1), pageTaskWait(2)];
  const report = pageReport(observations);
  report.title = TITLE;
  await inject(page, pageEvidence([...observations].reverse()), report, { source: PAGE_SOURCE, state: "collected", reason: null });
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await region.getByText("Inspect retained page waits", { exact: true }).click();
  const cards = region.locator("article");
  await expect(cards).toHaveCount(2);
  await expect(region).not.toContainText(english["ui.memory_failures.missing_row"]);
  const first = cards.first();
  await first.getByText("Investigation and native evidence", { exact: true }).click();
  await expect(first).toContainText("PAGELATCH_EX");
  await expect(first).toContainText("CXCONSUMER");
  await expect(first).toContainText("9007199254740993 ms");
  const taskDetails = first.getByText(english["ui.page_waits.task.details"], { exact: true });
  await taskDetails.click();
  await expect(first).toContainText("0000000000000001");
  await expect(first).not.toContainText("0000000000000002");
  await expect(first).toContainText(english["ui.page_waits.task.qualification"]);
  await expect(first.locator("dd", { hasText: /^-5$/ })).toBeVisible();
  await expect(first.locator("a")).toHaveCount(0);
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await first.screenshot({ path: info.outputPath("parallel-page-task-evidence.png") });
});

test("page waits retain the original affected request and qualified metadata in collapsed bounded cards", async ({ page }, info) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await openConnection(page, "mssql-demo");
  const native = fixture();
  await inject(page, native.evidence, native.report, { source: PAGE_SOURCE, state: "partial", reason: "Only bounded coordinator waits were retained." });
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  const cards = region.locator("article");
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await page.screenshot({ path: info.outputPath("page-waits-collapsed.png") });

  await region.getByText("Inspect retained page waits", { exact: true }).click();
  const first = cards.first();
  await expect(first.getByRole("heading")).toHaveText("Affected request 0");
  await first.getByText("Investigation and native evidence", { exact: true }).click();
  await expect(first).toContainText("In-memory page contention");
  await expect(first).toContainText("PAGELATCH_EX");
  await expect(first).toContainText("5:1:42");
  await expect(first).toContainText(PAGE_FROM);
  await expect(first).toContainText(PAGE_UNTIL);
  await expect(first).toContainText("18446744073709551615");
  await expect(first).toContainText("9007199254740993 ms");
  await first.getByText("Native request and wait rechecks", { exact: true }).click();
  await expect(first).toContainText(PAGE_BEFORE);
  await expect(first).toContainText(PAGE_AFTER);
  await expect(first).toContainText("Original wait no longer observed");
  await expect(first).toContainText("Same statement");
  await expect(first).toContainText("native-statement-handle");
  await first.getByText("Observed page and object metadata", { exact: true }).click();
  await expect(first).toContainText("orders_id");
  await expect(first).toContainText("-9223372036854775808");
  await expect(first).toContainText("000001:000002:0003");
  await first.getByText("Originally observed statement context", { exact: true }).click();
  await expect(first.locator("pre")).toHaveText("SELECT * FROM [orders] WHERE name=N'<script>canary</script>'");
  await expect(first.locator("script")).toHaveCount(0);
  await expect(first.locator("a")).toHaveCount(0);
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await first.screenshot({ path: info.outputPath("page-waits-original-context.png") });

  const unresolved = cards.nth(1);
  await unresolved.getByText("Investigation and native evidence", { exact: true }).click();
  await expect(unresolved).toContainText("Page I/O wait");
  await expect(unresolved).toContainText("PAGEIOLATCH_SH");
  await expect(unresolved).toContainText("5:1:43");
  await expect(unresolved.getByText("Observed page and object metadata", { exact: true })).toHaveCount(0);
  await region.getByRole("button", { name: "Next records", exact: true }).click();
  await expect(cards).toHaveCount(2);
  await expect(cards.first().getByRole("heading")).toHaveText("Affected request 10");
  await expect(region).toContainText("Observations 11–12 of 12");

  await inject(page, null, null, { source: PAGE_SOURCE, state: "unavailable", reason: "The page target could not be qualified safely." });
  await expect(region).toBeVisible();
  await expect(region.locator("article")).toHaveCount(0);
  await expect(region).toContainText("Not available here");
  const reason = region.getByText("The page target could not be qualified safely.", { exact: true });
  await expect(reason).not.toBeVisible();
  await region.getByText("What this evidence establishes", { exact: true }).click();
  await expect(reason).toBeVisible();
});
