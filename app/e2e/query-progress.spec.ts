import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { currentWorkReport, progressObservation, progressPlan, requestPageServerReadReport, sessionProgressContext } from "../src/test/query-progress";
import { progressCalls, replaceProgress, showProgress, type ProgressWindow } from "./query-progress";

test("request page-server counts remain exact in folded shared details and clear when native evidence is unavailable", async ({ page }) => {
  await showProgress(page, progressObservation({ engine: "mssql", page_server_reads: "9007199254740993", page_server_reads_report: requestPageServerReadReport() }), "mssql-demo");
  const panel = page.locator(".qprog");
  const details = panel.locator("details").filter({ has: page.locator("summary", { hasText: "Native page-server reads for this request" }) }).first();
  await expect(details).not.toHaveAttribute("open");
  await details.locator("summary").first().click();
  await expect(details).toContainText("9,007,199,254,740,993");
  const limits = details.locator("details").first();
  await expect(limits).not.toHaveAttribute("open");
  await limits.locator("summary").click();
  await expect(limits).toContainText("does not identify the statement");
  await expect(limits).toContainText("does not measure physical storage I/O");
  await replaceProgress(page, progressObservation({ engine: "mssql", running: null, request_key: null }), "mssql-demo");
  await expect(panel).not.toContainText("Native page-server reads for this request");
  await expect.poll(async () => (await progressCalls(page)).length).toBeGreaterThan(2);
});

test("unavailable and independent observations keep polling, clear plans and a same-SQL new native request gets one plan read", async ({ page }, info) => {
  const original = progressObservation({ plan: progressPlan("Original native plan") });
  await showProgress(page, original);
  const panel = page.locator(".qprog");
  await expect(panel.locator(".qp-plan")).toContainText("Original native plan");

  await replaceProgress(page, progressObservation({ running: null, request_key: null, statement: null }));
  await expect(panel).toContainText(english["ui.query_progress.unavailable"]);
  await expect(panel.locator(".qp-plan")).not.toContainText("Original native plan");
  await expect(panel.locator(".freshness")).not.toHaveText(english["ui.query_progress.stopped"]);
  await expect.poll(async () => (await progressCalls(page)).filter((call) => call.requestKey === null).length).toBeGreaterThan(1);

  await replaceProgress(page, progressObservation({ running: null, request_key: null, statement: null, session_context: sessionProgressContext() }));
  await expect(panel).toContainText(english["ui.query_progress.independent"]);
  await expect(panel.locator(".qp-state")).toHaveText(english["ui.query_progress.session_only"]);
  await expect(panel.locator(".qp-session-context")).toContainText("processlist");
  await expect(panel.locator(".qp-plan")).not.toContainText("Original native plan");

  const next = progressObservation({
    request_key: "native-request-b", plan: progressPlan("Replacement native plan"), phase: "Native sorting phase",
    phase_percent: 0.65, current_work_report: currentWorkReport(),
  });
  expect(next.statement).toBe(original.statement);
  await replaceProgress(page, next);
  await expect(panel.locator(".qp-plan")).toContainText("Replacement native plan");
  await expect.poll(async () => (await progressCalls(page)).filter((call) => call.requestKey === "native-request-b").length).toBeGreaterThan(2);
  const calls = await progressCalls(page);
  expect(calls.filter((call) => call.requestKey === "native-request-b" && call.withPlan)).toHaveLength(1);
  expect(calls.filter((call) => call.requestKey === null && call.withPlan)).toHaveLength(0);
  await expect(panel.getByRole("progressbar", { name: english["ui.query_progress.phase_only"] })).toHaveAttribute("aria-valuenow", "65");
  await expect(panel.locator('[data-progress-scope="statement"]')).toHaveCount(0);
  const work = panel.locator(".qp-current-work");
  await expect(work).not.toHaveAttribute("open");
  await work.locator(":scope > summary").click();
  await expect(work).toContainText("9007199254740993");
  await expect(work.locator("details").first()).not.toHaveAttribute("open");
  await page.screenshot({ path: info.outputPath("request-work-phase-progress.png") });
});

