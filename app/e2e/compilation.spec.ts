import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { CompilationMemory } from "../src/gen/CompilationMemory";
import type { CompilationReport } from "../src/gen/CompilationReport";
import type { CompilationSnapshot } from "../src/gen/CompilationSnapshot";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import { COMPILATION_SOURCE, compilationComparisonReport, compilationEvidence, compilationObservation, compilationReport, unavailableCompilationComparisonReport } from "../src/test/compilation";
import { openConnection, waitForReport } from "./demo";

const TITLE = english["view.compilation.title"];
const EXACT_COUNT = "9007199254740993";
const NATIVE_CONTEXT = `<script>compiled-context-executed</script>${"x".repeat(512)}`;

async function inject(page: Page, evidence: CompilationSnapshot | null, view: CompilationReport | null, source: SourceOutcome) {
  await waitForReport(page);
  await page.evaluate(async ({ path, evidence, view, source }) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A ready fixture report is required");
    const report = structuredClone(state.report.report);
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.operations = {
      ...report.snapshot.operations, spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null,
      memory_failures: undefined, native_failures: undefined, termination_history: undefined, node_resources: undefined,
      session_memory: undefined, page_waits: undefined, task_space: undefined, query_compilation: evidence ?? undefined,
    };
    report.snapshot.sources = [source];
    report.memory_failures = undefined;
    report.native_failures = undefined;
    report.request_memory = undefined;
    report.session_memory = undefined;
    report.page_waits = undefined;
    report.task_space = undefined;
    report.query_compilation = view ?? undefined;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", evidence, view, source });
}

test("compilation history keeps exact native identities in collapsed paged records without borrowing mismatched evidence", async ({ page }, info) => {
  await page.setViewportSize({ width: 1000, height: 900 });
  await openConnection(page, "mssql-demo");
  const rows = Array.from({ length: 12 }, (_, index) => compilationObservation(index));
  const firstNative = rows[0]!;
  if (firstNative.fact.kind !== "mssql-query-compilation") throw new Error("Compilation fixture required");
  firstNative.fact.data.identity.qualifiers["native-context"] = NATIVE_CONTEXT;
  // Freeze explanation references before changing the supplied native rows. Reusing a numeric query ID or key
  // must not mutate the explanation into agreement with unrelated evidence.
  const view = structuredClone(compilationReport(rows));
  view.title = TITLE;
  rows[1]!.source = "unrelated native source";
  const changed = rows[2]!;
  if (changed.fact.kind !== "mssql-query-compilation") throw new Error("Compilation fixture required");
  changed.fact.data.identity.qualifiers.context_settings_id = "6";
  await inject(page, compilationEvidence([...rows].reverse()), view, {
    source: COMPILATION_SOURCE, state: "partial", reason: "Some retained entries were not visible.",
  });
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  const records = region.locator(".observation-grid > .operational-card");
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(records).toHaveCount(10);
  await expect(records.first()).not.toBeVisible();
  const collapsed = info.outputPath("compilation-collapsed.png");
  await page.screenshot({ path: collapsed });
  await info.attach("compilation-collapsed", { path: collapsed, contentType: "image/png" });

  await region.getByText(english["ui.compilation.inspect"], { exact: true }).click();
  const first = records.first();
  await expect(first.getByRole("heading")).toHaveText("Native query 3");
  await expect(first).toContainText(EXACT_COUNT);
  await expect(first).not.toContainText("9007199254740992");
  await expect(first.locator("details[open]")).toHaveCount(0);
  await first.getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  await first.getByText(english["ui.memory_participants.native_properties"], { exact: true }).click();
  for (const value of ["3", "context_settings_id", "5", "object_id", "1999", "query_hash", "0123456789ABCDEF", "initial_compile_at", "2020-01-01T06:00:00Z"]) {
    await expect(first.getByText(value, { exact: true })).toBeVisible();
  }
  await expect(first.getByText(NATIVE_CONTEXT, { exact: true })).toBeVisible();
  await expect(first.locator("script")).toHaveCount(0);
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await first.scrollIntoViewIfNeeded();
  const expanded = info.outputPath("compilation-native-context.png");
  await page.screenshot({ path: expanded });
  await info.attach("compilation-native-context", { path: expanded, contentType: "image/png" });
  for (const invalid of [records.nth(1), records.nth(2)]) {
    await expect(invalid).toContainText(english["ui.memory_failures.missing_row"]);
    await expect(invalid).not.toContainText(EXACT_COUNT);
    await expect(invalid.locator("details")).toHaveCount(0);
  }
  await region.getByRole("button", { name: "Next records", exact: true }).click();
  await expect(records).toHaveCount(2);
  await expect(records.first().getByRole("heading")).toHaveText("Native query 13");
  await expect(records.first()).toContainText(EXACT_COUNT);

  await inject(page, null, null, {
    source: COMPILATION_SOURCE, state: "unavailable", reason: "Native compilation permission denied.",
  });
  await expect(region).toBeVisible();
  await expect(region.locator("article")).toHaveCount(0);
  await expect(region).not.toContainText(EXACT_COUNT);
  await region.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(region).toContainText("Native compilation permission denied.");
});

test("compilation comparison disclosures keep exact values and retire absent or unavailable comparisons", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  const rows = [compilationObservation(), compilationObservation(1)];
  const view = compilationComparisonReport(rows);
  const comparison = view.comparison;
  const matching = comparison?.entries[0];
  const unrelated = comparison?.entries[1];
  const baseline = comparison?.baseline;
  const database = baseline?.database;
  const limitation = comparison?.limitations[0];
  if (!comparison?.observed_from || !comparison.observed_until || !matching || !unrelated || !baseline?.id
      || !database?.incarnation || database.location.kind !== "server" || !limitation) {
    throw new Error("Two native comparison entries and their original provenance are required");
  }
  const privateBaselineValues = [baseline.id, database.incarnation, database.location.host];
  unrelated.identity.qualifiers.context_settings_id = "different-native-context";
  const source: SourceOutcome = { source: COMPILATION_SOURCE, state: "partial", reason: view.source_reason };
  await inject(page, compilationEvidence(rows), view, source);
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  const records = region.locator(".observation-grid > .operational-card");
  await expect(region).toContainText(comparison.summary);
  await expect(region).toContainText(comparison.observed_from);
  await expect(region).toContainText(comparison.observed_until);
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(region.getByText(limitation, { exact: true })).not.toBeVisible();
  await expect(records.first()).not.toBeVisible();
  await region.screenshot({ path: info.outputPath("compilation-comparison-collapsed.png") });

  const limits = region.getByText(english["ui.memory_failures.limitations"], { exact: true });
  await limits.click();
  await expect(region.getByText(limitation, { exact: true })).toBeVisible();
  await limits.click();
  await expect(region.getByText(limitation, { exact: true })).not.toBeVisible();
  await region.getByText(english["ui.compilation.inspect"], { exact: true }).click();
  const first = records.first();
  const second = records.nth(1);
  await expect(first.getByText(matching.explanation, { exact: true })).toBeVisible();
  await expect(first).toContainText("18446744073709551615");
  await expect(first).not.toContainText("18446744073709552000");
  await expect(second).toContainText(EXACT_COUNT);
  await expect(second).not.toContainText(unrelated.explanation);
  for (const record of [first, second]) {
    await record.getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  }
  await expect(first.getByText(matching.investigation, { exact: true })).toBeVisible();
  await expect(second.getByText(unrelated.investigation, { exact: true })).toHaveCount(0);
  const rendered = await region.evaluate((element) => element.outerHTML);
  for (const privateValue of privateBaselineValues) {
    expect(rendered).not.toContain(privateValue);
  }
  await page.setViewportSize({ width: 480, height: 800 });
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await region.screenshot({ path: info.outputPath("compilation-comparison-narrow.png") });

  await inject(page, compilationEvidence(rows), compilationReport(rows), source);
  await expect(first).toBeVisible();
  await expect(first).toContainText(EXACT_COUNT);
  await expect(region).not.toContainText(matching.explanation);
  await expect(region).not.toContainText(comparison.summary);
  await expect(region).not.toContainText(comparison.observed_from);
  await expect(region).not.toContainText(limitation);

  const unavailable = unavailableCompilationComparisonReport(rows);
  const unavailableComparison = unavailable.comparison;
  const unavailableReason = unavailableComparison?.limitations[0];
  if (!unavailableComparison || !unavailableReason) throw new Error("Unavailable comparison fixture required");
  await inject(page, compilationEvidence(rows), unavailable, source);
  await expect(region).toContainText(unavailableComparison.summary);
  await expect(first).toBeVisible();
  await expect(first).toContainText(EXACT_COUNT);
  await expect(region).not.toContainText("18446744073709551615");
  await expect(region).not.toContainText(comparison.observed_from);
  await expect(region).not.toContainText(comparison.observed_until);
  await expect(region.getByText(unavailableReason, { exact: true })).not.toBeVisible();
  await limits.click();
  await expect(region.getByText(unavailableReason, { exact: true })).toBeVisible();
  const unavailableRendered = await region.evaluate((element) => element.outerHTML);
  for (const privateValue of privateBaselineValues) {
    expect(unavailableRendered).not.toContain(privateValue);
  }
});

