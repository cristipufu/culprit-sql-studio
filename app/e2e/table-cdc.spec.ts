import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

test("captured CDC enrollment is folded and resets when the selected table observation changes", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  for (const tracked of [true, false, null]) {
    await page.evaluate(async ({ path, tracked }) => {
      const { getState, setState } = await import(path) as typeof import("../src/lib/store");
      const current = getState().report;
      if (current.status !== "ready") throw new Error("The owned demo report must be ready");
      const report = structuredClone(current.report);
      const table = report.snapshot.tables[0]!;
      table.mssql_tracked_by_cdc = tracked;
      setState({ report: { ...current, report }, view: "schema", advisor: false, schemaTable: `${table.schema}.${table.name}` });
    }, { path: "/src/lib/store.ts", tracked });
    const panel = page.locator(".td-head details.td-catalog").filter({ has: page.getByText(english["view.table_cdc.title"], { exact: true }) });
    await expect(panel).not.toHaveAttribute("open");
    await panel.locator(":scope > summary").focus();
    await page.keyboard.press("Enter");
    await expect(panel.locator("dd")).toHaveText(english[tracked === null ? "ui.indexes.columnstore.native_null" : tracked ? "ui.common.yes" : "ui.common.no"]);
    await expect(panel.getByText(english["view.table_cdc.qualification"], { exact: true })).toBeVisible();
    if (tracked === true) await panel.screenshot({ path: info.outputPath("table-cdc-desktop.png") });
  }
});
