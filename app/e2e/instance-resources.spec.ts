import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

test("native instance resource evidence is folded and shows exact independent fields on a narrow layout", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { instanceResourceCase } = await import(fixturePath);
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    report.instance_resources = instanceResourceCase("exact").report;
    setState({ report: { ...state.report, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/instance-resources.ts" });
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.instance_resources.title"], exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(region.locator("article")).toHaveCount(6);
  await expect(region.getByText("9007199254740993", { exact: true })).toBeVisible();
  await expect(region.getByText("0", { exact: true }).first()).toBeVisible();
  await expect(region.getByText(english["ui.memory_failures.not_reported"], { exact: true }).first()).toBeVisible();
  await expect(region.locator(".sig-dot")).toHaveCount(0);
  await page.setViewportSize({ width: 520, height: 900 });
  expect(await region.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
  await region.screenshot({ path: info.outputPath("instance-resources-expanded-narrow.png") });
});
