import { type Page, expect } from "@playwright/test";
import type { State } from "../src/lib/store";

/** Open the demo and one of its connections, by name, from the sidebar. */
export async function openConnection(page: Page, name: string) {
  await page.goto("/");
  await page.getByRole("complementary").getByRole("button", { name: new RegExp(` ${name} \\d+$`) }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Dashboard");
}

/** Fixture injection owns a settled report so the initial background scan cannot replace its evidence. */
export async function waitForReport(page: Page) {
  await expect.poll(async () => page.evaluate(async (path) => {
    const { getState } = await import(path) as { getState: () => State };
    const report = getState().report;
    return report.status === "ready" && !report.scanning;
  }, "/src/lib/store.ts")).toBe(true);
}

/** The open section, by its title. */
export function section(page: Page) {
  return page.getByRole("heading", { level: 1 });
}
