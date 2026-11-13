import { expect, test, type Page } from "@playwright/test";
import type { MemoryFailureReport } from "../src/gen/MemoryFailureReport";
import type { MemoryFailureSnapshot } from "../src/gen/MemoryFailureSnapshot";
import type { MssqlOutOfMemoryObservation } from "../src/gen/MssqlOutOfMemoryObservation";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import { openConnection } from "./demo";

test.use({ viewport: { width: 1000, height: 900 } });

const SOURCE = "memory failures";
const NATIVE_TIME = "2099-01-01T11:30:00.003";
const UNPARSED_DOCUMENT = `{"unrecognized":${"x".repeat(3800)}<img src=x onerror=alert(1)>`;

function observation(position: number): MssqlOutOfMemoryObservation {
  return {
    position,
    event_time_without_offset: NATIVE_TIME,
    oom_cause: 11,
    oom_cause_desc: "QUERY_MEMORY_GRANTS",
    oom_factor: 10,
    oom_factor_desc: "QUERY_MEM_QUEUE",
    available_physical_memory_mb: -1,
    process_memory_usage_mb: 2_147_483_647,
    non_sos_memory_usage_mb: 64,
    committed_memory_target_mb: 1024,
    committed_memory_mb: 1000,
    allocation_potential_memory_mb: -1,
    initial_job_object_memory_limit_mb: null,
    current_job_object_memory_limit_mb: 0,
    possible_non_sos_leaked_memory_mb: null,
    oom_resource_pools: { raw: "null", qualification: "parsed" },
    top_memory_clerks: { raw: UNPARSED_DOCUMENT, qualification: "unparsed" },
    top_resource_pools: { raw: '{"future":"<script>alert(1)</script>"}', qualification: "parsed" },
    possible_leaked_memory_clerks: null,
  };
}

function retainedEvidence(): MemoryFailureSnapshot {
  return {
    source: SOURCE,
    observed_from: "2026-11-13T08:00:00Z",
    observed_until: "2026-11-13T08:00:01Z",
    scope: "server",
    resource: null,
    // Native fields are identical; only the capture-local position distinguishes these retained rows.
    observations: Array.from({ length: 12 }, (_, position) => observation(position)),
  };
}

function explanations(evidence: MemoryFailureSnapshot): MemoryFailureReport {
  return {
    title: "Retained memory failures",
    summary: "12 retained observations; no incident count established.",
    source_state: "partial",
    source_reason: "Only a bounded native buffer was read.",
    limitations: ["Native clocks have no established UTC offset.", "Affected statements and memory owners are unresolved."],
    entries: evidence.observations.map(({ position }) => ({
      position,
      title: `Retained native observation ${position}`,
      explanation: "The source reports a heuristic cause, without identifying a responsible query.",
      investigation: "Qualify the native clock and resource before correlating memory participants.",
      verify: "Compare matching native observations after a measured change; an empty buffer alone is not proof of recovery.",
    })),
  };
}

