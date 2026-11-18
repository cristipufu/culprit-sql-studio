import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, unavailable = false) {
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, unavailable }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { performanceCounterHealthReport } = await import(fixturePath);
    const state = getState(); if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    const fixture = unavailable ? performanceCounterHealthReport(null) : performanceCounterHealthReport();
    report.performance_counters = fixture.performance_counters; report.snapshot.sources = fixture.snapshot.sources;
    setState({ report: { ...state.report, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/performance-counters.ts", unavailable });
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
  await openConnection(page, "mssql-demo"); await inject(page, true);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.performance_counters.title"], exact: true });
  await expect(region).toContainText(english["ui.dashboard.status.unavailable"]);
  await expect(region.locator("article")).toHaveCount(0); await expect(region).not.toContainText("9007199254740993");
});