test("Query details show individual compilation memory without multiplying it by the compile count", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  const memory: CompilationMemory = { average_kb: 32768, last_kb: 16384, maximum_kb: 65536 };
  await page.evaluate(async ({ path, memory }) => {
    const { getState, setState } = await import(path) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("The fixture report must be loaded");
    const report = structuredClone(state.report.report);
    const query = report.snapshot.workload?.queries[0];
    if (!query?.costs) throw new Error("The fixture must contain a query with recorded costs");
    query.costs = { ...query.costs, compile_history: {
      count: 300, total_ms: 6000, first_at: "2026-11-13T09:00:00Z", last_at: "2026-11-13T09:59:00Z",
      within_window: true, memory,
    } };
    setState({ report: { ...state.report, report }, view: "queries", query: query.id });
  }, { path: "/src/lib/store.ts", memory });
  const row = page.locator(".beyond-row").filter({ has: page.getByText("Average compile memory", { exact: true }) });
  await expect(row).toBeVisible();
  await expect(row.locator(".beyond-value")).toHaveText("32.0 MB");
  await expect(row).toContainText("16.0 MB last");
  await expect(row).toContainText("64.0 MB peak");
  await expect(row).toContainText("current use and concurrency are unknown");
  await expect(row).not.toContainText("GB");
  await row.screenshot({ path: info.outputPath("compile-memory.png") });

  await page.evaluate(async (path) => {
    const { getState, setState } = await import(path) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("The fixture report must be loaded");
    const report = structuredClone(state.report.report);
    for (const query of report.snapshot.workload?.queries ?? []) {
      if (query.costs?.compile_history) query.costs.compile_history.memory = undefined;
    }
    setState({ report: { ...state.report, report } });
  }, "/src/lib/store.ts");
  await expect(row).toHaveCount(0);
});
