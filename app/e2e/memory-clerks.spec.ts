import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, unavailable = false) {
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, unavailable }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { memoryClerkHealthReport } = await import(fixturePath);
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    const fixture = unavailable ? memoryClerkHealthReport(null) : memoryClerkHealthReport();
    report.memory_clerks = fixture.memory_clerks;
    report.snapshot.sources = fixture.snapshot.sources;
    setState({ report: { ...state.report, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/memory-clerks.ts", unavailable });
}

test("clerk allocations stay folded and page exact values without a pressure grade", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  await inject(page);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["collected.memory_clerks.title"], exact: true });
  const cards = region.locator(".operational-card");
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(cards.first().getByText("9007199254740993", { exact: true })).toBeVisible();
  await expect(cards.first().getByText("0", { exact: true }).last()).toBeVisible();
  await expect(cards.first().getByText(english["ui.memory_failures.not_reported"], { exact: true })).toHaveCount(2);
  await expect(region.locator(".sig-dot")).toHaveCount(0);
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(cards).toHaveCount(2);
  await expect(cards.first()).toContainText("native-clerk-10");
  await region.getByRole("button", { name: english["ui.termination_history.previous"], exact: true }).click();
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).toContainText("native-clerk-0");
});

test("an unavailable clerk body remains an explicit source gap", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  await inject(page, true);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["collected.memory_clerks.title"], exact: true });
  await expect(region).toContainText(english["ui.dashboard.status.unavailable"]);
  await expect(region.locator("article")).toHaveCount(0);
  await expect(region).not.toContainText("9007199254740993");
});
