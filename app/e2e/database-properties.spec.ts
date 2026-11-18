import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

test("properties stay compact until disclosed and preserve partial unknown native codes", async ({ page }, testInfo) => {
  await openConnection(page, "mssql-demo"); await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { databasePropertiesHealthReport } = await import(fixturePath);
    const state = getState(); if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report); const fixture = databasePropertiesHealthReport();
    report.database_properties = fixture.database_properties; report.snapshot.sources = fixture.snapshot.sources;
    report.database_properties!.fields[0]!.native_description = "<img src=x onerror=alert(1)>";
    setState({ report: { ...state.report, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/database-properties.ts" });
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.database_properties.title"], exact: true });
  await expect(region).toBeVisible(); await expect(region.locator("article")).toHaveCount(0);
  await region.screenshot({ path: testInfo.outputPath("properties-folded.png") });
  await region.getByText(english["view.database_properties.details"], { exact: true }).click();
  await expect(region.locator("article")).toHaveCount(4);
  await expect(region.getByText("255", { exact: true })).toBeVisible();
  await expect(region.getByText("0", { exact: true })).toBeVisible();
  await expect(region.getByText("2020-01-01T12:00:00.0000001Z", { exact: true })).toBeVisible();
  await expect(region.getByText(english["ui.memory_failures.not_reported"], { exact: true })).toBeVisible();
  await expect(region.locator("img,script,a")).toHaveCount(0);
  await page.setViewportSize({ width: 520, height: 900 });
  await expect(region).toBeVisible();
  expect(await region.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
  await region.screenshot({ path: testInfo.outputPath("properties-expanded-narrow.png") });
});
