import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import french from "../../locales/fr.json" with { type: "json" };
import type { ConnectionOutcomeReport } from "../src/gen/ConnectionOutcomeReport";
import type { Api } from "../src/lib/api";
import type { State } from "../src/lib/store";
import { DISPLAY_SESSION_COUNT, EXACT_SESSION_COUNT, connectionOutcomeReport,
  unavailableConnectionOutcomes } from "../src/test/connection-outcomes";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, view: ConnectionOutcomeReport | null) {
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath, view }) => {
    const { getState, setState } = await import(storePath) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const { connectionOutcomeHealthReport } = await import(fixturePath);
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    const fixture = connectionOutcomeHealthReport(view);
    report.snapshot.pressure = fixture.snapshot.pressure;
    report.snapshot.resources = [];
    report.connection_outcomes = fixture.connection_outcomes;
    setState({ report: { ...state.report, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/connection-outcomes.ts", view });
}

test("connection outcomes fold and page exact Rust-rendered values without HTML or inferred current pressure", async ({ page }, info) => {
  await openConnection(page, "pg-demo");
  const view = connectionOutcomeReport(12);
  const hostile = '<img src=x onerror="outcome_canary()"><script>outcome_canary()</script>https://fixture.invalid';
  view.entries[0]!.title = hostile;
  view.entries[0]!.metrics[0]!.native_key = hostile;
  await inject(page, view);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: view.title, exact: true });
  const cards = region.locator(".observation-grid > .operational-card");
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await expect(region).toContainText("Original rows: 17; read: 14; interpreted: 12.");
  await region.screenshot({ path: info.outputPath("connection-outcomes-folded.png") });

  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  const first = cards.first();
  await expect(first.getByRole("heading", { level: 4 })).toHaveText(hostile);
  await expect(first.getByText(DISPLAY_SESSION_COUNT, { exact: true })).toBeVisible();
  await expect(first.locator("dl").first().getByText("0", { exact: true })).toBeVisible();
  await expect(first.locator("dl").first().getByText(english["ui.memory_failures.not_reported"], { exact: true })).toBeVisible();
  await expect(first.getByText(EXACT_SESSION_COUNT, { exact: true })).not.toBeVisible();
  await expect(first).not.toContainText("9007199254740992");
  await first.getByText(english["ui.connection_outcomes.details"], { exact: true }).click();
  await expect(first.getByText(EXACT_SESSION_COUNT, { exact: true })).toBeVisible();
  await expect(first.getByText("123456.125", { exact: true })).toBeVisible();
  await expect(first.getByText(hostile, { exact: true })).toHaveCount(2);
  await expect(first.locator("script, img, a")).toHaveCount(0);
  await expect(region.locator(".sig-dot")).toHaveCount(0);
  await first.getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  await expect(first).toContainText(view.entries[0]!.verify);

  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(cards).toHaveCount(2);
  await expect(cards.first().getByRole("heading", { level: 4 })).toHaveText(view.entries[10]!.title);
  await expect(region.getByRole("button", { name: english["ui.termination_history.next"], exact: true })).toBeDisabled();
  await region.getByRole("button", { name: english["ui.termination_history.previous"], exact: true }).click();
  await expect(cards).toHaveCount(10);
  await expect(cards.first().getByRole("heading", { level: 4 })).toHaveText(hostile);
  await page.setViewportSize({ width: 480, height: 800 });
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await page.screenshot({ path: info.outputPath("connection-outcomes-narrow.png") });
});

test("an unavailable native source and a withheld explanation remain visible without fabricated observations", async ({ page }) => {
  await openConnection(page, "pg-demo");
  const view = unavailableConnectionOutcomes();
  await inject(page, view);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: view.title, exact: true });
  await expect(region).toBeVisible();
  await expect(region).toContainText(english["ui.dashboard.status.unavailable"]);
  await expect(region.locator("article")).toHaveCount(0);
  await region.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(region.getByText(english["view.connection_outcomes.limit.source"], { exact: true })).toBeVisible();
  await expect(region.getByText(english["view.connection_outcomes.limit.no_facts"], { exact: true })).toBeVisible();
  await expect(region).not.toContainText(EXACT_SESSION_COUNT);

  await inject(page, null);
  await expect(region).toBeVisible();
  await expect(region.getByText(english["view.connection_outcomes.invalid"], { exact: true })).toBeVisible();
  await expect(region.locator("article")).toHaveCount(0);
  await expect(page.getByRole("main")).not.toContainText(english["ui.health.empty"]);
});

test("a language reread replaces derived connection meanings and UI captions while preserving exact values and the request selector", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await inject(page, connectionOutcomeReport());
  await page.locator('[data-tile="health"]').click();
  await page.evaluate(async ({ apiPath, fixturePath }) => {
    const client = await (await import(apiPath)).api() as Pick<Api, "latestReport">;
    const original = client.latestReport.bind(client);
    client.latestReport = async (name, windowMinutes, population) => {
      if (population === undefined) throw new Error("The original default/group selector is required");
      const report = await original(name, windowMinutes, population);
      if (!report) return report;
      if (windowMinutes !== null && report.snapshot.options?.window_minutes !== windowMinutes) {
        throw new Error("A language reread changed the original window");
      }
      const { connectionOutcomeReport } = await import(fixturePath);
      return { ...report, snapshot: { ...report.snapshot, pressure: null, resources: [] },
        connection_outcomes: connectionOutcomeReport() };
    };
  }, { apiPath: "/src/lib/api.ts", fixturePath: "/src/test/connection-outcomes.ts" });
  await page.locator(".language-select").selectOption("fr");
  const region = page.getByRole("region", { name: french["view.connection_outcomes.title"], exact: true });
  await expect(region).toBeVisible();
  await expect(region).toContainText(french["ui.connection_outcomes.scope"]);
  await expect(region).not.toContainText(english["view.connection_outcomes.population.database"]);
  await region.getByText(french["ui.retained_failures.observations"], { exact: true }).click();
  const first = region.locator("article").first();
  await expect(first).toContainText(french["view.connection_outcomes.population.database"]);
  await expect(first.getByText(DISPLAY_SESSION_COUNT, { exact: true })).toBeVisible();
  await first.getByText(french["ui.connection_outcomes.details"], { exact: true }).click();
  await expect(first.getByText(EXACT_SESSION_COUNT, { exact: true })).toBeVisible();
  await expect(first).toContainText(french["view.connection_outcomes.meaning.sessions_established"]);
  await expect(first).not.toContainText(english["view.connection_outcomes.meaning.sessions_established"]);
  await expect(first.locator("dl").first().getByText(french["ui.memory_failures.not_reported"], { exact: true })).toBeVisible();
});
