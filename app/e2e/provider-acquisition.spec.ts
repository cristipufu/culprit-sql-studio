import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";
test("current acquisition disclosure is folded and paged separately from historical captures", async ({
  page,
}, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(
    async ({ storePath, fixturePath }) => {
      const { getState, setState } = (await import(storePath)) as typeof import("../src/lib/store");
      const { currentAcquisition } = (await import(fixturePath)) as typeof import("../src/test/provider-acquisition");
      const current = getState().report;
      if (current.status !== "ready") throw new Error("ready demo report");
      const report = structuredClone(current.report);
      report.provider_acquisition = currentAcquisition(12);
      setState({ report: { ...current, report } });
    },
    { storePath: "/src/lib/store.ts", fixturePath: "/src/test/provider-acquisition.ts" },
  );
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.provider_acquisition.title"], exact: true });
  const cards = region.locator("article.operational-card");
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(cards.first()).toBeVisible();
  await cards.first().locator("details > summary").first().click();
  await expect(cards.first()).toContainText("9007199254740993");
  await expect(cards.first()).toContainText("18446744073709551615");
  await page.setViewportSize({ width: 390, height: 844 });
  await cards.first().scrollIntoViewIfNeeded();
  expect(await cards.first().evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("current-acquisition-narrow.png") });
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(cards).toHaveCount(2);
  await expect(region).not.toContainText("manifest_sha256");
});
