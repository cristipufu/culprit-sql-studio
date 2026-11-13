import { expect, test } from "@playwright/test";
import type { CompilationMemory } from "../src/gen/CompilationMemory";
import type { State } from "../src/lib/store";
import { openConnection } from "./demo";

test("Query details show individual compilation memory without multiplying it by the compile count", async ({ page }, info) => {
  await openConnection(page, "mssql-demo");
  const memory: CompilationMemory = { average_kb: 32768, last_kb: 16384, maximum_kb: 65536 };
  await page.evaluate(async ({ path, memory }) => {
    const { getState, setState } = await import(path) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("The fixture report must be loaded");
    const report = structuredClone(state.report.report);
    const query = report.snapshot.workload?.queries[0];
    if (!query?.costs) throw new Error("The fixture must contain a query with recorded costs");
    query.costs = { ...query.costs, compile_history: {
      count: 300, total_ms: 6000, first_at: "2026-11-13T09:00:00Z", last_at: "2026-11-13T09:59:00Z",
      within_window: true, memory,
    } };
    setState({ report: { ...state.report, report }, view: "queries", query: query.id });
  }, { path: "/src/lib/store.ts", memory });
  const row = page.locator(".beyond-row").filter({ has: page.getByText("Average compile memory", { exact: true }) });
  await expect(row).toBeVisible();
  await expect(row.locator(".beyond-value")).toHaveText("32.0 MB");
  await expect(row).toContainText("16.0 MB last");
  await expect(row).toContainText("64.0 MB peak");
  await expect(row).toContainText("current use and concurrency are unknown");
  await expect(row).not.toContainText("GB");
  await row.screenshot({ path: info.outputPath("compile-memory.png") });

  await page.evaluate(async (path) => {
    const { getState, setState } = await import(path) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("The fixture report must be loaded");
    const report = structuredClone(state.report.report);
    for (const query of report.snapshot.workload?.queries ?? []) {
      if (query.costs?.compile_history) query.costs.compile_history.memory = undefined;
    }
    setState({ report: { ...state.report, report } });
  }, "/src/lib/store.ts");
  await expect(row).toHaveCount(0);
});
