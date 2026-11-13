import { expect, test, type Page } from "@playwright/test";
import type { NativeFailureSnapshot } from "../src/gen/NativeFailureSnapshot";
import type { RetainedFailureReport } from "../src/gen/RetainedFailureReport";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import { openConnection } from "./demo";

function evidence(): NativeFailureSnapshot {
  return {
    source: "native failures",
    observed_from: "2026-11-13T10:00:00Z",
    observed_until: "2026-11-13T10:00:01Z",
    resource: null,
    observations: Array.from({ length: 12 }, (_, position) => ({
      native: "mysql-error-log",
      observation: {
        position, logged_at: "2020-01-01T01:02:03.000001Z", thread_id: "18446744073709551615",
        priority: "Warning", error_code: "MY-999999", subsystem: "Server", message: "PRIVATE_LOG_MESSAGE",
      },
    })),
  };
}

function explanations(evidence: NativeFailureSnapshot): RetainedFailureReport {
  return {
    title: "Retained native diagnostics", summary: "12 retained diagnostic records; no incident count established.",
    source_state: "partial", source_reason: "The bounded native read retained only a subset.",
    limitations: ["No historical query owner is established."],
    entries: evidence.observations.map(({ observation }) => ({
      position: observation.position, title: `Native diagnostic ${observation.position}`,
      explanation: "The backend has not classified this native code.",
      investigation: "Use the same instance and period.", verify: "Measure the effect of a tested change.",
    })),
  };
}

async function inject(page: Page, evidence: NativeFailureSnapshot | null, view: RetainedFailureReport | null, source: SourceOutcome, requested = true) {
  await expect.poll(async () => page.evaluate(async (path) => {
    const { getState } = await import(path) as { getState: () => State };
    const report = getState().report;
    return report.status === "ready" && !report.scanning;
  }, "/src/lib/store.ts")).toBe(true);
  await page.evaluate(async ({ path, evidence, view, source, requested }) => {
    const { getState, setState } = await import(path) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("The fixture report must be loaded");
    const report = structuredClone(state.report.report);
    if (!report.snapshot.options) throw new Error("The fixture scan options must be present");
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.options.collect_native_failures = requested;
    report.snapshot.operations.native_failures = evidence ?? undefined;
    report.snapshot.operations.memory_failures = undefined;
    report.snapshot.sources = [source];
    report.memory_failures = undefined;
    report.native_failures = view ?? undefined;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", evidence, view, source, requested });
}

test("retained native diagnostics stay collapsed, page exact metadata and expose requested unsupported reads", async ({ page }, info) => {
  await page.setViewportSize({ width: 1000, height: 900 });
  await openConnection(page, "mysql-demo");
  const native = evidence();
  await inject(page, native, explanations(native), { source: "native failures", state: "partial", reason: "The bounded native read retained only a subset." });
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const section = page.locator(".health-tab > .operational-health").filter({ has: page.getByRole("heading", { name: "Retained native diagnostics", exact: true }) });
  const cards = section.locator("article");
  await expect(section).toBeVisible();
  await expect(section.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await info.attach("native-failures-collapsed", { body: await page.screenshot({ path: info.outputPath("native-failures-collapsed.png") }), contentType: "image/png" });

  await section.getByText("Inspect retained observations", { exact: true }).click();
  await expect(cards.first()).toBeVisible();
  await expect(cards.first()).toContainText("2020-01-01T01:02:03.000001Z");
  await expect(cards.first().getByText("MY-999999", { exact: true })).not.toBeVisible();
  await cards.first().getByText("Investigation and native evidence", { exact: true }).click();
  await expect(cards.first().getByText("MY-999999", { exact: true })).toBeVisible();
  await expect(cards.first()).toContainText("18446744073709551615");
  await expect(section).not.toContainText("PRIVATE_LOG_MESSAGE");
  await section.getByRole("button", { name: "Next records", exact: true }).click();
  await expect(cards).toHaveCount(2);
  await expect(cards.first().getByRole("heading")).toHaveText("Native diagnostic 10");
  await expect(section).toContainText("Observations 11–12 of 12");
  await expect(section.getByRole("button", { name: "Next records", exact: true })).toBeDisabled();

  const unsupported: SourceOutcome = { source: "native failures", state: "not-applicable", reason: "The native source does not apply to this provider." };
  await inject(page, null, null, unsupported);
  await expect(section).toBeVisible();
  await expect(section).toContainText("This source is not available for the requested profile.");
  await expect(section).not.toContainText("No retained diagnostic rows");
  await inject(page, null, null, unsupported, false);
  await expect(section).toHaveCount(0);
});
