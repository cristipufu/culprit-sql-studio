import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { Plan } from "../src/gen/Plan";
import { pageServerDisplay, pageServerPlan } from "../src/test/page-server-plan";
import { openConnection, waitForReport } from "./demo";

async function openPlan(page: Page, plan: Plan) {
  await page.evaluate(async ({ path, plan }) => {
    const { addPlan } = await import(path) as typeof import("../src/lib/plans");
    addPlan(plan, "Recorded page-server display", "Renderer fixture");
  }, { path: "/src/lib/plans.ts", plan });
  await page.getByRole("tablist", { name: "Plan view" }).getByRole("tab", { name: "Operations" }).click();
  await expect(page.locator(".plan-surface .nd")).toHaveCount(1);
}

test("recorded page-server counters stay folded, exact and independent on a narrow layout", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await openPlan(page, pageServerPlan());
  const panel = page.locator(".nd-page-server");
  await expect(panel).toHaveCount(1);
  await expect(panel).not.toHaveAttribute("open");
  await expect(panel.locator(".nd-page-server-body")).toBeHidden();
  expect((await panel.boundingBox())!.height).toBeLessThan(40);
  await panel.screenshot({ path: info.outputPath("page-server-folded.png") });
  await panel.locator(":scope > summary").focus();
  await page.keyboard.press("Enter");
  await expect(panel.locator("dd")).toHaveCount(4);
  await expect(panel.getByText("9007199254740995", { exact: true })).toBeVisible();
  await expect(panel).not.toContainText("9,007,199,254,740,995");
  await expect(panel).not.toContainText("9007199254740996");
  await expect(panel.getByText(english["view.plan_page_server_io.limit.recorded"], { exact: true })).toBeVisible();
  await panel.screenshot({ path: info.outputPath("page-server-expanded.png") });
  await page.keyboard.press("Space");
  await expect(panel.locator(".nd-page-server-body")).toBeHidden();
  await page.keyboard.press("Enter");
  await expect(panel.locator(".nd-page-server-body")).toBeVisible();

  await openPlan(page, pageServerPlan(pageServerDisplay(true)));
  await expect(panel).not.toHaveAttribute("open");
  await panel.locator(":scope > summary").click();
  await expect(panel.locator("dd")).toHaveText(["Unknown", "0", "5", "Unknown"]);
  await expect(panel.getByText(english["view.plan_page_server_io.limit.partial"], { exact: true })).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(panel.locator("dd")).toHaveText(["Unknown", "0", "5", "Unknown"]);
  const [list, workspace] = await Promise.all([
    page.locator(".plans-list").boundingBox(),
    page.locator(".plans-main").boundingBox(),
  ]);
  expect(list).not.toBeNull();
  expect(workspace).not.toBeNull();
  expect(list!.y + list!.height).toBeLessThanOrEqual(workspace!.y + 1);
  expect(workspace!.width).toBeGreaterThan(0);
  expect(workspace!.x + workspace!.width).toBeLessThanOrEqual(391);
  expect(await panel.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await panel.screenshot({ path: info.outputPath("page-server-narrow-partial.png") });
  await panel.locator(":scope > summary").click();
  await panel.scrollIntoViewIfNeeded();
  await expect(panel).toBeInViewport();
  await page.screenshot({ path: info.outputPath("page-server-narrow-workspace.png") });

  await openPlan(page, pageServerPlan(null));
  await expect(page.locator(".nd-page-server")).toHaveCount(0);
});
