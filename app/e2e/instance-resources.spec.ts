import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

async function openResourceEvidence(page: Page, name: "exact" | "cpu_exact" | "schedulers_limited") {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, name }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { instanceResourceCase } = await import(fixturePath);
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    report.instance_resources = instanceResourceCase(name).report;
    setState({ report: { ...state.report, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/instance-resources.ts", name });
  await page.locator('[data-tile="health"]').click();
}

test("native instance resource evidence is folded and shows exact independent fields on a narrow layout", async ({ page }, info) => {
  await openResourceEvidence(page, "exact");
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

test("process CPU totals keep their own folded point alongside the independent memory evidence", async ({ page }) => {
  await openResourceEvidence(page, "cpu_exact");
  const cpu = page.getByRole("region", { name: english["view.process_cpu.title"], exact: true });
  await expect(cpu).toBeVisible();
  await expect(cpu).toContainText("2020-01-01T12:00:00.5Z");
  await expect(cpu.locator("details[open]")).toHaveCount(0);
  await cpu.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(cpu.getByText("9007199254740993", { exact: true })).toBeVisible();
  await expect(cpu.locator(".sig-dot")).toHaveCount(0);
  await expect(page.getByRole("region", { name: english["view.instance_resources.title"], exact: true })).toBeVisible();
});


test("individual scheduler rows use the folded paged observation surface on a narrow viewport", async ({ page }, info) => {
  await openResourceEvidence(page, "schedulers_limited");
  const region = page.getByRole("region", { name: english["view.schedulers.title"], exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(region.locator("article")).toHaveCount(10);
  await expect(region.getByRole("heading", { name: "Scheduler 0", exact: true })).toBeVisible();
  await expect(region.getByText("work_queue_count", { exact: true }).first()).toBeVisible();
  await expect(region.getByText("queued_disk_io_count", { exact: true }).first()).toBeVisible();
  await expect(region.getByText("spinlock_wait_count", { exact: true }).first()).toBeVisible();
  await expect(region.getByText(
    english["view.schedulers.counter.instrumentation"].replace("{flag}", "8134"),
    { exact: true },
  ).first()).toBeVisible();
  await expect(region.getByText("9007199254740993", { exact: true }).first()).toBeVisible();
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(region.getByRole("heading", { name: "Scheduler 10", exact: true })).toBeVisible();
  await expect(region.getByRole("heading", { name: "Scheduler 0", exact: true })).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await region.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
  await region.getByRole("heading", { name: "Scheduler 10", exact: true }).scrollIntoViewIfNeeded();
  await page.screenshot({ path: info.outputPath("native-schedulers-paged-narrow.png") });
});
