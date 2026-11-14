import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { TaskSpaceReport } from "../src/gen/TaskSpaceReport";
import type { TaskSpaceSnapshot } from "../src/gen/TaskSpaceSnapshot";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import { TASK_SOURCE, taskSpace, taskSpaceEvidence, taskSpaceReport } from "../src/test/task-space";
import { openConnection } from "./demo";

const TITLE = english["view.task_space.title"];

async function inject(page: Page, evidence: TaskSpaceSnapshot | null, view: TaskSpaceReport | null, source: SourceOutcome) {
  await expect.poll(async () => page.evaluate(async (path) => {
    const { getState } = await import(path) as { getState: () => State };
    const report = getState().report;
    return report.status === "ready" && !report.scanning;
  }, "/src/lib/store.ts")).toBe(true);
  await page.evaluate(async ({ path, evidence, view, source }) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A ready fixture report is required");
    const report = structuredClone(state.report.report);
    if (!report.snapshot.options) throw new Error("The fixture scan options must be present");
    report.snapshot.options.collect_task_space = true;
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.operations = {
      ...report.snapshot.operations, spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null,
      memory_failures: undefined, native_failures: undefined, termination_history: undefined, node_resources: undefined,
      session_memory: undefined, page_waits: undefined, task_space: evidence ?? undefined,
    };
    report.snapshot.sources = [source];
    report.memory_failures = undefined;
    report.native_failures = undefined;
    report.request_memory = undefined;
    report.session_memory = undefined;
    report.page_waits = undefined;
    report.task_space = view ?? undefined;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", evidence, view, source });
}

test("task tempdb counts retain exact native tuples and checked SQL context in collapsed bounded records", async ({ page }, info) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await openConnection(page, "mssql-demo");
  const rows = Array.from({ length: 12 }, (_, index) => taskSpace(String(index)));
  rows[1]!.after_check!.state = "changed";
  rows[1]!.request!.statement = { state: "unavailable", reason: "missing" };
  rows[1]!.statement_identity = null;
  rows[1]!.plan_identity = null;
  const view = taskSpaceReport(rows);
  view.title = TITLE;
  await inject(page, taskSpaceEvidence([...rows].reverse()), view, { source: TASK_SOURCE, state: "partial", reason: "The native task read was bounded." });
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  const cards = region.locator("article");
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await page.screenshot({ path: info.outputPath("task-space-collapsed.png") });
  await region.getByText(english["ui.task_space.inspect"], { exact: true }).click();
  const first = cards.first();
  await first.getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  await expect(first).toContainText("9223372036854775807");
  await expect(first).toContainText("9007199254740993");
  await expect(first).toContainText("2026-11-13T10:00:00.200000Z");
  await first.getByText(english["ui.task_space.checks"], { exact: true }).click();
  await expect(first).toContainText("2026-11-13T10:00:00.300000Z");
  await expect(first).toContainText("2026-11-13T10:00:00.700000Z");
  await first.getByText(english["ui.task_space.current_statement"], { exact: true }).click();
  await expect(first.locator("pre")).toHaveText("SELECT * FROM [orders] WHERE name=N'<script>canary</script>'");
  await expect(first.locator("script")).toHaveCount(0);
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await page.screenshot({ path: info.outputPath("task-space-native-context.png") });
  const changed = cards.nth(1);
  await changed.getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  await expect(changed.getByText(english["ui.task_space.current_statement"], { exact: true })).toHaveCount(0);
  await expect(changed).not.toContainText("task-native-statement");
  await region.getByRole("button", { name: "Next records", exact: true }).click();
  await expect(cards).toHaveCount(2);
  await expect(cards.first().getByRole("heading")).toHaveText("Native task 10");
  await inject(page, null, null, { source: TASK_SOURCE, state: "unavailable", reason: "Task visibility could not be established." });
  await expect(region).toBeVisible();
  await expect(region.locator("article")).toHaveCount(0);
  await region.getByText("What this evidence establishes", { exact: true }).click();
  await expect(region).toContainText("Task visibility could not be established.");
});
