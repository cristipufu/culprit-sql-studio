import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import french from "../../locales/fr.json" with { type: "json" };
import type { PageServerReadReport } from "../src/gen/PageServerReadReport";
import type { Api } from "../src/lib/api";
import type { State } from "../src/lib/store";
import { EXACT_PAGE_SERVER_PAGES, pageServerReadReport, unavailablePageServerReads } from "../src/test/page-server-reads";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, view: PageServerReadReport | null) {
  await waitForReport(page);
  await page.evaluate(async ({ view, storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { pageServerReadHealthReport } = await import(fixturePath);
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    const fixture = pageServerReadHealthReport(view);
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.sources = fixture.snapshot.sources;
    report.page_server_reads = fixture.page_server_reads;
    setState({ report: { ...state.report, report } });
  }, { view, storePath: "/src/lib/store.ts", fixturePath: "/src/test/page-server-reads.ts" });
}

test("page-server history folds, pages exact Rust text and exposes no definition action", async ({ page }) => {
  await openConnection(page, "pg-demo");
  const report = pageServerReadReport();
  await inject(page, report);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: report.title, exact: true });
  await expect(region).toBeVisible();
  await expect(region).toContainText(report.summary);
  await expect(region).toContainText(english["ui.common.read_partial"]);
  await expect(region.locator("details[open]")).toHaveCount(0);
  const rows = region.locator(".observation-grid > article");
  await expect(rows).toHaveCount(10);
  await expect(rows.first()).not.toBeVisible();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(rows.first().getByText(EXACT_PAGE_SERVER_PAGES, { exact: true })).toHaveCount(2);
  await expect(region).not.toContainText("9007199254740992");
  await rows.first().getByText(english["ui.page_server_reads.identities"], { exact: true }).click();
  await expect(rows.first()).toContainText(english["view.page_server_reads.limit.definition_link"]);
  await expect(region.locator("a, img, script, .sig-dot")).toHaveCount(0);
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(rows).toHaveCount(2);
  await expect(rows.first().getByRole("heading", { level: 4 })).toHaveText(report.entries[10]!.explanation.title);
  await region.getByRole("button", { name: english["ui.termination_history.previous"], exact: true }).click();
  await expect(rows).toHaveCount(10);
});

test("known unavailable columns and an absent invalid carrier stay visible without zero observations", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await inject(page, unavailablePageServerReads());
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.page_server_reads.title"], exact: true });
  await expect(region).toContainText(english["view.page_server_reads.unavailable"]);
  await expect(region.locator("article")).toHaveCount(0);
  await inject(page, null);
  await expect(region).toContainText(english["ui.dashboard.status.unavailable"]);
  await region.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(region).toContainText(english["view.page_server_reads.invalid"]);
  await expect(page.getByRole("main")).not.toContainText(english["ui.health.empty"]);
});

test("language reread replaces derived words and preserves the exact historical values and selector", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await inject(page, pageServerReadReport(1));
  await page.locator('[data-tile="health"]').click();
  await page.evaluate(async ({ apiPath, fixturePath }) => {
    const client = await (await import(apiPath)).api() as Pick<Api, "latestReport">;
    const original = client.latestReport.bind(client);
    client.latestReport = async (name, windowMinutes, population) => {
      if (population === undefined) throw new Error("Original population selector required");
      const report = await original(name, windowMinutes, population);
      if (!report) return report;
      if (windowMinutes !== null && report.snapshot.options?.window_minutes !== windowMinutes) {
        throw new Error("A language reread changed the original window");
      }
      const { pageServerReadReport } = await import(fixturePath);
      return { ...report, snapshot: { ...report.snapshot, pressure: null, resources: [] }, page_server_reads: pageServerReadReport(1) };
    };
  }, { apiPath: "/src/lib/api.ts", fixturePath: "/src/test/page-server-reads.ts" });
  await page.locator(".language-select").selectOption("fr");
  const region = page.getByRole("region", { name: french["view.page_server_reads.title"], exact: true });
  await expect(region).toContainText(french["ui.page_server_reads.scope"]);
  await region.getByText(french["ui.retained_failures.observations"], { exact: true }).click();
  await expect(region).toContainText(french["view.page_server_reads.observed"]);
  await expect(region).not.toContainText(english["view.page_server_reads.observed"]);
  await expect(region.getByText(EXACT_PAGE_SERVER_PAGES, { exact: true })).toHaveCount(2);
});
