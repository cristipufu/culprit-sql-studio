import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, withheld = false) {
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, withheld, words }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { tableMemoryReport } = await import(fixturePath) as typeof import("../src/test/table-memory");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("The owned demo report must be ready");
    const report = structuredClone(current.report);
    report.table_memory = Array.from({ length: withheld ? 1 : 13 }, (_, index) => {
      const original = tableMemoryReport(index);
      return withheld ? {
        ...original, original: null, native_rows: null, observed_rows: 0, read_complete: false,
        source: { source: original.source.source, state: "unavailable" as const, reason: words["view.table_memory.sharing_withheld"] },
        summary: words["view.table_memory.sharing_withheld"], metrics: [],
      } : original;
    });
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/table-memory.ts", withheld, words: english });
}

test("independent table-memory originals stay folded and page exact native values", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await inject(page);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.table_memory.title"], exact: true }).first();
  const cards = region.locator("article.operational-card");
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(cards.first().getByRole("heading", { name: "dbo.original-table-0" })).toBeVisible();
  await expect(cards.first().getByText("9007199254740993", { exact: true })).toBeVisible();
  await expect(cards.first().getByText("0", { exact: true })).toBeVisible();
  await expect(cards.first().getByText(english["view.table_memory.unknown"], { exact: true })).toBeVisible();
  await expect(region.locator(".sig-dot")).toHaveCount(0);
  expect(await region.innerHTML()).not.toContain("private-original-host");
  expect(await region.innerHTML()).not.toContain("private-process-and-database");
  await cards.first().screenshot({ path: info.outputPath("table-memory-desktop.png") });
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(cards).toHaveCount(3);
  await expect(cards.first().getByRole("heading", { name: "dbo.original-table-10" })).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  await cards.first().screenshot({ path: info.outputPath("table-memory-narrow.png") });
  expect(await cards.first().evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
});

test("withdrawn table-memory originals retain unavailable history without measurements or native owners", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  await inject(page, true);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.table_memory.title"], exact: true }).first();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(region).toContainText(english["view.table_memory.sharing_withheld"]);
  await expect(region).not.toContainText("original-table-0");
  await expect(region).not.toContainText("9007199254740993");
  await expect(region.locator("dd")).toHaveCount(0);
});
