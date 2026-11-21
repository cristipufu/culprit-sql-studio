import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

test("historical Group Replication captures page and disclose canonical native facts without current-scan joins", async ({ page }, info) => {
  await openConnection(page, "mysql-demo"); await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { groupReplicationReport } = await import(fixturePath) as typeof import("../src/test/group-replication");
    const current = getState().report; if (current.status !== "ready") throw new Error("owned demo report required");
    const report = structuredClone(current.report); report.group_replication = Array.from({ length: 12 }, (_, i) => groupReplicationReport(i));
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/group-replication.ts" });
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.group_replication.title"], exact: true }).first();
  await expect(region).toBeVisible(); await expect(region.locator("details[open]")).toHaveCount(0);
  await region.locator(":scope > details").last().locator(":scope > summary").click();
  const card = region.locator("article.operational-card").first(); await expect(card).toContainText("original-group-0");
  const members = card.getByRole("region", { name: english["view.group_replication.title"], exact: true });
  await members.locator(":scope > details").last().locator(":scope > summary").click();
  await members.locator("summary").filter({ hasText: english["view.group_replication.member_title"] }).click();
  await expect(members.getByText("SECONDARY", { exact: true })).toBeVisible();
  await members.locator("summary").filter({ hasText: english["view.group_replication.totals"] }).click();
  await expect(members.getByText("9007199254740993", { exact: true })).toBeVisible();
  expect(await region.innerHTML()).not.toContain("private-observer-process");
  await page.setViewportSize({ width: 390, height: 844 }); await card.screenshot({ path: info.outputPath("group-replication-narrow.png") });
  expect(await card.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
});
