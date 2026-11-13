import { expect, test } from "@playwright/test";
import type { WaitsReport } from "../src/gen/WaitsReport";
import type { Api } from "../src/lib/api";
import { openConnection } from "./demo";

test.use({ viewport: { width: 1000, height: 900 } });

test("independent native wait totals remain visible without contribution percentages or clipped metadata", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  const waits: WaitsReport = {
    engine: "mssql", scope: "window", source: "Native counter scopes", total_ms: 17_700,
    categories: [{
      key: "parallelism", label: "Recorded parallel waits", wait_ms: 17_700, share: 1, top_wait: "native-workers",
      meaning: "The native category counter and query counter have separate scopes.", detail: null, attribution: null,
      queries: [{
        id: "query-1", text: "SELECT id FROM app.orders WHERE customer_id = @id", wait_ms: 120_000,
        mean_recorded_ms: 60_000, executions: 2, rows_per_run: 3,
      }],
    }],
    notes: ["Query totals are not asserted to be portions of the category counter."],
  };
  await page.evaluate(async ({ waits, path }) => {
    const client = await (await import(path)).api() as Api;
    client.waits = async () => waits;
  }, { waits, path: "/src/lib/api.ts" });
  await page.locator('[data-tile="waits"]').click();
  const row = page.locator(".wq-row");
  await expect(page.locator(".waits-row-num > span")).toHaveText("17.7 s");
  await expect(row.locator(".wq-time")).toHaveText("2 min");
  await expect(row.locator(".wq-meta")).toContainText("1 min accumulated per execution on average");
  await expect(row).not.toContainText("%");
  await expect(row).not.toContainText("of its time");

  for (const language of ["en", "es"]) {
    await page.getByRole("complementary").getByRole("combobox").selectOption(language);
    await expect(page.locator("html")).toHaveAttribute("lang", language);
    const metadata = row.locator(".wq-meta");
    await expect(metadata).toContainText(language === "en" ? "2 runs" : "2 ejecuciones");
    await expect(metadata).toHaveCSS("white-space", "normal");
    const bounds = await metadata.evaluate((element) => ({
      width: element.clientWidth, scroll: element.scrollWidth, height: element.getBoundingClientRect().height,
    }));
    expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
    expect(bounds.height).toBeGreaterThan(25);
    await row.screenshot({ path: info.outputPath(`wait-metadata-${language}.png`) });
  }
});
