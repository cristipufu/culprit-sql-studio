import { readFileSync } from "node:fs";
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

// Every catalog the build found, each read as the app shows it: the sidebar's own words come from that file.
const catalog = (code: string) => JSON.parse(readFileSync(new URL(`../../locales/${code}.json`, import.meta.url), "utf8")) as Record<string, string>;

for (const code of ["de", "es", "fr"]) {
  test(`The app reads in ${code} once it is picked`, async ({ page }) => {
    const words = catalog(code);
    await openConnection(page, "pg-demo");
    await page.getByRole("complementary").getByRole("combobox").selectOption(code);
    const sidebar = page.getByRole("complementary");
    const startsWith = (text: string) => new RegExp(`^${text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`);
    await expect(sidebar.getByRole("combobox", { name: words["ui.sidebar.language"] })).toBeVisible();
    await expect(sidebar.getByRole("button", { name: startsWith(words["ui.common.view.findings"]!) })).toBeVisible();
    await expect(sidebar.getByRole("button", { name: startsWith(catalog("en")["ui.common.view.findings"]!) })).toHaveCount(0);
  });
}
