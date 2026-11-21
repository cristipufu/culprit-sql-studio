import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

test("Health folds indexed views separately from table indexes and resets a withdrawn original", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { indexedViewsReport } = await import(fixturePath) as typeof import("../src/test/indexed-views");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(current.report);
    report.indexed_views = [indexedViewsReport()];
    setState({ report: { ...current, report }, view: "dashboard", advisor: false });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/indexed-views.ts" });
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.indexed_views.title"], exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(region.getByText("reporting.Revenue · ix_revenue", { exact: true })).toBeVisible();
  await region.getByText(english["ui.indexed_views.usage"], { exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(region.getByText("2026-11-10T07:00:10.123", { exact: true })).toBeVisible();
  await region.scrollIntoViewIfNeeded();
  await page.screenshot({ path: info.outputPath("indexed-view-original-desktop.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await region.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("indexed-view-original-narrow.png") });
  await page.evaluate(async ({ storePath, reason, summary }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(current.report);
    report.snapshot.id += "-withdrawn";
    report.indexed_views = report.indexed_views.map((view) => ({ ...view, entries: [], context: null, native_index_rows: null, observed_index_rows: 0, observed_view_indexes: 0, source: { source: "indexes", state: "unavailable", reason }, summary }));
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", reason: english["view.indexed_views.sharing_withheld"], summary: english["view.indexed_views.unavailable"] });
  await expect(region.getByText("reporting.Revenue · ix_revenue", { exact: true })).toHaveCount(0);
  await expect(region.getByText(english["view.indexed_views.unavailable"], { exact: true })).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
});
