import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { StatementCounterSnapshot } from "../src/gen/StatementCounterSnapshot";
import type { StatementWorkReport } from "../src/gen/StatementWorkReport";
import type { State } from "../src/lib/store";
import { statementEvidence, statementPoint, statementReport } from "../src/test/statement-work";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, evidence: StatementCounterSnapshot, view: StatementWorkReport | null) {
  await waitForReport(page);
  await page.evaluate(async ({ path, evidence, view }) => {
    const { getState, setState } = await import(path) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A ready report is required");
    const report = structuredClone(state.report.report);
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.operations.statement_work = evidence;
    report.statement_work = view ?? undefined;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", evidence, view });
}

test("statement outcomes stay folded and paged while preserving exact errors, measured zero and native context", async ({ page }, info) => {
  await openConnection(page, "mysql-demo");
  const rows = Array.from({ length: 12 }, (_, index) => statementPoint(index));
  const hostile = rows[0]!;
  if (hostile.fact.kind !== "statement-counters") throw new Error("Statement fixture required");
  const label = "<script>statement_context()</script>https://fixture.invalid";
  hostile.fact.data.identity.qualifiers.schema = label;
  const view = statementReport(rows);
  await inject(page, statementEvidence([...rows].reverse()), view);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: view.title, exact: true });
  const records = region.locator(".observation-grid > .operational-card");
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(records).toHaveCount(10);
  await expect(records.first()).not.toBeVisible();
  await region.screenshot({ path: info.outputPath("statement-outcomes-collapsed.png") });

  await region.getByText(english["ui.statement_work.inspect"], { exact: true }).click();
  const first = records.first();
  await expect(first).toContainText("9007199254740993");
  await expect(first).not.toContainText("9007199254740992");
  await expect(first.locator("dd")).toContainText(["0"]);
  await expect(first).toContainText("2020-01-01T12:00:00.123456Z");
  await first.getByText(english["ui.memory_participants.native_properties"], { exact: true }).click();
  await expect(first.getByText(label, { exact: true })).toBeVisible();
  await expect(first.locator("script, img, a")).toHaveCount(0);

  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(records).toHaveCount(2);
  await records.first().getByText(english["ui.memory_participants.native_properties"], { exact: true }).click();
  await expect(records.first()).toContainText("schema-10");
  await page.setViewportSize({ width: 480, height: 800 });
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await region.screenshot({ path: info.outputPath("statement-outcomes-narrow.png") });
});

test("statement source failures and a changed native schema cannot turn into borrowed counts or healthy emptiness", async ({ page }) => {
  await openConnection(page, "mysql-demo");
  const view = statementReport();
  const changed = statementPoint();
  if (changed.fact.kind !== "statement-counters") throw new Error("Statement fixture required");
  changed.fact.data.identity.qualifiers.schema = "unrelated-schema";
  await inject(page, statementEvidence([changed]), view);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: view.title, exact: true });
  await region.getByText(english["ui.statement_work.inspect"], { exact: true }).click();
  await expect(region).toContainText(english["ui.memory_failures.missing_row"]);
  await expect(region).not.toContainText("9007199254740993");

  const unavailable = statementEvidence([]);
  unavailable.context = null;
  unavailable.coverage.state = "unavailable";
  const failed = statementReport([]);
  failed.source_state = "unavailable";
  failed.source_reason = "Native statement permission denied.";
  await inject(page, unavailable, failed);
  await expect(region.locator("article")).toHaveCount(0);
  await region.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(region).toContainText(failed.source_reason);
  await expect(region).not.toContainText("9007199254740993");

  await inject(page, unavailable, null);
  const withheld = page.getByRole("region", { name: english["view.statement_work.title"], exact: true });
  await expect(withheld).toBeVisible();
  await expect(withheld).not.toContainText(english["ui.health.empty"]);
  await withheld.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(withheld).toContainText(english["view.statement_work.invalid"]);
});
