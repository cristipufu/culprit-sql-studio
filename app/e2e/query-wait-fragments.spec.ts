import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

async function setWaitHistory(page: Page, unavailable: boolean) {
  await page.evaluate(async ({ storePath, fixturePath, unavailable, words }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { queryWaitFragmentReport } = await import(fixturePath) as typeof import("../src/test/query-wait-fragments");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("A settled demo report is required");
    const report = structuredClone(current.report);
    const history = queryWaitFragmentReport(unavailable ? 0 : 12);
    if (unavailable) {
      history.source_state = "unavailable";
      history.selected_rows = null;
      history.summary = words["view.query_wait_fragments.unavailable"];
      history.source_reason = words["view.query_wait_fragments.sharing_withheld"];
    }
    report.query_wait_fragments = history;
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/query-wait-fragments.ts", unavailable, words: english });
}

test("original wait fragments fold, page with separate outcomes and retain unavailable history", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await setWaitHistory(page, false);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.query_wait_fragments.title"], exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await region.scrollIntoViewIfNeeded();
  await page.screenshot({ path: info.outputPath("query-wait-fragments-folded-desktop.png") });
  const observations = region.getByText(english["ui.retained_failures.observations"], { exact: true });
  await observations.focus();
  await page.keyboard.press("Enter");
  await expect(region.locator("article")).toHaveCount(10);
  await expect(region.getByText("9,007,199,254,740,993 ms", { exact: true }).first()).toBeVisible();
  await expect(region.getByText("Regular", { exact: true }).first()).toBeVisible();
  await expect(region.getByText("Aborted", { exact: true }).first()).toBeVisible();
  await expect(region.locator(".sig-dot, .sev")).toHaveCount(0);
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(region.locator("article")).toHaveCount(2);
  await expect(region.getByRole("heading", { name: "Native category 10", exact: true })).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  await region.getByRole("heading", { name: "Native category 10", exact: true }).scrollIntoViewIfNeeded();
  expect(await region.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("query-wait-fragments-paged-narrow.png") });
  await setWaitHistory(page, true);
  await expect(region).toContainText(english["view.query_wait_fragments.unavailable"]);
  await expect(region.locator("article")).toHaveCount(0);
  await expect(page.getByText(english["ui.health.empty"], { exact: true })).toHaveCount(0);
  await region.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(region).toContainText(english["view.query_wait_fragments.sharing_withheld"]);
});
