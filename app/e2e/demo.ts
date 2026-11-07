import { type Page, expect } from "@playwright/test";

/** Open the demo and one of its connections, by name, from the sidebar. */
export async function openConnection(page: Page, name: string) {
  await page.goto("/");
  await page.getByRole("complementary").getByRole("button", { name: new RegExp(` ${name} \\d+$`) }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Dashboard");
}

/** The open section, by its title. */
export function section(page: Page) {
  return page.getByRole("heading", { level: 1 });
}
