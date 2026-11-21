import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

async function setHistory(page: Page, unavailable: boolean) {
  await page.evaluate(async ({ storePath, runtimePath, waitsPath, unavailable, words }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { runtimeFragmentReport } = await import(runtimePath) as typeof import("../src/test/runtime-fragments");
    const { queryWaitFragmentReport } = await import(waitsPath) as typeof import("../src/test/query-wait-fragments");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("A settled demo report is required");
    const report = structuredClone(current.report);
    const runtime = runtimeFragmentReport(unavailable ? 0 : 12);
    if (unavailable) {
      runtime.source_state = "unavailable";
      runtime.selected_rows = null;
      runtime.references_complete = null;
      runtime.collected_from = null;
      runtime.collected_until = null;
      runtime.summary = words["view.runtime_fragments.unavailable"];
      runtime.source_reason = words["view.runtime_fragments.sharing_withheld"];
      runtime.limitations = [runtime.source_reason];
    }
    report.runtime_fragments = runtime;
    report.query_wait_fragments = queryWaitFragmentReport(1);
    setState({ report: { ...current, report } });
  }, {
    storePath: "/src/lib/store.ts", runtimePath: "/src/test/runtime-fragments.ts",
    waitsPath: "/src/test/query-wait-fragments.ts", unavailable, words: english,
  });
}

test("runtime originals fold and page independently from waits, preserving exact values and unavailable context", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await setHistory(page, false);
  await page.locator('[data-tile="health"]').click();
  const runtime = page.getByRole("region", { name: english["view.runtime_fragments.title"], exact: true });
  const waits = page.getByRole("region", { name: english["view.query_wait_fragments.title"], exact: true });
  await expect(runtime).toBeVisible();
  await expect(waits).toBeVisible();
  await expect(runtime.locator("details[open]")).toHaveCount(0);
  await runtime.scrollIntoViewIfNeeded();
  await page.screenshot({ path: info.outputPath("runtime-fragments-folded-desktop.png") });
  await runtime.getByText(english["ui.retained_failures.observations"], { exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(runtime.locator("article")).toHaveCount(10);
  await expect(runtime.locator("article h4").first()).toContainText("9007199254740993");
  await runtime.getByText(english["view.runtime_fragments.metric.duration"], { exact: true }).first().focus();
  await page.keyboard.press("Enter");
  await expect(runtime.getByText("9007199254740993 µs", { exact: true }).first()).toBeVisible();
  await expect(runtime.getByText(english["view.runtime_fragments.unknown"], { exact: true }).first()).toBeVisible();
  await expect(waits.locator("details[open]")).toHaveCount(0);
  await runtime.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(runtime.locator("article")).toHaveCount(2);
  await page.setViewportSize({ width: 390, height: 844 });
  await runtime.locator("article").first().scrollIntoViewIfNeeded();
  expect(await runtime.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("runtime-fragments-paged-narrow.png") });
  await setHistory(page, true);
  await expect(runtime).toContainText(english["view.runtime_fragments.unavailable"]);
  await expect(runtime.locator("article")).toHaveCount(0);
  await expect(runtime).not.toContainText("2020-01-01");
  await expect(waits).toBeVisible();
  await expect(page.getByText(english["ui.health.empty"], { exact: true })).toHaveCount(0);
});
