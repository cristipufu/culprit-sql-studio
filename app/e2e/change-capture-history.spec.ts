import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

async function setHistory(page: Page, withdrawn = false) {
  await page.evaluate(async ({ storePath, fixturePath, withdrawn }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { cdcHistoryCase, changeFeedHistoryCase } = await import(fixturePath) as typeof import("../src/test/change-capture");
    const current = getState().report;if (current.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(current.report);
    report.cdc = [cdcHistoryCase(withdrawn ? "withdrawn" : "cdc_exact")];
    report.change_feed = withdrawn ? [] : [changeFeedHistoryCase("change_feed_many")];
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/change-capture.ts", withdrawn });
}

test("independent original histories fold, page by keyboard and remain bounded on narrow screens", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");await waitForReport(page);await setHistory(page);
  await page.locator('[data-tile="health"]').click();
  const cdc = page.getByRole("region", { name: english["view.change_capture.history.cdc"], exact: true });
  const feed = page.getByRole("region", { name: english["view.change_capture.history.change_feed"], exact: true });
  await expect(cdc).toBeVisible();await expect(feed).toBeVisible();await expect(feed.locator("details[open]")).toHaveCount(0);
  await feed.scrollIntoViewIfNeeded();
  await page.screenshot({ path: info.outputPath("change-capture-history-folded-desktop.png") });
  const captures = feed.locator(":scope > details").last().locator(":scope > summary");await captures.focus();await page.keyboard.press("Enter");
  const original = feed.getByRole("region", { name: english["view.change_feed.title"], exact: true });
  const entries = original.locator(":scope > details").last().locator(":scope > summary");await entries.focus();await page.keyboard.press("Space");
  await expect(original.locator("article.change-capture-entry")).toHaveCount(10);
  await original.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(original.locator("article.change-capture-entry")).toHaveCount(5);
  await expect(original.getByText("9007199254740993", { exact: true }).first()).toBeVisible();
  await expect(original.locator(".sig-dot, .sev")).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });await original.scrollIntoViewIfNeeded();
  expect(await original.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("change-capture-history-paged-narrow.png") });
  await setHistory(page, true);
  await expect(cdc).toContainText(english["view.change_capture.history.sharing_withheld"]);
  await expect(page.getByText(english["ui.health.empty"], { exact: true })).toHaveCount(0);
});
