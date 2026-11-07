import { expect, test } from "@playwright/test";
import { openConnection, section } from "./demo";

test("Number keys move between a connection's sections, and type as text inside the editor", async ({ page }) => {
  await openConnection(page, "pg-demo");
  for (const [key, title] of [["2", "Findings"], ["3", "Queries"], ["5", "Schema"], ["1", "Dashboard"], ["6", "Editor"]] as const) {
    await page.keyboard.press(key);
    await expect(section(page)).toHaveText(title);
  }
  // Once the editor has focus, a digit is SQL, not a shortcut.
  await expect(page.locator(".cm-content")).toBeFocused();
  await page.keyboard.press("End");
  await page.keyboard.type(" 2");
  await expect(section(page)).toHaveText("Editor");
  await expect(page.locator(".cm-content")).toContainText(" 2");
});

test("⌘K searches sections and actions; Enter opens the choice and Escape closes it", async ({ page }) => {
  await openConnection(page, "pg-demo");
  const palette = page.getByRole("dialog", { name: "Command palette" });

  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette).toBeVisible();
  const search = palette.getByRole("combobox");
  await expect(search).toBeFocused();
  await search.fill("schema");
  await expect(palette.getByRole("option", { selected: true })).toContainText("Schema");
  await page.keyboard.press("Enter");
  await expect(palette).toBeHidden();
  await expect(section(page)).toHaveText("Schema");

  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(palette).toBeHidden();
  await expect(section(page)).toHaveText("Schema");
});

test("The theme applies at once and is remembered", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("radio", { name: "Dark" }).check();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.getByRole("radio", { name: "Dark" })).toBeChecked();
});
