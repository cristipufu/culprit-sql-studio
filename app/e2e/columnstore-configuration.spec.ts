import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

test("columnstore configuration folds, supports the keyboard and wraps bounded native names", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(async (path) => {
    const { getState, setState } = await import(path) as typeof import("../src/lib/store");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("The owned demo report must be ready");
    const report = structuredClone(current.report);
    const index = report.snapshot.indexes[0]!;
    index.kind = "CLUSTERED COLUMNSTORE";
    index.mssql_columnstore = {
      table: { database_id: 7, object_id: 100, created_without_offset: "2026-11-10T07:00:00.123" },
      index_id: 1, schema: index.schema, table_name: index.table, name: index.name,
      table_modified_without_offset: "2026-11-10T07:01:00.123",
      compression_delay_availability: "available", compression_delay_minutes: null,
      column_store_order_availability: "available", data_clustering_availability: "unsupported",
      columns: [{ index_column_id: 1, column_id: 1, name: "owned<script>column</script>" + "界".repeat(300), column_store_order_ordinal: 0, data_clustering_ordinal: null }],
    };
    setState({ report: { ...current, report }, view: "schema", advisor: false, schemaTable: `${index.schema}.${index.table}` });
  }, "/src/lib/store.ts");
  await page.getByRole("tab", { name: /^Indexes/ }).click();
  const panel = page.locator("details.index-columnstore");
  await expect(panel).toBeVisible();
  await expect(panel).not.toHaveAttribute("open");
  expect((await panel.boundingBox())!.height).toBeLessThan(48);
  await panel.locator(":scope > summary").focus();
  await page.keyboard.press("Enter");
  await expect(panel.locator("dd").first()).toHaveText(english["ui.indexes.columnstore.native_null"]);
  await expect(panel.locator("tbody tr").first().locator("td").nth(1)).toHaveText("0");
  expect(await panel.locator("code").textContent()).toContain("owned<script>column</script>");
  await expect(panel.locator("script")).toHaveCount(0);
  await panel.screenshot({ path: info.outputPath("columnstore-desktop.png") });
  await page.getByRole("button", { name: "Hide tables ( [ )", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await panel.scrollIntoViewIfNeeded();
  await panel.screenshot({ path: info.outputPath("columnstore-narrow.png") });
  expect(await panel.evaluate((node) => node.clientWidth > 80 && node.scrollWidth <= node.clientWidth + 1)).toBe(true);
});
