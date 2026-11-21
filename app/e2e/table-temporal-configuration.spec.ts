import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import desktop from "../src-tauri/tauri.conf.json" with { type: "json" };
import type { MssqlTableTemporalConfiguration } from "../src/gen/MssqlTableTemporalConfiguration";
import { openConnection, waitForReport } from "./demo";

function configuration(infinite: boolean): MssqlTableTemporalConfiguration {
  return {
    temporal_type_availability: "available", temporal_type: 2,
    temporal_type_desc_availability: "available", temporal_type_desc: "SYSTEM_VERSIONED_TEMPORAL_TABLE",
    history_table_id_availability: "available", history_table_id: 123,
    history_retention_period_availability: "available", history_retention_period: infinite ? -1 : 6,
    history_retention_period_unit_availability: "available", history_retention_period_unit: infinite ? -1 : 5,
    history_retention_period_unit_desc_availability: "available", history_retention_period_unit_desc: infinite ? "INFINITE" : "MONTH",
  };
}

async function setConfiguration(page: Page, configuration: MssqlTableTemporalConfiguration | null) {
  await page.evaluate(async ({ path, configuration, words }) => {
    const { getState, setState } = await import(path) as typeof import("../src/lib/store");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("The demo report must be ready");
    const report = structuredClone(current.report);
    const table = report.snapshot.tables[0]!;
    table.mssql_temporal_configuration = configuration;
    const fields = [
      ["view.table_temporal.field.type", "2"],
      ["view.table_temporal.field.type_desc", "SYSTEM_VERSIONED_TEMPORAL_TABLE"],
      ["view.table_temporal.field.history_id", "123"],
      ["view.table_temporal.field.retention_period", String(configuration?.history_retention_period)],
      ["view.table_temporal.field.retention_unit", String(configuration?.history_retention_period_unit)],
      ["view.table_temporal.field.retention_unit_desc", configuration?.history_retention_period_unit_desc ?? ""],
    ] as const;
    report.table_temporal = configuration ? {
      title: words["view.table_temporal.title"], summary: "One captured table configuration",
      source_state: report.snapshot.sources?.find((source) => source.source === "tables")?.state ?? null,
      source_reason: null, observed_from: null, observed_until: null, native_rows: null,
      observed_rows: 1, read_complete: null, qualification: words["view.table_temporal.qualification"],
      limitations: [words["view.table_temporal.limit.history"]],
      entries: [{ schema: table.schema, table: table.name, observation_key: null,
        fields: fields.map(([key, text]) => ({ key, label: words[key], text, availability: "available" })),
      }],
    } : undefined;
    setState({ report: { ...current, report }, view: "schema", advisor: false, schemaTable: `${table.schema}.${table.name}` });
  }, { path: "/src/lib/store.ts", configuration, words: english });
}

test("temporal configuration folds, supports the keyboard and resets after captured values change", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await setConfiguration(page, configuration(false));
  const panel = page.locator(".td-head details.td-chunks").filter({
    has: page.getByText(english["view.table_temporal.title"], { exact: true }),
  });
  await expect(panel).toBeVisible();
  await expect(panel).not.toHaveAttribute("open");
  expect((await panel.boundingBox())!.height).toBeLessThan(48);
  await panel.locator(":scope > summary").focus();
  await page.keyboard.press("Enter");
  await expect(panel.locator("dd")).toHaveText(["2", "SYSTEM_VERSIONED_TEMPORAL_TABLE", "123", "6", "5", "MONTH"]);
  await expect(panel.getByText(english["view.table_temporal.qualification"], { exact: true })).toBeVisible();
  await expect(panel.getByText(english["view.table_temporal.limit.history"], { exact: true })).toBeVisible();
  await expect(panel.locator(".sev")).toHaveCount(0);
  await page.screenshot({ path: info.outputPath("table-temporal-desktop.png") });
  await page.setViewportSize({ width: desktop.app.windows[0]!.minWidth, height: 844 });
  await panel.scrollIntoViewIfNeeded();
  expect(await panel.evaluate((node) => node.clientWidth > 300 && node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("table-temporal-narrow.png") });
  await setConfiguration(page, configuration(true));
  await expect(panel).not.toHaveAttribute("open");
  await panel.locator(":scope > summary").click();
  await expect(panel.locator("dd")).toHaveText(["2", "SYSTEM_VERSIONED_TEMPORAL_TABLE", "123", "-1", "-1", "INFINITE"]);
  await setConfiguration(page, null);
  await expect(panel).toHaveCount(0);
});