async function injectMemory(page: Page, evidence: MemoryFailureSnapshot | null, view: MemoryFailureReport | null, source: SourceOutcome) {
  // Opening a connection renders cached data before its refresh. Install the synthetic capture only after that
  // existing request has finished, so the refresh cannot legitimately replace the test's evidence underneath it.
  await expect.poll(async () => page.evaluate(async (path) => {
    const { getState } = await import(path) as { getState: () => State };
    const report = getState().report;
    return report.status === "ready" && !report.scanning;
  }, "/src/lib/store.ts")).toBe(true);
  await page.evaluate(async ({ path, evidence, view, source }) => {
    const { getState, setState } = await import(path) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("The fixture report must be loaded");
    const report = structuredClone(state.report.report);
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.operations = {
      log_failures: [], spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null,
      memory_failures: evidence ?? undefined,
    };
    report.snapshot.sources = [source];
    report.memory_failures = view ?? undefined;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", evidence, view, source });
}

test("retained memory evidence stays compact and preserves native rows, clocks and opaque documents", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  const evidence = retainedEvidence();
  await injectMemory(page, evidence, explanations(evidence), { source: SOURCE, state: "partial", reason: "Only a bounded native buffer was read." });
  await page.locator('[data-tile="health"]').click();
  const memory = page.locator(".health-tab > .operational-health").filter({ has: page.getByRole("heading", { name: "Retained memory failures", exact: true }) });
  const cards = memory.locator("article");
  await expect(memory).toBeVisible();
  await expect(memory).toContainText("12 retained observations");
  await expect(memory).toContainText("This read was partial.");
  await expect(cards).toHaveCount(10);
  await expect(cards.getByRole("heading")).toHaveText(Array.from({ length: 10 }, (_, position) => `Retained native observation ${position}`));
  await expect(memory.locator("details[open]")).toHaveCount(0);
  await expect(memory.getByText("Only a bounded native buffer was read.", { exact: true })).not.toBeVisible();
  await expect(cards.first()).toContainText("UTC offset unknown");
  await expect(cards.first().locator("code").first()).toHaveText(NATIVE_TIME);
  expect((await cards.first().boundingBox())!.height).toBeLessThan(300);
  await info.attach("memory-failures-collapsed-1000", { body: await page.screenshot(), contentType: "image/png" });

  await memory.getByRole("button", { name: "Next records", exact: true }).click();
  await expect(cards).toHaveCount(2);
  await expect(cards.getByRole("heading")).toHaveText(["Retained native observation 10", "Retained native observation 11"]);
  await expect(memory).toContainText("Observations 11–12 of 12");
  await expect(memory.getByRole("button", { name: "Next records", exact: true })).toBeDisabled();
  await memory.getByRole("button", { name: "Previous records", exact: true }).click();
  await expect(cards).toHaveCount(10);
  await expect(cards.first().getByRole("heading")).toHaveText("Retained native observation 0");

  const first = cards.first();
  await first.getByText("Investigation and native evidence", { exact: true }).click();
  await expect(first).toContainText("-1 MB");
  await expect(first).toContainText("2147483647 MB");
  await expect(first.locator('dt[title="initial_job_object_memory_limit_mb"] + dd')).toHaveText("Not reported");
  await expect(first.locator('dt[title="current_job_object_memory_limit_mb"] + dd')).toHaveText("0 MB");
  await expect(first.getByText("Original native JSON documents", { exact: true })).toBeVisible();
  await expect(first.locator("pre:visible")).toHaveCount(0);
  await first.getByText("Original native JSON documents", { exact: true }).click();
  await first.getByText("Native top memory clerks", { exact: true }).click();
  const document = first.locator("pre").filter({ hasText: UNPARSED_DOCUMENT });
  await expect(document).toBeVisible();
  await expect(document).toHaveText(UNPARSED_DOCUMENT);
  await expect(first).toContainText("could not be parsed within the supported JSON bounds");
  await expect(first.locator("img, script")).toHaveCount(0);
  const documentBounds = await document.evaluate((element) => ({
    width: element.clientWidth, scrollWidth: element.scrollWidth,
    height: element.clientHeight, scrollHeight: element.scrollHeight,
  }));
  expect(documentBounds.scrollWidth).toBeLessThanOrEqual(documentBounds.width + 1);
  expect(documentBounds.height).toBeLessThanOrEqual(289);
  expect(documentBounds.scrollHeight).toBeGreaterThan(documentBounds.height);
  const expandedBounds = await first.boundingBox();
  const neighborBounds = await cards.nth(1).boundingBox();
  expect(neighborBounds!.height).toBeLessThan(300);
  expect(expandedBounds!.width).toBeGreaterThan(neighborBounds!.width * 2);
  const sectionBounds = await memory.evaluate((element) => ({ width: element.clientWidth, scrollWidth: element.scrollWidth }));
  expect(sectionBounds.scrollWidth).toBeLessThanOrEqual(sectionBounds.width + 1);

  await first.getByText("Native exhausted resource pools", { exact: true }).click();
  await expect(first.locator("pre").filter({ hasText: /^null$/ })).toBeVisible();
  await first.getByText("Native possible leaked memory clerks", { exact: true }).click();
  await expect(first.getByText("SQL NULL: no document supplied.", { exact: true })).toBeVisible();
  await info.attach("memory-failures-native-details-1000", { body: await page.screenshot(), contentType: "image/png" });

  await injectMemory(page, null, null, { source: SOURCE, state: "unavailable", reason: "The native read did not establish permission or bounded completion." });
  await expect(memory).toBeVisible();
  await expect(memory).toContainText("Not available here");
  await expect(memory.locator("article")).toHaveCount(0);
  await expect(memory.locator("details[open]")).toHaveCount(0);
  const reason = memory.getByText("The native read did not establish permission or bounded completion.", { exact: true });
  await expect(reason).not.toBeVisible();
  await memory.getByText("What this evidence establishes", { exact: true }).click();
  await expect(reason).toBeVisible();
  await expect(memory).not.toContainText("0 failures");
  await info.attach("memory-failures-unavailable-1000", { body: await page.screenshot(), contentType: "image/png" });
});
