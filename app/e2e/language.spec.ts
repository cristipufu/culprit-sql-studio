import { expect, test } from "@playwright/test";
import { openConnection } from "./demo";

test("The language picked in the sidebar is the page's, and stays across a reload", async ({ page }) => {
  await openConnection(page, "pg-demo");
  const picker = page.getByRole("complementary").getByRole("combobox");
  await expect(picker).toHaveValue("en");
  await expect(picker.locator("option")).toHaveText(["English", "Deutsch", "Español", "Français"]);
  await picker.selectOption("es");
  await expect(page.locator("html")).toHaveAttribute("lang", "es");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("lang", "es");
  await expect(page.getByRole("complementary").getByRole("combobox")).toHaveValue("es");
});
