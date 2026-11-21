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


test("Theme colors follow system and explicit choices with the original palettes", async ({ page }) => {
  await page.goto("/");
  for (const system of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: system });
    for (const choice of [
      { label: "Follow the system", theme: "system" },
      { label: "Light", theme: "light" },
      { label: "Dark", theme: "dark" },
    ] as const) {
      await page.getByRole("radio", { name: choice.label, exact: true }).check();
      const dark = choice.theme === "dark" || (choice.theme === "system" && system === "dark");
      if (choice.theme === "system") await expect(page.locator("html")).not.toHaveAttribute("data-theme");
      else await expect(page.locator("html")).toHaveAttribute("data-theme", choice.theme);
      await expect.poll(() => page.evaluate(() => {
        const root = getComputedStyle(document.documentElement);
        return {
          background: getComputedStyle(document.body).backgroundColor,
          colorScheme: root.colorScheme,
          mutedText: root.getPropertyValue("--text-3").trim(),
          codeComment: root.getPropertyValue("--code-comment").trim(),
          highText: root.getPropertyValue("--high-text").trim(),
          waitCpu: root.getPropertyValue("--wait-cpu").trim(),
          planTint: root.getPropertyValue("--tint-0").trim(),
        };
      })).toEqual(dark ? {
        background: "rgb(18, 18, 17)", colorScheme: "dark", mutedText: "#8f8c86", codeComment: "#8a8781", highText: "#f0676b", waitCpu: "#7aa3d6", planTint: "#1c2944",
      } : {
        background: "rgb(250, 250, 249)", colorScheme: "light", mutedText: "#6b6862", codeComment: "#726f68", highText: "#b4232b", waitCpu: "#4e79a7", planTint: "#e8effd",
      });
    }
  }
});
