import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, unavailable = false) {
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, unavailable }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { configuredLogRateHealthReport } = await import(fixturePath);
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    const fixture = unavailable ? configuredLogRateHealthReport(null) : configuredLogRateHealthReport();
    report.configured_log_rates = fixture.configured_log_rates;
    report.snapshot.sources = fixture.snapshot.sources;
    setState({ report: { ...state.report, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/configured-log-rates.ts", unavailable });
}

test("configured native limits disclose exact values only after opening folded observations", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  await inject(page);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.configured_log_rates.title"], exact: true });
  const cards = region.locator(".operational-card");
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(3);
  await expect(cards.first()).not.toBeVisible();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(cards.first().getByText("9007199254740993 B/s", { exact: true })).toBeVisible();
  await expect(cards.nth(1).getByText("0 B/s", { exact: true })).toBeVisible();
  await expect(cards.nth(2).getByText(english["view.configured_log_rates.unknown"], { exact: true })).toBeVisible();
  await expect(region.locator(".sig-dot")).toHaveCount(0);
});

test("a retained configuration source failure remains visible without a fabricated cap", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  await inject(page, true);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.configured_log_rates.title"], exact: true });
  await expect(region).toContainText(english["ui.dashboard.status.unavailable"]);
  await expect(region.locator("article")).toHaveCount(0);
  await expect(region).not.toContainText("0 B/s");
});
