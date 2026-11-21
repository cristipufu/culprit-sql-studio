import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

async function setConfiguration(page: Page, mode: "health" | "schema") {
  await page.evaluate(async ({ storePath, fixturePath, mode }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { selectedIndexConfiguration } = await import(fixturePath) as typeof import("../src/test/index-configuration");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(current.report);
    const { report: fixture, index: selected } = selectedIndexConfiguration();
    const view = fixture.index_configuration[0]!;
    const entry = view.entries[0]!;
    const index = report.snapshot.indexes[0]!;
    const native = selected.mssql_configuration!;
    native.parent_schema = entry.parent_schema = index.schema;
    native.parent_name = entry.parent_name = index.table;
    native.name = entry.index_name = index.name;
    index.mssql_configuration = native;
    if (mode === "health") {
      view.entries = Array.from({ length: 12 }, (_, position) => ({
        ...structuredClone(entry), index_id: entry.index_id + position,
        index_name: position === 0 ? entry.index_name : `${entry.index_name}_${position}`,
      }));
      view.observed_rows = view.presented_rows = view.entries.length;
    }
    report.index_configuration = [view];
    report.snapshot.sources = [
      ...(report.snapshot.sources ?? []).filter((source) => source.source !== "indexes"),
      { source: "indexes", state: "partial", reason: null },
    ];
    setState({ report: { ...current, report }, view: mode === "schema" ? "schema" : "dashboard", advisor: false, schemaTable: `${index.schema}.${index.table}` });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/index-configuration.ts", mode });
}

test("Health folds and pages parents, and preserves exact individual partition values on narrow screens", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await setConfiguration(page, "health");
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.index_configuration.title"], exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await region.scrollIntoViewIfNeeded();
  await page.screenshot({ path: info.outputPath("index-configuration-folded-desktop.png") });
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(region.locator("article")).toHaveCount(10);
  const first = region.locator("article").first();
  await first.getByText(english["ui.index_configuration.title"], { exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(first.getByText("80", { exact: true })).toBeVisible();
  const partition = first.getByText("Partition 1 · ID 9007199254740993", { exact: true });
  await partition.focus();
  await page.keyboard.press("Enter");
  await expect(first.getByText("ROW (1)", { exact: true })).toBeVisible();
  await expect(first.getByText("9007199254741993", { exact: true })).toBeVisible();
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(region.locator("article")).toHaveCount(2);
  await page.setViewportSize({ width: 390, height: 844 });
  await region.locator("article").first().scrollIntoViewIfNeeded();
  expect(await region.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("index-configuration-paged-narrow.png") });
});

test("Schema discloses the same qualified values and withdraws a changed native generation", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await setConfiguration(page, "schema");
  await page.getByRole("tab", { name: /^Indexes/ }).click();
  const summary = page.getByText(english["ui.index_configuration.title"], { exact: true });
  await expect(summary).toBeVisible();
  const panel = summary.locator("..");
  await expect(panel).not.toHaveAttribute("open");
  await summary.focus();
  await page.keyboard.press("Enter");
  await expect(panel.getByText(english["view.index_configuration.qualification"], { exact: true })).toBeVisible();
  await expect(panel.getByText("80", { exact: true })).toBeVisible();
  await panel.screenshot({ path: info.outputPath("index-configuration-schema.png") });
  await page.evaluate(async (path) => {
    const { getState, setState } = await import(path) as typeof import("../src/lib/store");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(current.report);
    report.snapshot.indexes[0]!.mssql_configuration!.parent_object.created_without_offset = "2000-01-01T00:00:00.0000000";
    setState({ report: { ...current, report } });
  }, "/src/lib/store.ts");
  await expect(summary).toHaveCount(0);
});
