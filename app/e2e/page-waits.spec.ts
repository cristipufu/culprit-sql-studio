import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { PageWaitReport } from "../src/gen/PageWaitReport";
import type { PageWaitSnapshot } from "../src/gen/PageWaitSnapshot";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import { PAGE_AFTER, PAGE_BEFORE, PAGE_FROM, PAGE_SOURCE, PAGE_UNTIL, pageEvidence, pageReport, pageWait } from "../src/test/page-waits";
import { openConnection } from "./demo";

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
  await expect.poll(async () => page.evaluate(async (path) => {
    const { getState } = await import(path) as { getState: () => State };
    const report = getState().report;
    return report.status === "ready" && !report.scanning;
  }, "/src/lib/store.ts")).toBe(true);
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
