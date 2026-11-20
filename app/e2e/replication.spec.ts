import { expect, test, type Locator, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { ReplicationReport } from "../src/gen/ReplicationReport";
import type { State } from "../src/lib/store";
import { replicationFields, replicationReport } from "../src/test/replication";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, view: ReplicationReport) {
  await waitForReport(page);
  await page.evaluate(async ({ path, view }) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A ready report is required");
    const report = structuredClone(state.report.report);
    report.snapshot.id = `${report.snapshot.id}:replication-replacement`;
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.sources = [];
    report.snapshot.operations = { log_failures: [], table_memory: [], spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null };
    report.findings = [];
    report.replication = view;
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", view });
}

async function openHealth(page: Page, view: ReplicationReport) {
  await page.locator('[data-tile="health"]').click();
  await page.getByRole("dialog", { name: "Database health", exact: true }).waitFor();
  return page.getByRole("region", { name: view.title, exact: true });
}

async function openEvidence(region: Locator) {
  await region.locator(".replication-disclosure > summary").click();
}

test("native replication starts compact and lazily separates database, group and replica fields", async ({ page }, testInfo) => {
  const view = replicationReport("mssql_availability_group");
  await openConnection(page, "mssql-demo");
  await inject(page, view);
  const region = await openHealth(page, view);
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(region.locator(".replication-entry")).toHaveCount(0);
  await expect(region.locator(".replication-fields")).toHaveCount(0);
  expect((await region.boundingBox())!.height).toBeLessThan(180);
  await region.screenshot({ path: testInfo.outputPath("replication-collapsed.png") });
  await openEvidence(region);
  await expect(region.locator(".replication-entry")).toHaveCount(Math.min(10, view.entries.length));
  await expect(region.locator(".replication-fields")).toHaveCount(0);
  const first = region.locator(".replication-entry").first();
  await first.locator(":scope > summary").click();
  await expect(first.locator(".replication-entry-body > .replication-fields")).toHaveCount(1);
  await expect(first.locator(".replication-group > summary")).toHaveText(view.entries[0]!.groups.map((group) => group.title));
  await expect(first.locator(".replication-group .replication-fields")).toHaveCount(0);
  const group = first.locator(".replication-group").first();
  await group.locator(":scope > summary").click();
  await expect(group.locator(".replication-fields")).toHaveCount(1);
  await region.screenshot({ path: testInfo.outputPath("replication-native-group-expanded.png") });
  await group.locator(":scope > summary").click();
  await expect(group.locator(".replication-fields")).toHaveCount(0);
  await openEvidence(region);
  await expect(region.locator(".replication-entry")).toHaveCount(0);
  await expect(region.locator(".replication-fields")).toHaveCount(0);
});

test("partial and unavailable replication stays visible without legacy rows and presentation limits preserve native counts", async ({ page }) => {
  const partial = replicationReport("partial");
  await openConnection(page, "mssql-demo");
  await inject(page, partial);
  const region = await openHealth(page, partial);
  await openEvidence(region);
  for (const source of partial.sources) {
    await expect(region.locator(".replication-sources")).toContainText(source.source);
    if (source.reason != null) await expect(region.locator(".replication-sources")).toContainText(source.reason);
  }
  const unavailable = replicationReport("unavailable");
  await inject(page, unavailable);
  await expect(region.locator(".replication-disclosure[open]")).toHaveCount(0);
  await openEvidence(region);
  await expect(region.locator(".replication-entry")).toHaveCount(0);
  await expect(region.locator(".replication-sources")).toContainText(english["ui.replication.source.unavailable"]);
  await expect(region.locator(".replication-sources")).not.toContainText(english["ui.replication.source.empty"]);
  const empty = replicationReport("empty");
  expect([unavailable.native_rows, unavailable.observed_rows, unavailable.presented_rows]).toEqual([empty.native_rows, empty.observed_rows, empty.presented_rows]);
  await inject(page, empty);
  await expect(region.locator(".replication-disclosure[open]")).toHaveCount(0);
  await expect(region.locator(".replication-body")).toHaveCount(0);
  await openEvidence(region);
  await expect(region.locator(".replication-sources")).toContainText(english["ui.replication.source.empty"]);
  await expect(region.locator(".replication-sources")).not.toContainText(english["ui.replication.source.unavailable"]);
  const limited = replicationReport("limited");
  await inject(page, limited);
  await expect(region.locator(".replication-disclosure[open]")).toHaveCount(0);
  await openEvidence(region);
  await expect(region.locator(".replication-limit")).toContainText(english["ui.replication.presentation_limit"]);
  const counts = region.locator(".replication-counts dd");
  await expect(counts).toHaveText([limited.native_rows, limited.observed_rows, limited.state_rows, limited.presented_rows].map((value) => new Intl.NumberFormat("en").format(value)));
  const entries = region.locator(".replication-entry");
  await expect(entries).toHaveCount(10);
  const firstTitle = await entries.first().locator(":scope > summary").innerText();
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(entries.first().locator(":scope > summary")).not.toHaveText(firstTitle);
  await expect(entries).toHaveCount(Math.min(10, limited.entries.length - 10));
  await expect(region.locator(".replication-entry-body")).toHaveCount(0);
});

test("exact MySQL values and unzoned clocks survive opening while hostile native text creates no HTML or links", async ({ page }, testInfo) => {
  const view = replicationReport("mysql_channel");
  const first = view.entries[0]!;
  const fields = replicationFields(view);
  const clock = fields.find((field) => field.key === "io_error_at_without_offset")?.value;
  if (!clock) throw new Error("The Rust MySQL case must retain its native error clock");
  const position = fields.find((field) => field.key === "received-source-log-position")?.value;
  expect(position).toBe("9007199254740993");
  first.fields.push({ key: "hostile-display", label: "<img src=x onerror=alert(1)>", value: "<script>replication_xss_canary()</script> javascript:alert(1) https://example.invalid" });
  await openConnection(page, "mysql-demo");
  await inject(page, view);
  const region = await openHealth(page, view);
  await openEvidence(region);
  const entry = region.locator(".replication-entry").first();
  await entry.locator(":scope > summary").click();
  const groups = entry.locator(".replication-group");
  await expect(groups).toHaveCount(first.groups.length);
  for (const group of await groups.all()) {
    await group.locator(":scope > summary").click();
    await expect(group.locator(".replication-fields")).toHaveCount(1);
  }
  await expect(entry).toContainText(position!);
  await expect(entry).toContainText(clock);
  await expect(entry).not.toContainText(`${clock}Z`);
  await expect(entry).toContainText("<script>replication_xss_canary()</script>");
  await expect(entry).toContainText(english["ui.memory_failures.not_reported"]);
  await expect(region.locator("script, img, a")).toHaveCount(0);
  await page.setViewportSize({ width: 480, height: 800 });
  const bounds = await region.boundingBox();
  expect(bounds).not.toBeNull();
  expect(await region.evaluate((element) => element.scrollWidth)).toBeLessThanOrEqual(Math.ceil(bounds!.width) + 1);
  await region.screenshot({ path: testInfo.outputPath("replication-native-mysql-narrow.png") });
});
