import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { Plan } from "../src/gen/Plan";
import { currentTypeSchema, typeSchemaPlan } from "../src/test/plan-type-schema";
import { openConnection, waitForReport } from "./demo";

async function openPlan(page: Page, plan: Plan) {
  await page.evaluate(async ({ path, plan }) => {
    const { addPlan } = await import(path) as typeof import("../src/lib/plans");
    addPlan(plan, "Current type-schema renderer fixture", "Renderer fixture");
  }, { path: "/src/lib/plans.ts", plan });
  await expect(page.locator(".plan-surface")).toHaveCount(1);
}

test("current type metadata stays folded, keyboard accessible and horizontally contained on narrow screens", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await openPlan(page, typeSchemaPlan());
  const panel = page.locator(".plan-type-schema");
  await expect(panel).not.toHaveAttribute("open");
  await expect(panel.locator(".plan-type-schema-body")).toBeHidden();
  expect((await panel.boundingBox())!.height).toBeLessThan(42);
  await panel.locator(":scope > summary").focus();
  await page.keyboard.press("Enter");
  await expect(panel.getByText(english["view.plan_type_schema.limit.current"], { exact: true })).toBeVisible();
  await expect(panel.getByRole("link", { name: "dbo.RowsType", exact: true })).toHaveCount(2);
  await expect(panel.locator(".plan-type-schema-columns").getByRole("cell", { name: "128", exact: true })).toBeVisible();
  await expect(panel.locator(".plan-type-schema-columns").getByRole("cell", { name: "-1", exact: true })).toBeVisible();
  await expect(panel.getByRole("cell", { name: "NONCLUSTERED HASH", exact: true })).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  await panel.scrollIntoViewIfNeeded();
  expect(await panel.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  expect(await panel.locator(".plan-type-schema-scroll").evaluateAll((nodes) => nodes.every((node) => node.clientWidth > 0 && node.scrollWidth > node.clientWidth))).toBe(true);
  await panel.screenshot({ path: info.outputPath("type-schema-narrow.png") });
  await panel.locator(":scope > summary").focus();
  await page.keyboard.press("Space");
  await expect(panel.locator(".plan-type-schema-body")).toBeHidden();

  const unavailable = currentTypeSchema();
  unavailable.status = "changed";
  unavailable.module = null;
  unavailable.parameters = [];
  unavailable.types = [];
  const changed = typeSchemaPlan(unavailable);
  changed.view!.current_type_schema!.summary = english["view.plan_type_schema.changed"];
  await openPlan(page, changed);
  await expect(panel).not.toHaveAttribute("open");
  await panel.locator(":scope > summary").click();
  await expect(panel.getByText(english["view.plan_type_schema.changed"], { exact: true })).toBeVisible();
  await expect(panel.locator("table")).toHaveCount(0);
  await openPlan(page, typeSchemaPlan(null));
  await expect(panel).toHaveCount(0);
});
