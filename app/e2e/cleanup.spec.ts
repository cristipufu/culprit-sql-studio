import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { CleanupReport } from "../src/gen/CleanupReport";
import type { CleanupSnapshot } from "../src/gen/CleanupSnapshot";
import type { RetentionSnapshot } from "../src/gen/RetentionSnapshot";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import {
  EXACT_REASON_COUNT, NATIVE_COLUMN_CANARY, PREPARED_AT, PVS_SOURCE, cleanupPressure,
  persistentVersionObservation, persistentVersionRead, persistentVersionReport,
  preparedRetainer, retainer, retentionEvidence, retentionReport,
} from "../src/test/cleanup";
import { openConnection } from "./demo";

const TITLE = english["view.cleanup.title"];

async function inject(page: Page, reads: CleanupSnapshot[], retention: RetentionSnapshot | null, view: CleanupReport | null, sources: SourceOutcome[]) {
  await expect.poll(async () => page.evaluate(async (path) => {
    const { getState } = await import(path) as { getState: () => State };
    const report = getState().report;
    return report.status === "ready" && !report.scanning;
  }, "/src/lib/store.ts")).toBe(true);
  await page.evaluate(async ({ path, reads, pressure, view, sources }) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A ready fixture report is required");
    const report = structuredClone(state.report.report);
    report.snapshot.pressure = pressure;
    report.snapshot.resources = [];
    report.snapshot.sources = sources;
    report.snapshot.operations = {
      ...report.snapshot.operations, spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null,
      memory_failures: undefined, native_failures: undefined, termination_history: undefined, node_resources: undefined,
      session_memory: undefined, page_waits: undefined, task_space: undefined, query_compilation: undefined, cleanup: reads,
    };
    report.memory_failures = undefined;
    report.native_failures = undefined;
    report.request_memory = undefined;
    report.session_memory = undefined;
    report.page_waits = undefined;
    report.task_space = undefined;
    report.query_compilation = undefined;
    report.cleanup = view ?? undefined;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", reads, pressure: retention ? cleanupPressure(retention) : null, view, sources });
}

test("native PVS cleanup counts and columns stay exact escaped and source-qualified in collapsed evidence", async ({ page }, info) => {
  await page.setViewportSize({ width: 1000, height: 900 });
  await openConnection(page, "mssql-demo");
  const native = persistentVersionObservation();
  const view = persistentVersionReport(native);
  await inject(page, [persistentVersionRead([native])], null, view, view.sources);
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  const card = region.locator("article").first();
  await expect(card).not.toBeVisible();
  await page.screenshot({ path: info.outputPath("cleanup-pvs-collapsed.png") });
  await region.getByText(english["ui.cleanup.inspect"], { exact: true }).click();
  await expect(card).toContainText(EXACT_REASON_COUNT);
  await expect(card).not.toContainText("9007199254740992");
  await card.getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  const columns = card.locator("dl > div").filter({ hasText: english["ui.cleanup.native_columns"] });
  await expect(columns.locator("code")).toContainText(NATIVE_COLUMN_CANARY);
  await expect(card.locator("script")).toHaveCount(0);
  await expect(card).toContainText("2026-11-13T10:00:00Z");
  await expect(card).toContainText("2026-11-13T10:00:01Z");
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await page.screenshot({ path: info.outputPath("cleanup-pvs-native-columns.png") });

  const wrongRow = { ...native, source: "unrelated-source" };
  for (const reads of [
    [{ ...persistentVersionRead(), source: "unrelated-source" }],
    [persistentVersionRead([wrongRow])],
    [persistentVersionRead([native, structuredClone(native)])],
  ]) {
    await inject(page, reads, null, view, view.sources);
    await expect(region).toContainText(english["ui.memory_failures.missing_row"]);
    await expect(region).not.toContainText(EXACT_REASON_COUNT);
  }
  await inject(page, [], null, null, [{ source: PVS_SOURCE, state: "unavailable", reason: "Native cleanup permission denied." }]);
  await expect(region).toBeVisible();
  await expect(region.locator("article")).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Database health", exact: true })).not.toContainText(english["ui.health.empty"]);
  await region.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(region).toContainText("Native cleanup permission denied.");
});

test("PG cleanup joins full retainer generations pages ten records and keeps prepared time separate", async ({ page }, info) => {
  await page.setViewportSize({ width: 1000, height: 900 });
  await openConnection(page, "pg-demo");
  const holders = Array.from({ length: 12 }, (_, index) => retainer(index));
  const view = retentionReport(holders);
  await inject(page, [], retentionEvidence([...holders].reverse()), view, view.sources);
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  const cards = region.locator("article");
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await page.screenshot({ path: info.outputPath("cleanup-pg-collapsed.png") });
  await region.getByText(english["ui.cleanup.inspect"], { exact: true }).click();
  const first = cards.first();
  await expect(first.getByRole("heading")).toHaveText("Native retainer 0");
  await first.getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  await first.getByText(english["ui.memory_participants.native_properties"], { exact: true }).first().click();
  await expect(first.getByText(holders[0]!.session!.generation, { exact: true }).first()).toBeVisible();
  await expect(first.getByText("native-xid-0", { exact: true })).toBeVisible();
  await expect(first).not.toContainText(holders[1]!.session!.generation);
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await page.screenshot({ path: info.outputPath("cleanup-pg-native-generation.png") });
  await region.getByRole("button", { name: "Next records", exact: true }).click();
  await expect(cards).toHaveCount(2);
  const later = cards.first();
  await expect(later.getByRole("heading")).toHaveText("Native retainer 10");
  await later.getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  await expect(later.getByText("native-xid-10", { exact: true })).toBeVisible();
  await expect(later).not.toContainText("native-xid-0");

  const original = retainer(0);
  const originalView = retentionReport([original]);
  for (const observed of [[retainer(1)], [{ ...original, source: "unrelated-retainer-source" }], [original, structuredClone(original)]]) {
    await inject(page, [], retentionEvidence(observed), originalView, originalView.sources);
    await expect(region).toContainText(english["ui.memory_failures.missing_row"]);
    await expect(region).not.toContainText("native-xid-0");
  }
  const prepared = preparedRetainer();
  const preparedView = retentionReport([prepared]);
  await inject(page, [], retentionEvidence([prepared]), preparedView, preparedView.sources);
  const preparedCard = cards.first();
  await expect(preparedCard.getByRole("heading")).toHaveText("Prepared native transaction");
  await preparedCard.getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  const started = preparedCard.locator("dl > div").filter({ hasText: english["ui.cleanup.transaction_started"] });
  const preparation = preparedCard.locator("dl > div").filter({ hasText: english["ui.cleanup.transaction_prepared"] });
  await expect(started.locator("dd")).toHaveText(english["ui.memory_failures.not_reported"]);
  await expect(preparation.locator("dd")).toHaveText(PREPARED_AT);
  await expect(preparedCard.getByText("generation", { exact: true })).toHaveCount(0);
  await page.screenshot({ path: info.outputPath("cleanup-pg-prepared-context.png") });
});
