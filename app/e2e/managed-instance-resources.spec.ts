import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

test("original instance history folds, pages and resets on immutable saved report replacement", async ({ page }, info) => {
  await openConnection(page, "mssql-demo"); await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, i18nPath }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { managedInstanceResourceReport } = await import(fixturePath) as typeof import("../src/test/managed-instance-resources");
    const current = getState().report; if (current.status !== "ready") throw new Error("owned demo report required");
    const report = structuredClone(current.report); report.managed_instance_resources = managedInstanceResourceReport();
    const { t } = await import(i18nPath) as typeof import("../src/lib/i18n");
    report.managed_instance_resources.summary = t("view.mi_resources.summary", { n: 12, count: "12" });
    const interval = report.managed_instance_resources.intervals[0]!;
    const start = Date.parse("2020-01-01T06:00:00Z");
    report.managed_instance_resources.intervals = Array.from({ length: 12 }, (_, i) => ({ ...structuredClone(interval), started_at: new Date(start + i * 15_000).toISOString(), ended_at: new Date(start + (i + 1) * 15_000).toISOString() }));
    report.managed_instance_resources.until = "2020-01-01T06:03:15Z";
    report.managed_instance_resources.native_rows = 12; report.managed_instance_resources.recorded_rows = 12; report.managed_instance_resources.observed_rows = 12;
    report.managed_instance_resources.native_source!.records = 12;
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/managed-instance-resources.ts", i18nPath: "/src/lib/i18n.ts" });
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.mi_resources.title"], exact: true }).first();
  await expect(region).toBeVisible(); await expect(region.locator("article")).toHaveCount(0);
  await region.locator(":scope > details > summary").focus(); await page.keyboard.press("Enter");
  await expect(region.locator("article")).toHaveCount(10); await expect(region.getByText("9007199254740993", { exact: true }).first()).toBeVisible();
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click(); await expect(region.locator("article")).toHaveCount(2);
  await page.screenshot({ path: info.outputPath("mi-history-desktop.png") });
  await page.setViewportSize({ width: 390, height: 844 }); await page.screenshot({ path: info.outputPath("mi-history-narrow.png") });
  expect(await region.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.evaluate(async (storePath) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store"); const current = getState().report;
    if (current.status !== "ready") throw new Error("owned demo report required");
    const report = structuredClone(current.report); report.snapshot.id += "-other-original"; const history = report.managed_instance_resources!;
    history.source_state = "unavailable"; history.intervals = []; history.observed_at = null; history.native_source = null; history.native_rows = null; history.recorded_rows = null; history.observed_rows = 0; history.read_complete = false; history.from = null; history.until = null;
    setState({ report: { ...current, report } });
  }, "/src/lib/store.ts");
  await expect(region.locator("article")).toHaveCount(0); await expect(region.locator(":scope > details[open]")).toHaveCount(0);
});
