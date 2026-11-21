import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

test("historical Galera captures page and disclose canonical native facts without current-scan joins", async ({ page }, info) => {
  await openConnection(page, "mysql-demo"); await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const { galeraReport } = await import(fixturePath) as typeof import("../src/test/galera");
    const current = getState().report; if (current.status !== "ready") throw new Error("owned demo report required");
    const report = structuredClone(current.report); report.galera = Array.from({ length: 12 }, (_, i) => galeraReport(i));
    report.galera[0]!.entries[0]!.fields.push({ key: "native-comment", label: "Native comment", value: "<img src=x onerror=alert(1)>🌿" });
    setState({ report: { ...current, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/galera.ts" });
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.galera.title"], exact: true }).first();
  await expect(region).toBeVisible(); await expect(region.locator("details[open]")).toHaveCount(0);
  await region.locator(":scope > details").last().locator(":scope > summary").click();
  const card = region.locator("article.operational-card").first();
  await expect(card.locator(".replication-fields")).toHaveCount(0);
  const members = card.getByRole("region", { name: english["view.galera.title"], exact: true });
  await members.locator(":scope > details").last().locator(":scope > summary").click();
  await members.locator("summary").filter({ hasText: english["view.galera.node_title"] }).click();
  await expect(members.getByText("native-state-0", { exact: true })).toBeVisible();
  await expect(members.getByText("<img src=x onerror=alert(1)>🌿", { exact: true })).toBeVisible();
  await expect(members.locator("img, script")).toHaveCount(0);
  await members.locator("summary").filter({ hasText: english["view.galera.totals"] }).click();
  await expect(members.getByText("9007199254740993", { exact: true })).toBeVisible();
  expect(await region.innerHTML()).not.toContain("private-galera-process");
  await page.setViewportSize({ width: 390, height: 844 }); await card.screenshot({ path: info.outputPath("galera-narrow.png") });
  expect(await card.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(region.locator("article.operational-card")).toHaveCount(2);
  const next = region.locator("article.operational-card").first();
  await next.locator(":scope > section > details").last().locator(":scope > summary").click();
  await next.locator("summary").filter({ hasText: english["view.galera.node_title"] }).click();
  await expect(next.getByText("native-state-10", { exact: true })).toBeVisible();
});
