import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

async function setStorage(page: Page, name: "file_census_large" | "file_census_unknown_basis" | "file_census_denied") {
  await page.evaluate(async ({ storePath, fixturePath, name }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { storageReport } = await import(fixturePath);
    const current = getState().report;
    if (current.status !== "ready") throw new Error("The original demo report must be settled");
    const report = structuredClone(current.report);
    report.storage = storageReport(name);
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/storage.ts", name });
}

test("original file census is folded, keyboard accessible, exact before row limits and resets on withdrawn quantities", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await setStorage(page, "file_census_large");
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.storage.title"], exact: true });
  const census = region.locator("details.storage-file-census");
  await expect(census).toBeVisible();
  await expect(census).not.toHaveAttribute("open");
  await census.locator(":scope > summary").focus();
  await page.keyboard.press("Enter");
  await expect(census.getByText("5277655810867200", { exact: true })).toBeVisible();
  await expect(census.getByText("300", { exact: true })).toBeVisible();
  await expect(census.getByText(english["view.storage.file_summary.limit.log"], { exact: true })).toBeVisible();
  await expect(census.locator(".sig-dot,.sev")).toHaveCount(0);
  await census.screenshot({ path: info.outputPath("database-file-census-desktop.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await census.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
  await census.screenshot({ path: info.outputPath("database-file-census-narrow.png") });
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  const log = region.locator("article").filter({ has: page.getByRole("heading", { name: english["view.storage.population.log"], exact: true }) });
  await expect(log.getByText("2020-01-01T12:00:00.75Z", { exact: true })).toBeVisible();
  await expect(log.getByText("196608", { exact: true })).toBeVisible();
  await setStorage(page, "file_census_unknown_basis");
  await expect(census).not.toHaveAttribute("open");
  await census.locator(":scope > summary").click();
  await expect(census.getByText(english["ui.memory_failures.not_reported"], { exact: true }).first()).toBeVisible();
  await expect(census.getByText("2", { exact: true })).toBeVisible();
  await setStorage(page, "file_census_denied");
  await expect(census).not.toHaveAttribute("open");
  await census.locator(":scope > summary").click();
  await expect(census.getByText(english["view.storage.file_summary.unavailable"], { exact: true })).toBeVisible();
  await expect(census.getByText("5277655810867200", { exact: true })).toHaveCount(0);
});
