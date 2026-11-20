import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { MssqlTableStorageConfiguration } from "../src/gen/MssqlTableStorageConfiguration";
import { openConnection, waitForReport } from "./demo";

async function setConfiguration(page: Page, configuration: MssqlTableStorageConfiguration | null) {
  await page.evaluate(async ({ path, configuration }) => {
    const { getState, setState } = await import(path) as typeof import("../src/lib/store");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("The owned demo report must be ready");
    const report = structuredClone(current.report);
    const table = report.snapshot.tables[0]!;
    table.mssql_storage_configuration = configuration;
    setState({ report: { ...current, report }, view: "schema", advisor: false, schemaTable: `${table.schema}.${table.name}` });
  }, { path: "/src/lib/store.ts", configuration });
}

test("table storage context folds, supports the keyboard and resets when captured values change", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await setConfiguration(page, { memory_optimized: true, durability: 1, durability_desc: "SCHEMA_ONLY" });
  const panel = page.locator(".td-head details.td-chunks").filter({
    has: page.getByText(english["view.table_storage.title"], { exact: true }),
  });
  await expect(panel).toBeVisible();
  await expect(panel).not.toHaveAttribute("open");
  expect((await panel.boundingBox())!.height).toBeLessThan(48);
  await panel.locator(":scope > summary").focus();
  await page.keyboard.press("Enter");
  await expect(panel.locator("dd")).toHaveText([english["ui.common.yes"], "1", "SCHEMA_ONLY"]);
  await expect(panel.getByText(english["view.table_storage.qualification"], { exact: true })).toBeVisible();
  await expect(panel.locator(".sev")).toHaveCount(0);
  await panel.screenshot({ path: info.outputPath("table-storage-desktop.png") });
  await page.getByRole("button", { name: "Hide tables ( [ )", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await panel.scrollIntoViewIfNeeded();
  await panel.screenshot({ path: info.outputPath("table-storage-narrow.png") });
  expect(await panel.evaluate((node) => node.clientWidth > 80 && node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await setConfiguration(page, { memory_optimized: true, durability: 0, durability_desc: "SCHEMA_AND_DATA" });
  await expect(panel).not.toHaveAttribute("open");
  await panel.locator(":scope > summary").click();
  await expect(panel.locator("dd")).toHaveText([english["ui.common.yes"], "0", "SCHEMA_AND_DATA"]);
  await setConfiguration(page, null);
  await expect(panel).toHaveCount(0);
});
