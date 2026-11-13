import { expect, test } from "@playwright/test";
import { openConnection, section } from "./demo";

test("A finding opens in place with its evidence, its fix and how to verify it", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await page.keyboard.press("2");
  await expect(section(page)).toHaveText("Findings");

  await page.getByRole("button", { name: /^orders is read in full to find rows by customer_id/ }).click();
  const main = page.getByRole("main");
  await expect(main.getByRole("heading", { name: "orders is read in full to find rows by customer_id" })).toBeVisible();
  for (const part of [/^Evidence/, /^Fix/, "How to verify"]) {
    await expect(main.getByRole("heading", { name: part })).toBeVisible();
  }
});

test("The filter narrows the findings as you type", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await page.keyboard.press("2");
  const largeResult = page.getByRole("button", { name: /^A read from orders returns/ });
  const fullRead = page.getByRole("button", { name: /^orders is read in full/ });
  await expect(largeResult).toBeVisible();
  await expect(fullRead).toBeVisible();

  await page.getByRole("main").getByPlaceholder("Filter").fill("returns");
  await expect(largeResult).toBeVisible();
  await expect(fullRead).toBeHidden();
});

test("Find a table narrows the tables as you type, and Enter opens the one chosen", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await page.keyboard.press("5");
  await expect(section(page)).toHaveText("Schema");

  const find = page.getByRole("combobox", { name: "Find a table" });
  await find.fill("cust");
  await expect(page.getByRole("listbox", { name: "Tables" }).getByRole("option")).toHaveCount(1);
  await find.press("Enter");
  await expect(page.getByRole("main").getByRole("heading", { name: "public.customers" })).toBeVisible();

  // Its detail tabs follow the arrow keys, Home and End, and only the chosen one is a Tab stop.
  const details = page.getByRole("tablist", { name: "Table details" });
  const tabs = details.getByRole("tab");
  const chosen = details.getByRole("tab", { selected: true });
  await chosen.focus();
  const first = await chosen.textContent();
  await page.keyboard.press("ArrowRight");
  await expect(chosen).not.toHaveText(first ?? "");
  await expect(chosen).toBeFocused();
  await page.keyboard.press("End");
  await expect(tabs.last()).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Home");
  await expect(tabs.first()).toHaveAttribute("aria-selected", "true");
  await expect(details.locator('[role="tab"][tabindex="0"]')).toHaveCount(1);
});
