import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { CachedRuntimeRead } from "../src/gen/CachedRuntimeRead";
import type { CompletedSpillReport } from "../src/gen/CompletedSpillReport";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import { COMPLETED_SPILL_SOURCE, EXACT_SPILL_COUNT, SPILL_FROM, SPILL_UNTIL, completedSpillFixture, partialCompletedSpillFixture } from "../src/test/completed-spills";
import { openConnection, waitForReport } from "./demo";

const TITLE = english["view.completed_spills.title"];

async function inject(page: Page, read: CachedRuntimeRead | null, view: CompletedSpillReport | null, source?: SourceOutcome) {
  await waitForReport(page);
  await page.evaluate(async ({ read, view, source, path }) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A ready fixture report is required");
    const report = structuredClone(state.report.report);
    report.snapshot.operations.cached_runtime = read ?? undefined;
    report.completed_spills = view ?? undefined;
    report.snapshot.sources = source ? [source] : [];
    setState({ report: { ...state.report, report } });
  }, { read, view, source, path: "/src/lib/store.ts" });
}

test("completed-spill records disclose exact native values and statement evidence in bounded pages", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  const fixture = completedSpillFixture();
  const first = fixture.report.entries[0]!;
  first.statement_preview = "SELECT '<script>completed-spill-preview</script>'";
  first.statement_preview_truncated = true;
  await inject(page, fixture.read, fixture.report);
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  const records = region.locator(".observation-grid > .operational-card");
  await expect(region).toBeVisible();
  await expect(region).toContainText(SPILL_FROM);
  await expect(region).toContainText(SPILL_UNTIL);
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(records).toHaveCount(10);
  await expect(records.first()).not.toBeVisible();
  await expect(region.getByText(fixture.report.limitations[0]!, { exact: true })).not.toBeVisible();
  await region.getByText(english["ui.completed_spills.inspect"], { exact: true }).click();
  await expect(records.first()).toBeVisible();
  await expect(records.first()).toContainText(EXACT_SPILL_COUNT);
  await expect(records.first()).not.toContainText("9007199254740992");
  await expect(records.first()).toContainText("2026-11-13T09:59:00.003");
  await expect(records.first()).not.toContainText("2026-11-13T09:59:00.003Z");
  await records.first().getByText(english["ui.completed_spills.statement"], { exact: true }).click();
  await expect(records.first().locator("pre")).toHaveText(first.statement_preview);
  await expect(records.first()).toContainText(english["ui.completed_spills.statement_truncated"]);
  await expect(records.first().locator("script")).toHaveCount(0);
  await records.first().getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  await expect(records.first()).toContainText(first.verify);
  await expect(records.first()).toContainText(english["ui.completed_spills.read_reference"]);
  for (const privateValue of ["private-native-host", "private-native-process", "0xaabbccdd", "0xeeff0011"]) {
    expect(await region.evaluate((element) => element.outerHTML)).not.toContain(privateValue);
  }
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(records).toHaveCount(2);
  await expect(records.first().getByRole("heading")).toHaveText("Completed statement 11");
  await expect(records.first()).toContainText(EXACT_SPILL_COUNT);
  await region.getByRole("button", { name: english["ui.termination_history.previous"], exact: true }).click();
  await page.setViewportSize({ width: 480, height: 800 });
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await region.screenshot({ path: info.outputPath("completed-spills-expanded.png") });
});

test("partial definition and runtime reads stay independent, and empty or unavailable replacements clear old counters", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  const fixture = partialCompletedSpillFixture();
  await inject(page, fixture.read, fixture.report);
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  const component = page.locator(".completed-spills");
  await expect(region).toContainText("Original rows: 3. Rows read: 1.");
  await expect(region).toContainText(english["ui.common.read_partial"]);
  await expect(component.locator("details[open]")).toHaveCount(0);
  await expect(component.getByText("2026-11-13T10:01:00Z", { exact: false })).not.toBeVisible();
  await component.getByText(english["ui.completed_spills.definitions"], { exact: true }).first().click();
  await expect(component.getByText("2026-11-13T10:01:00Z", { exact: false })).toBeVisible();
  const definitions = component.getByRole("region", { name: english["ui.completed_spills.definitions"], exact: true });
  await definitions.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(definitions).toContainText("Some definition bodies were not read.");
  await region.getByText(english["ui.completed_spills.inspect"], { exact: true }).click();
  const entry = region.locator("article").first();
  await expect(entry).toContainText(english["view.completed_spills.binding.not_observed"]);
  await entry.getByText(english["ui.completed_spills.statement"], { exact: true }).click();
  await expect(entry).toContainText(english["ui.completed_spills.statement_unavailable"]);
  const empty = completedSpillFixture(0);
  await inject(page, empty.read, empty.report);
  await expect(region.locator("article")).toHaveCount(0);
  await expect(region).toContainText(english["view.completed_spills.summary.missing"]);
  await expect(region).not.toContainText(EXACT_SPILL_COUNT);
  await expect(component.getByText("2026-11-13T10:01:00Z", { exact: false })).toHaveCount(0);
  await inject(page, null, null, { source: COMPLETED_SPILL_SOURCE, state: "unavailable", reason: "Native completed-spill source denied." });
  await expect(region).toBeVisible();
  await expect(region.locator("article")).toHaveCount(0);
  await expect(region).not.toContainText(EXACT_SPILL_COUNT);
  await region.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(region).toContainText("Native completed-spill source denied.");
  await inject(page, fixture.read, null);
  await expect(region).toContainText(english["view.completed_spills.invalid"]);
  await expect(region).not.toContainText(EXACT_SPILL_COUNT);
});
