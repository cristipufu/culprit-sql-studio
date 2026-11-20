import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, unavailable = false) {
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, unavailable, words }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { fullTextPopulationReport } = await import(fixturePath) as typeof import("../src/test/full-text-populations");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("The owned demo report must be ready");
    const report = structuredClone(current.report);
    report.snapshot.id = `full-text-presentation-${unavailable ? "withdrawn" : "recorded"}`;
    const original = fullTextPopulationReport();
    report.full_text_populations = [unavailable ? { ...original, original: null, native_rows: null, observed_rows: 0,
      read_complete: false, entries: [], summary: words["view.full_text_populations.sharing_withheld"],
      source: { source: original.source.source, state: "unavailable" as const, reason: words["view.full_text_populations.sharing_withheld"] } } : original];
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/full-text-populations.ts", unavailable, words: english });
}

test("original full-text populations use bounded folded context, native local time and withdrawal", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await inject(page);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.full_text_populations.title"], exact: true }).first();
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  const original = region.locator("article.operational-card").first();
  await expect(original).not.toBeVisible();
  await region.locator(":scope > details").filter({ has: page.getByText(english["ui.retained_failures.observations"], { exact: true }) }).locator(":scope > summary").click();
  await expect(original).toBeVisible();
  const population = original.locator(".observation-list");
  await population.locator(":scope > details").filter({ has: page.getByText(english["ui.retained_failures.observations"], { exact: true }) }).locator(":scope > summary").click();
  await expect(population.getByText("2019-12-31T18:00:00.007", { exact: true })).toBeVisible();
  await expect(population.locator(".sev")).toHaveCount(0);
  await original.screenshot({ path: info.outputPath("full-text-population-desktop.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  await original.screenshot({ path: info.outputPath("full-text-population-narrow.png") });
  expect(await original.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await inject(page, true);
  await expect(region.locator("details[open]")).toHaveCount(0);
  await region.locator(":scope > details").filter({ has: page.getByText(english["ui.retained_failures.observations"], { exact: true }) }).locator(":scope > summary").click();
  await expect(region).toContainText(english["view.full_text_populations.sharing_withheld"]);
  await expect(region.locator("dd")).toHaveCount(0);
  await expect(region).not.toContainText("2019-12-31T18:00:00.007");
});
