import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { MemoryParticipantsReport } from "../src/gen/MemoryParticipantsReport";
import type { SessionMemorySnapshot } from "../src/gen/SessionMemorySnapshot";
import type { SourceOutcome } from "../src/gen/SourceOutcome";
import type { State } from "../src/lib/store";
import { openConnection } from "./demo";

const SOURCE = "session memory";
const TITLE = english["view.session_memory.title"];
const ROW_CLOCK = "2026-11-13T10:00:00.500000Z";
const EVENT_CLOCK = "2026-11-13T10:00:00.600000Z";

function evidence(): SessionMemorySnapshot {
  return {
    source: SOURCE, observed_from: "2026-11-13T10:00:00.100000Z", observed_until: "2026-11-13T10:00:00.900000Z",
    resource: {
      engine: "mysql", kind: "instance", native: { id: "native-instance", qualifiers: {} },
      namespace: { id: "mysql-server", qualifiers: { server_uuid: "native-instance" } },
    },
    // Presentation order is independent of the native read order and qualifier insertion order.
    observations: Array.from({ length: 12 }, (_, index) => ({
      source: SOURCE, identity: { id: String(120 + index), qualifiers: { namespace: "native-instance", generation: "1770000000" } },
      observed_at: ROW_CLOCK, session: { id: String(75 + index), generation: String(120 + index) }, tracking_enabled: true,
      current_bytes: index === 0 ? "18446744073709551615" : String(1024 + index), controlled_current_bytes: "0",
      high_water_bytes: "18446744073709551615", controlled_high_water_bytes: null, native_properties: { INSTRUMENTED: "YES" },
      current_context: { request: { id: String(5 + index), generation: String(999 + index) }, observed_at: EVENT_CLOCK, database: "app", query_hash: "a".repeat(64) },
    })).reverse(),
  };
}

function explanation(evidence: SessionMemorySnapshot): MemoryParticipantsReport {
  return {
    title: TITLE, summary: "12 native sessions retained; no allocating statement established.",
    source_state: "partial", source_reason: "Only a bounded native population was read.",
    limitations: ["The current event is context, not the statement that allocated this session's memory."],
    entries: [...evidence.observations].reverse().map((row, index) => ({
      observation_key: `backend-reference-${index}`,
      observation_identity: { id: row.identity.id, qualifiers: { generation: "1770000000", namespace: "native-instance" } },
      title: `Native participant ${index}`, explanation: "This is tracked session allocation, without an established allocating statement.",
      investigation: "Correlate native identity and capture clocks before testing a code change.",
      verify: "Compare the same native measurement after the tested change.",
    })),
  };
}

async function inject(page: Page, evidence: SessionMemorySnapshot | null, view: MemoryParticipantsReport | null, source: SourceOutcome) {
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
    report.snapshot.options.collect_session_memory = true;
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.operations = {
      ...report.snapshot.operations, spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null,
      memory_failures: undefined, native_failures: undefined, termination_history: undefined, node_resources: undefined,
      session_memory: evidence ?? undefined,
    };
    report.snapshot.sources = [source];
    report.memory_failures = undefined;
    report.native_failures = undefined;
    report.request_memory = undefined;
    report.session_memory = view ?? undefined;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", evidence, view, source });
}

test("memory participants stay collapsed and page exact typed native references without promoting context to an owner", async ({ page }, info) => {
  await page.setViewportSize({ width: 1000, height: 900 });
  await openConnection(page, "mysql-demo");
  const native = evidence();
  await inject(page, native, explanation(native), { source: SOURCE, state: "partial", reason: "Only a bounded native population was read." });
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  const region = page.getByRole("region", { name: TITLE, exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  const cards = region.locator("article");
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await page.screenshot({ path: info.outputPath("memory-participants-collapsed.png") });

  await region.getByText("Inspect retained participants", { exact: true }).click();
  await expect(cards.first()).toBeVisible();
  await expect(cards.first().getByRole("heading")).toHaveText("Native participant 0");
  await cards.first().getByText("Investigation and native evidence", { exact: true }).click();
  await expect(cards.first()).toContainText("18446744073709551615 B");
  await expect(cards.first()).toContainText("0 B");
  await expect(cards.first()).toContainText(ROW_CLOCK);
  await expect(cards.first()).toContainText(EVENT_CLOCK);
  await expect(cards.first()).toContainText("without an established allocating statement");
  const bounds = await region.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await page.screenshot({ path: info.outputPath("memory-participants-native-details.png") });
  await region.getByRole("button", { name: "Next records", exact: true }).click();
  await expect(cards).toHaveCount(2);
  await expect(cards.first().getByRole("heading")).toHaveText("Native participant 10");
  await expect(region).toContainText("Observations 11–12 of 12");

  await inject(page, null, null, { source: SOURCE, state: "unavailable", reason: "The native source could not be read safely." });
  await expect(region).toBeVisible();
  await expect(region.locator("article")).toHaveCount(0);
  await expect(region).toContainText("Not available here");
  const reason = region.getByText("The native source could not be read safely.", { exact: true });
  await expect(reason).not.toBeVisible();
  await region.getByText("What this evidence establishes", { exact: true }).click();
  await expect(reason).toBeVisible();
});
