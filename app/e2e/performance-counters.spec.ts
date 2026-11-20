import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, scenario: "shared" | "unavailable" | "both" = "shared") {
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, scenario, withheld }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { performanceCounterHealthReport, performanceCounterReport } = await import(fixturePath);
    const state = getState(); if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    const fixture = scenario === "unavailable" ? performanceCounterHealthReport(null) : performanceCounterHealthReport();
    if (scenario === "both") {
      const selected = performanceCounterReport(0);
      selected.population = "selected-database";
      selected.source_state = "unavailable";
      selected.source_reason = withheld;
      selected.summary = withheld;
      selected.limitations = [withheld];
      selected.read_complete = null;
      selected.native_observations = null;
      selected.observed_observations = 0;
      selected.native_counter_rows = null;
      selected.missing_fields = [];
      fixture.performance_counters.push(selected);
    }
    report.performance_counters = fixture.performance_counters; report.snapshot.sources = fixture.snapshot.sources;
    setState({ report: { ...state.report, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/performance-counters.ts", scenario,
    withheld: english["view.performance_counters.sharing_withheld"] });
}

test("performance counters remain folded and page exact values with missing fields kept unknown", async ({ page }) => {
  await openConnection(page, "mssql-demo"); await inject(page);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.performance_counters.title"], exact: true });
  const cards = region.locator(".operational-card");
  await expect(region).toBeVisible(); await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(10); await expect(cards.first()).not.toBeVisible();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(cards.first().getByText("9007199254740993 KiB", { exact: true })).toBeVisible();
  await expect(cards.nth(1).getByText("0 KiB", { exact: true })).toBeVisible();
  await expect(region.locator(".sig-dot")).toHaveCount(0);
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(cards).toHaveCount(2); await expect(cards.first()).toContainText("native-counter-10");
  await region.getByRole("button", { name: english["ui.termination_history.previous"], exact: true }).click();
  await expect(cards.first()).toContainText("native-counter-0");
});

test("an unavailable performance source remains visible without fabricated counter values", async ({ page }) => {
  await openConnection(page, "mssql-demo"); await inject(page, "unavailable");
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.performance_counters.title"], exact: true });
  await expect(region).toContainText(english["ui.dashboard.status.unavailable"]);
  await expect(region.locator("article")).toHaveCount(0); await expect(region).not.toContainText("9007199254740993");
});

test("a withdrawn selected population keeps its scope and does not change shared counter paging", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await inject(page, "both");
  await page.locator('[data-tile="health"]').click();
  const regions = page.getByRole("region", { name: english["view.performance_counters.title"], exact: true });
  await expect(regions).toHaveCount(2);
  const shared = regions.filter({ hasText: english["view.performance_counters.scope"] });
  const selected = regions.filter({ hasText: english["view.performance_counters.selected_scope"] });
  await expect(shared).toBeVisible();
  await expect(selected).toBeVisible();
  await expect(selected).toContainText(english["ui.dashboard.status.unavailable"]);
  await expect(selected).toContainText(english["view.performance_counters.sharing_withheld"]);
  await expect(selected.locator("article")).toHaveCount(0);
  await expect(selected).not.toContainText("9007199254740993");
  await shared.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await shared.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(shared.locator("article")).toHaveCount(2);
  await expect(shared.locator("article").first()).toContainText("native-counter-10");
  await expect(selected.locator("details[open]")).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  await selected.screenshot({ path: info.outputPath("selected-counter-withdrawn-narrow.png") });
});
