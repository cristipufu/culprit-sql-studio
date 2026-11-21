import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

test("original CPU and memory node sources fold and page independently with narrow keyboard access", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { numaNodeReport } = await import(fixturePath) as typeof import("../src/test/numa-nodes");
    const current = getState().report;
    if (current.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(current.report);
    report.numa_nodes = [numaNodeReport("numa_limited")];
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/numa-nodes.ts" });
  await page.locator('[data-tile="health"]').click();
  const cpu = page.getByRole("region", { name: english["view.numa_nodes.cpu"], exact: true });
  const memory = page.getByRole("region", { name: english["view.numa_nodes.memory"], exact: true });
  await expect(cpu).toBeVisible();
  await expect(memory).toBeVisible();
  await expect(cpu.locator("details[open]")).toHaveCount(0);

  const rows = cpu.getByText(english["ui.retained_failures.observations"], { exact: true });
  await rows.focus();
  await page.keyboard.press("Enter");
  await expect(cpu.locator("article")).toHaveCount(10);
  await cpu.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(cpu.getByRole("heading", { name: "Node 10", exact: true })).toBeVisible();
  await expect(memory.locator("details[open]")).toHaveCount(0);

  await page.setViewportSize({ width: 390, height: 844 });
  await cpu.scrollIntoViewIfNeeded();
  expect(await cpu.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("numa-node-original-paged-narrow.png") });
});
