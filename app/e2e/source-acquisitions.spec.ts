import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";
async function inject(page: Page, withheld = false) {
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, withheld, words }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { historicalAcquisition } = await import(fixturePath) as typeof import("../src/test/source-acquisitions");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("The demo report must be ready");
    const report = structuredClone(current.report);
    report.source_acquisitions = withheld ? [{ kind: "unavailable", reason: words["view.source_overhead.sharing_withheld"] }]
      : Array.from({ length: 13 }, (_, index) => historicalAcquisition(index));
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/source-acquisitions.ts", withheld, words: english });
}
test("historical acquisition metadata is folded and paged independently of current scan scope", async ({ page }, info) => {
  await openConnection(page, "mssql-demo"); await inject(page);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.source_overhead.title"], exact: true }).first();
  const cards = region.locator("article.operational-card");
  await expect(region).toBeVisible(); await expect(region.locator("details[open]")).toHaveCount(0);
  await region.scrollIntoViewIfNeeded();
  await page.screenshot({ path: info.outputPath("source-acquisitions-folded.png") });
  await expect(cards).toHaveCount(10); await expect(cards.first()).not.toBeVisible();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).first().click();
  await expect(cards.first().getByRole("heading", { name: "original-capture-0" })).toBeVisible();
  await cards.first().getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(cards.first()).toContainText("9007199254740993");
  await page.setViewportSize({ width: 390, height: 844 });
  await cards.first().scrollIntoViewIfNeeded();
  expect(await cards.first().evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("source-acquisitions-details-narrow.png") });
  expect(await region.innerHTML()).not.toContain("private-manifest-digest");
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(cards).toHaveCount(3); await expect(cards.first().getByRole("heading", { name: "original-capture-10" })).toBeVisible();
});
test("sharing withdrawal stays visible without historical clocks or invented zero work", async ({ page }) => {
  await openConnection(page, "mssql-demo"); await inject(page, true);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.source_overhead.title"], exact: true }).first();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(region).toContainText(english["view.source_overhead.sharing_withheld"]);
  await expect(region).not.toContainText("2020-01-01"); await expect(region).not.toContainText("9007199254740993");
});
