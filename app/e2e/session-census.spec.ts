import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { Api } from "../src/lib/api";
import { openConnection, waitForReport } from "./demo";

test("idle session context remains folded and exposes exact values without a progress request on narrow layouts", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(async ({ fixturePath, apiPath, storePath }) => {
    const { sessionCensusLive } = await import(fixturePath);
    const client = await (await import(apiPath)).api() as Api;
    client.live = async () => sessionCensusLive("idle-connection");
    client.queryProgress = async () => { throw new Error("A quiet census must not request native progress"); };
    (await import(storePath)).setState({ view: "live" });
  }, { fixturePath: "/src/test/session-census.ts", apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts" });
  const census = page.locator("details.live-census");
  await expect(census).toBeVisible();
  await expect(census).not.toHaveAttribute("open");
  await expect(page.locator('tr[data-session="61"]')).toHaveCount(0);
  await page.getByLabel(english["ui.live.show_idle"]).check();
  const row = page.locator('tr[data-session="61"]');
  await expect(row).toBeVisible();
  await expect(row).not.toHaveAttribute("tabindex");
  await census.locator(":scope > summary").click();
  const region = census.getByRole("region", { name: english["view.session_census.title"], exact: true });
  await expect(region.locator("details[open]")).toHaveCount(0);
  await region.locator(":scope > details > summary").filter({ hasText: english["ui.retained_failures.observations"] }).click();
  const observations = region.locator(":scope > details > .observation-grid");
  await expect(observations.getByText("9007199254740993", { exact: true })).toBeVisible();
  await expect(observations.getByText("37dd5a75-5bb6-4487-89a7-a6b1b0d1ca12", { exact: true })).toBeVisible();
  await expect(observations.getByText("0", { exact: true }).first()).toBeVisible();
  await page.setViewportSize({ width: 520, height: 900 });
  expect(await census.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
  const heading = await census.locator(":scope > summary").boundingBox();
  const content = await region.boundingBox();
  expect(heading).not.toBeNull();
  expect(content).not.toBeNull();
  expect(content!.y).toBeGreaterThanOrEqual(heading!.y + heading!.height);
  await census.screenshot({ path: info.outputPath("session-census-expanded-narrow.png") });
});

test("two MARS requests remain separate rows and cannot select an arbitrary session-only progress target", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(async ({ fixturePath, apiPath, storePath }) => {
    const { sessionCensusLive } = await import(fixturePath);
    const client = await (await import(apiPath)).api() as Api;
    client.live = async () => sessionCensusLive("mars");
    (window as typeof window & { censusProgressCalls: number }).censusProgressCalls = 0;
    client.queryProgress = async () => {
      (window as typeof window & { censusProgressCalls: number }).censusProgressCalls++;
      throw new Error("MARS requires an exact request-capable progress endpoint");
    };
    (await import(storePath)).setState({ view: "live" });
  }, { fixturePath: "/src/test/session-census.ts", apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts" });
  const rows = page.locator('tr[data-session="61"]');
  await expect(rows).toHaveCount(2);
  for (const row of await rows.all()) {
    await expect(row).not.toHaveAttribute("tabindex");
    await row.click();
  }
  expect(await page.evaluate(() => (window as typeof window & { censusProgressCalls: number }).censusProgressCalls)).toBe(0);
  const census = page.locator("details.live-census");
  await census.locator(":scope > summary").click();
  const region = census.getByRole("region", { name: english["view.session_census.title"], exact: true });
  await region.locator(":scope > details > summary").filter({ hasText: english["ui.retained_failures.observations"] }).click();
  await expect(census.getByText("11111111-1111-1111-1111-111111111111", { exact: true })).toBeVisible();
  await expect(census.getByText("22222222-2222-2222-2222-222222222222", { exact: true })).toBeVisible();
  const groups = region.getByRole("region", { name: english["view.session_census.groups.title"], exact: true });
  await expect(groups).not.toBeVisible();
  await region.locator(":scope > details > summary").filter({ hasText: english["view.session_census.groups.title"] }).click();
  await expect(groups).toBeVisible();
  await groups.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(groups.locator("article")).toHaveCount(1);
  await expect(groups.getByText("9007199254740993", { exact: true })).toBeVisible();
  expect(await page.evaluate(() => (window as typeof window & { censusProgressCalls: number }).censusProgressCalls)).toBe(0);
});
