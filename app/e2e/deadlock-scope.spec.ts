import { expect, test, type Page } from "@playwright/test";
import samples from "../../fixtures/deadlocks.sample.json" with { type: "json" };
import english from "../../locales/en.json" with { type: "json" };
import type { DeadlockReport } from "../src/gen/DeadlockReport";
import type { Api } from "../src/lib/api";
import type { State } from "../src/lib/store";
import { fromRust } from "../src/test/fixtures";
import { openConnection, waitForReport } from "./demo";

async function installHistory(page: Page, history: DeadlockReport) {
  await page.evaluate(async ({ apiPath, storePath, history }) => {
    const client = await (await import(apiPath)).api() as Pick<Api, "deadlocks">;
    client.deadlocks = async () => history;
    const { getState, setState } = await import(storePath) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A settled fixture report is required");
    const report = structuredClone(state.report.report);
    report.snapshot.deadlocks = history;
    setState({ report: { ...state.report, report }, view: "live" });
  }, { apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts", history });
}

test("shared incidents remain readable without a database badge or a healthy empty-history claim", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  const history = fromRust<DeadlockReport>(samples.mssql);
  await installHistory(page, history);
  const tab = page.getByRole("tab", { name: english["ui.live.tab.deadlocks"], exact: true });
  await expect(tab.locator(".tab-count")).toHaveCount(0);
  await tab.click();
  const view = page.locator(".dl-view");
  await expect(view.getByRole("status")).toContainText(english["evidence.deadlocks.database_scope"]);
  await expect(view.getByRole("option")).toHaveCount(history.deadlocks.length);
  await expect(view.locator(".dl-detail")).toContainText(history.deadlocks[0]!.headline);
  await view.screenshot({ path: info.outputPath("shared-deadlock-history.png") });

  await installHistory(page, { ...history, scope: null, deadlocks: [], counted: 72 });
  await page.getByRole("radio", { name: english["ui.deadlocks.range.week"], exact: true }).click();
  await expect(view.locator(".dl-empty")).toContainText(english["evidence.deadlocks.database_scope"]);
  await expect(view.locator(".dl-ok, .dl-counter, .dl-list")).toHaveCount(0);
  await page.setViewportSize({ width: 620, height: 800 });
  await view.screenshot({ path: info.outputPath("unresolved-deadlock-history-narrow.png") });
});

test("a database counter without graphs reports its native count and a later counted zero", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await waitForReport(page);
  const history = fromRust<DeadlockReport>(samples.postgres);
  history.counted = 72;
  await installHistory(page, history);
  await page.getByRole("tab", { name: english["ui.live.tab.deadlocks"], exact: true }).click();
  const view = page.locator(".dl-view");
  await expect(view.locator(".dl-counter-value")).toHaveText("72");
  await expect(view.locator(".dl-ok, .dl-list")).toHaveCount(0);
  await expect(view).not.toContainText(english["evidence.deadlocks.database_scope"]);

  await installHistory(page, { ...history, counted: 0 });
  await page.getByRole("tab", { name: english["ui.live.tab.sessions"], exact: true }).click();
  await page.getByRole("tab", { name: english["ui.live.tab.deadlocks"], exact: true }).click();
  await expect(view.locator(".dl-ok")).toBeVisible();
  await expect(view.locator(".dl-counter, .dl-list")).toHaveCount(0);
});
