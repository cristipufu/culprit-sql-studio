import { expect, test } from "@playwright/test";
import { openConnection, section } from "./demo";

test("A sample plan draws its steps, a step explains itself, and the view tabs follow the arrow keys", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await page.keyboard.press("7");
  await expect(section(page)).toHaveText("Plans");

  await page.getByRole("button", { name: "PostgreSQL PostgreSQL", exact: true }).click();
  await page.getByRole("button", { name: "One plan" }).click();
  const main = page.getByRole("main");
  await expect(main.getByRole("heading", { level: 2, name: /^Plan \d+$/ })).toBeVisible();

  const assessment = main.locator(".ins-context");
  await expect(assessment.locator("summary")).toHaveText("Assessment notes");
  await expect(assessment.locator(".ins-note")).toBeHidden();
  await assessment.locator("summary").focus();
  await page.keyboard.press("Enter");
  await expect(assessment.locator(".ins-note")).toBeVisible();
  await page.keyboard.press("Space");
  await expect(assessment.locator(".ins-note")).toBeHidden();

  await main.getByRole("button", { name: "Seq Scan on orders, 43% of the work" }).click();
  await expect(main.getByRole("heading", { name: "What it does" })).toBeVisible();
  await expect(main.getByRole("code").filter({ hasText: "(status = 'shipped'::text)" })).toBeVisible();

  const views = main.getByRole("tablist", { name: "Plan view" });
  await views.getByRole("tab", { name: "Diagram" }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(views.getByRole("tab", { name: "Tree" })).toHaveAttribute("aria-selected", "true");
  await expect(views.getByRole("tab", { name: "Tree" })).toBeFocused();
});
