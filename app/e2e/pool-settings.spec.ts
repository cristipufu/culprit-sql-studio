import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

test("native resource-pool settings disclose separate membership, unknown values and safe text on a narrow viewport", async ({ page }, info) => {
  await openConnection(page, "mssql-demo"); await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { poolSettingsReport } = await import(fixturePath) as typeof import("../src/test/pool-settings");
    const current = getState().report; if (current.status !== "ready") throw new Error("owned demo report required");
    const report = structuredClone(current.report); report.pool_settings = poolSettingsReport(); report.pool_settings.fields[1]!.explanation = "<img src=x onerror=alert(1)>🌿";
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/pool-settings.ts" });
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.pool_settings.title"], exact: true });
  await expect(region).toBeVisible(); await expect(region.locator("details[open]")).toHaveCount(0); await expect(region.getByText("recorded-elastic-pool", { exact: true })).toHaveCount(0);
  await region.locator(":scope > details > summary").click();
  await expect(region.getByText("recorded-elastic-pool", { exact: true })).toBeVisible(); await expect(region.getByText("2026-11-01T00:00:02Z", { exact: true })).toBeVisible();
  await expect(region.locator("article")).toHaveCount(6); await expect(region.getByText("0%", { exact: true })).toBeVisible(); await expect(region.getByText(english["ui.memory_failures.not_reported"], { exact: true })).toBeVisible();
  await expect(region.getByText("<img src=x onerror=alert(1)>🌿", { exact: true })).toBeVisible(); await expect(region.locator("img,script")).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  await region.locator(":scope > details > summary").scrollIntoViewIfNeeded();
  await page.screenshot({ path: info.outputPath("pool-settings-narrow.png") });
  expect(await region.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.evaluate(async ({ storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { databasePoolSettingsReport } = await import(fixturePath) as typeof import("../src/test/pool-settings");
    const current = getState().report; if (current.status !== "ready") throw new Error("owned demo report required");
    const report = structuredClone(current.report); report.pool_settings = databasePoolSettingsReport("unavailable");
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/pool-settings.ts" });
  await expect(region.getByText(english["view.pool_settings.summary.membership_unavailable"], { exact: true })).toBeVisible();
  await expect(region.getByText("recorded-elastic-pool", { exact: true })).toHaveCount(0);
  await expect(region.locator("article")).toHaveCount(6);
});
