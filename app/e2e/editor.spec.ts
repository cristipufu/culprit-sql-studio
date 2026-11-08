import { type Page, expect, test } from "@playwright/test";
import { openConnection, section } from "./demo";

async function openEditor(page: Page) {
  await openConnection(page, "pg-demo");
  await page.keyboard.press("6");
  await expect(section(page)).toHaveText("Editor");
  await expect(page.locator(".cm-content")).toBeFocused();
}

async function replaceSql(page: Page, sql: string) {
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.type(sql);
}

test("⌘↵ runs the query and its rows show in a grid", async ({ page }) => {
  await openEditor(page);
  await replaceSql(page, "SELECT * FROM public.customers LIMIT 5");
  await page.keyboard.press("ControlOrMeta+Enter");

  const grid = page.getByRole("grid", { name: "Query results" });
  await expect(grid).toBeVisible();
  await expect(grid.getByRole("columnheader", { name: "email" })).toBeVisible();
  // A header row and the five asked for.
  await expect(grid.getByRole("row")).toHaveCount(6);
  await expect(page.getByText("5 rows (first 5; more exist)")).toBeVisible();
});

test("A query the connection can't answer says so in place of rows, where a screen reader hears it", async ({ page }) => {
  await openEditor(page);
  await replaceSql(page, "SELECT * FROM public.no_such_table");
  await page.keyboard.press("ControlOrMeta+Enter");
  // The browser demo answers only reads of its own tables: anything else is "not available here", not a failure.
  await expect(page.getByRole("main").getByRole("status")).toBeVisible();
  await expect(page.getByRole("grid", { name: "Query results" })).toHaveCount(0);
});

test("Tabs open and close from the keyboard, and keep their SQL across a reload", async ({ page }) => {
  await openEditor(page);
  await replaceSql(page, "SELECT 1 AS kept");
  const tabs = page.getByRole("tab");

  await page.keyboard.press("ControlOrMeta+t");
  await expect(tabs).toHaveText(["Query 1", "Query 2"]);
  await expect(page.getByRole("tab", { name: "Query 2" })).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ControlOrMeta+Shift+w");
  await expect(tabs).toHaveText(["Query 1"]);
  await expect(page.locator(".cm-content")).toContainText("SELECT 1 AS kept");

  await page.reload();
  await page.getByRole("complementary").getByRole("button", { name: / pg-demo \d+$/ }).click();
  await page.keyboard.press("6");
  await expect(page.locator(".cm-content")).toContainText("SELECT 1 AS kept");
});

test("Each tab keeps its own text and undo history while another is shown", async ({ page }) => {
  await openEditor(page);
  await replaceSql(page, "SELECT 1 AS first");
  await page.keyboard.press("ControlOrMeta+t");
  await replaceSql(page, "SELECT 2 AS second");
  await page.keyboard.press("ControlOrMeta+Shift+[");
  await expect(page.getByRole("tab", { name: "Query 1" })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".cm-content")).toContainText("SELECT 1 AS first");
  // The kept state is the tab's own: undo there takes back that tab's typing, not the other tab's.
  await page.keyboard.press("ControlOrMeta+z");
  await expect(page.locator(".cm-content")).not.toContainText("SELECT 1 AS first");
  await page.keyboard.press("ControlOrMeta+Shift+]");
  await expect(page.locator(".cm-content")).toContainText("SELECT 2 AS second");
});