test("a late progress reply cannot follow the same session ID onto another connection", async ({ page }) => {
  await showProgress(page, progressObservation({ plan: progressPlan("Original native plan") }));
  const panel = page.locator(".qprog");
  await expect(panel.locator(".qp-plan")).toContainText("Original native plan");
  await page.evaluate(() => { (window as ProgressWindow).queryProgressProbe.heldConnections.push("mysql-demo"); });
  await expect.poll(() => page.evaluate(() => (window as ProgressWindow).queryProgressProbe.pending.length)).toBeGreaterThan(0);
  const replacement = progressObservation({ request_key: "other-connection-native-request", plan: progressPlan("Other route native plan") });
  await page.evaluate(async ({ replacement, storePath }) => {
    const probe = (window as ProgressWindow).queryProgressProbe;
    probe.frames["other-progress-route"] = replacement;
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const state = getState();
    const original = state.connections.find((connection) => connection.name === "mysql-demo")!;
    setState({ connections: [...state.connections, { ...original, name: "other-progress-route" }], active: "other-progress-route", view: "live" });
  }, { replacement, storePath: "/src/lib/store.ts" });
  await expect(panel).toHaveCount(0);
  await page.locator('tr[data-session="42"]').click();
  await expect(panel.locator(".qp-plan")).toContainText("Other route native plan");
  const retired = progressObservation({ request_key: "retired-native-request", plan: progressPlan("Retired reply must not appear") });
  await page.evaluate((retired) => {
    const probe = (window as ProgressWindow).queryProgressProbe;
    for (const pending of probe.pending.splice(0)) pending.resolve(retired);
  }, retired);
  await expect.poll(async () => (await progressCalls(page)).filter((call) => call.connection === "other-progress-route").length).toBeGreaterThan(1);
  await expect(panel.locator(".qp-plan")).toContainText("Other route native plan");
  await expect(panel).not.toContainText("Retired reply must not appear");
});

test("watching another session discards the previous session's pending native reply", async ({ page }) => {
  await showProgress(page, progressObservation({ plan: progressPlan("Original session plan") }));
  await expect(page.locator(".qprog .qp-plan")).toContainText("Original session plan");
  await page.evaluate(() => {
    const probe = (window as ProgressWindow).queryProgressProbe;
    probe.live.sessions.push({ ...probe.live.sessions[0]!, id: "43" });
    probe.heldConnections.push("mysql-demo");
  });
  await expect.poll(() => page.evaluate(() => (window as ProgressWindow).queryProgressProbe.pending.length)).toBeGreaterThan(0);
  await expect(page.locator('tr[data-session="43"]')).toBeVisible();
  await replaceProgress(page, progressObservation({ session: "43", request_key: "session-43-native-request", plan: progressPlan("New session native plan") }));
  await page.evaluate(() => { (window as ProgressWindow).queryProgressProbe.heldConnections = []; });
  await page.locator('tr[data-session="43"]').click();
  const panel = page.locator(".qprog");
  await expect(panel.locator(".qp-plan")).toContainText("New session native plan");
  await page.evaluate((retired) => {
    const probe = (window as ProgressWindow).queryProgressProbe;
    for (const pending of probe.pending.splice(0)) pending.resolve(retired);
  }, progressObservation({ plan: progressPlan("Retired session reply must not appear") }));
  await expect.poll(async () => (await progressCalls(page)).filter((call) => call.session === "43").length).toBeGreaterThan(1);
  await expect(panel.locator(".qp-plan")).toContainText("New session native plan");
  await expect(panel).not.toContainText("Retired session reply must not appear");
});

test("an unavailable initial request continues refreshing and only an explicit idle observation stops polling", async ({ page }) => {
  await page.clock.install();
  await showProgress(page, progressObservation({ running: null, request_key: null, statement: null }));
  const panel = page.locator(".qprog");
  await expect(panel.locator(".qp-state")).toHaveText(english["ui.query_progress.unverified"]);
  await expect.poll(async () => (await progressCalls(page)).length).toBeGreaterThan(2);
  expect((await progressCalls(page)).filter((call) => call.withPlan)).toHaveLength(1);
  await replaceProgress(page, progressObservation({ running: false, request_key: null, state: "idle", statement: null }));
  await page.clock.runFor(1500);
  await expect(panel.locator(".freshness")).toHaveText(english["ui.query_progress.stopped"]);
  const stoppedCount = (await progressCalls(page)).length;
  await page.clock.runFor(3500);
  expect((await progressCalls(page)).length).toBe(stoppedCount);
});
