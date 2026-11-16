import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { CompilerAdmissionReport } from "../src/gen/CompilerAdmissionReport";
import type { CompilerAdmissionSnapshot } from "../src/gen/CompilerAdmissionSnapshot";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import { compilerAdmissionEvidence, compilerAdmissionReport, compilerGateway, compilerWaiter, COMPILER_ADMISSION_SOURCE } from "../src/test/compiler-admission";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, evidence: CompilerAdmissionSnapshot | null, view: CompilerAdmissionReport | null, source: SourceOutcome) {
  await waitForReport(page);
  await page.evaluate(async ({ path, evidence, view, source }) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A ready report is required");
    const report = structuredClone(state.report.report);
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.operations = { ...report.snapshot.operations, compiler_admission: evidence ?? undefined };
    report.snapshot.sources = [source];
    report.compiler_admission = view ?? undefined;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", evidence, view, source });
}

test("compiler admission separates the affected actor from gateway context and keeps limits, paging and failures explicit", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  const rows = [compilerWaiter(), ...Array.from({ length: 12 }, (_, index) => compilerGateway(index))];
  const view = structuredClone(compilerAdmissionReport(rows));
  rows[1]!.source = "other-native-source";
  const changed = rows[2]!;
  if (changed.fact.kind !== "counter-set") throw new Error("gateway fixture");
  changed.fact.data.identity.qualifiers.resource_pool_id = "different-native-pool";
  await inject(page, compilerAdmissionEvidence([...rows].reverse()), view, { source: COMPILER_ADMISSION_SOURCE, state: "partial", reason: "Native gateway visibility is partial." });
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: english["view.compiler_admission.title"], exact: true });
  const cards = region.locator(".observation-grid > .operational-card");
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await expect(region).toContainText(english["ui.compiler_admission.window_unknown"]);
  await region.getByText(english["ui.compiler_admission.inspect"], { exact: true }).click();
  await cards.first().getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  await expect(cards.first()).toContainText("original-session");
  await expect(cards.first()).toContainText("original-request");
  await expect(cards.first()).toContainText("native-client<script>canary</script>");
  await expect(region.locator("script")).toHaveCount(0);
  await expect(region).not.toContainText("compiler_private_sql_body_canary");
  for (const index of [1, 2]) {
    await expect(cards.nth(index)).toContainText(english["ui.memory_failures.missing_row"]);
    await expect(cards.nth(index)).not.toContainText("9007199254740993");
  }
  await cards.nth(3).getByText(english["ui.memory_participants.native_properties"], { exact: true }).first().click();
  await expect(cards.nth(3)).toContainText("9007199254740993");
  await region.getByRole("button", { name: "Next records", exact: true }).click();
  await expect(cards).toHaveCount(3);
  await inject(page, null, null, { source: COMPILER_ADMISSION_SOURCE, state: "unavailable", reason: "Native compiler permission denied." });
  await expect(region.locator("article")).toHaveCount(0);
  await region.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(region).toContainText("Native compiler permission denied.");
});
