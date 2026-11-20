import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

async function setCatalog(page: Page, enabled: boolean | null) {
  await page.evaluate(async ({ path, enabled, words }) => {
    const { getState, setState } = await import(path) as typeof import("../src/lib/store");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("The owned demo report must be ready");
    const report = structuredClone(current.report);
    const table = report.snapshot.tables[0]!;
    table.mssql_full_text = {
      availability: "available",
      index: enabled === null ? null : {
        unique_index_id: 1, catalog_id: 3, enabled, change_tracking: "manual", last_crawl_completed: null,
      },
    };
    report.table_full_text = {
      title: words["view.table_full_text.title"], summary: "One owned catalog entry",
      source_state: "collected", source_reason: null,
      observed_from: null, observed_until: null, native_rows: null, observed_rows: 1,
      read_complete: null, limitations: [],
      entries: [{
        schema: table.schema, table: table.name, observation_key: null,
        summary: words[enabled === null ? "view.table_full_text.absent" : "view.table_full_text.present"],
        qualification: words["view.table_full_text.limit.catalog"], verify: words["view.table_full_text.verify"],
        metrics: enabled === null ? [] : [{
          key: "view.table_full_text.field.enabled", label: words["view.table_full_text.field.enabled"],
          value: 0, unit: "text", text: words[enabled ? "view.storage.yes" : "view.storage.no"],
        }],
      }],
    };
    setState({ report: { ...current, report }, view: "schema", advisor: false, schemaTable: `${table.schema}.${table.name}` });
  }, { path: "/src/lib/store.ts", enabled, words: english });
}

test("full-text details fold, support the keyboard and reset when native catalog values change", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await setCatalog(page, true);
  const panel = page.locator(".td-head details.td-chunks").filter({
    has: page.getByText(english["view.table_full_text.title"], { exact: true }),
  });
  await expect(panel).toBeVisible();
  await expect(panel).not.toHaveAttribute("open");
  expect((await panel.boundingBox())!.height).toBeLessThan(48);
  await panel.locator(":scope > summary").focus();
  await page.keyboard.press("Enter");
  await expect(panel.getByText(english["view.table_full_text.limit.catalog"], { exact: true })).toBeVisible();
  await expect(panel.locator("dd")).toHaveText(english["view.storage.yes"]);
  await panel.screenshot({ path: info.outputPath("full-text-desktop.png") });
  await page.getByRole("button", { name: "Hide tables ( [ )", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await panel.scrollIntoViewIfNeeded();
  await panel.screenshot({ path: info.outputPath("full-text-narrow.png") });
  expect(await panel.evaluate((node) => node.clientWidth > 80 && node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await setCatalog(page, false);
  await expect(panel).not.toHaveAttribute("open");
  await panel.locator(":scope > summary").click();
  await expect(panel.locator("dd")).toHaveText(english["view.storage.no"]);
  await setCatalog(page, null);
  await expect(panel).not.toHaveAttribute("open");
  await panel.locator(":scope > summary").click();
  await expect(panel.getByText(english["view.table_full_text.absent"], { exact: true })).toBeVisible();
  await expect(panel.locator("dd")).toHaveCount(0);
});
