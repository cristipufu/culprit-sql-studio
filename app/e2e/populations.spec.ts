import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { NativeIdentity } from "../src/gen/NativeIdentity";
import type { Finding } from "../src/gen/Finding";
import type { Report } from "../src/gen/Report";
import type { Api } from "../src/lib/api";
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

type Read = { kind: "cache" | "scan"; name: string; minutes: number; population: NativeIdentity | null; language: string };
type PopulationProbe = {
  calls: Read[];
  cache: Map<string, Report>;
  holdFirst: boolean;
  pending: { finish: () => void; fail: () => void }[];
  finding: Finding;
  actions: { kind: string; reportId: string }[];
};
type ProbeWindow = typeof window & { populationProbe: PopulationProbe; populationHtml?: boolean };

async function controlledGroups(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1000, height: 900 });
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(async ({ apiPath, storePath, fixturePath, identityPath, languagePath }) => {
    const store = await import(storePath) as typeof import("../src/lib/store");
    const fixture = await import(fixturePath) as typeof import("../src/test/populations");
    const { nativeIdentityKey } = await import(identityPath) as typeof import("../src/lib/native-identity");
    const { language } = await import(languagePath) as typeof import("../src/lib/i18n");
    const client = await (await import(apiPath)).api() as Api;
    const state = store.getState();
    if (state.report.status !== "ready") throw new Error("The initial demo read did not settle");
    const base = structuredClone(state.report.report);
    const key = (name: string, minutes: number, population: NativeIdentity | null) => JSON.stringify([name, minutes, population ? nativeIdentityKey(population) : null]);
    if (!base.findings[0]) throw new Error("The action fixture requires one original finding shape");
    const probe: PopulationProbe = { calls: [], cache: new Map(), holdFirst: false, pending: [], finding: structuredClone(base.findings[0]), actions: [] };
    (window as ProbeWindow).populationProbe = probe;
    for (const name of ["mssql-demo", "mssql-demo-staging"]) {
      for (const minutes of [60, 360, 10080]) {
        for (const population of [null, fixture.GROUP_A, fixture.GROUP_B]) {
          probe.cache.set(key(name, minutes, population), fixture.executionGroupReport(base, name, minutes, population));
        }
      }
    }
    client.latestReport = async (name, minutes, population) => {
      const selected = minutes ?? 10080;
      probe.calls.push({ kind: "cache", name, minutes: selected, population, language: language() });
      const report = probe.cache.get(key(name, selected, population));
      if (!report) return null;
      if (probe.holdFirst && population && nativeIdentityKey(population) === nativeIdentityKey(fixture.GROUP_A)) {
        return new Promise<Report>((resolve, reject) => probe.pending.push({
          finish: () => resolve(structuredClone(report)),
          fail: () => reject(new Error("Retired group cache failed")),
        }));
      }
      return structuredClone(report);
    };
    client.scan = async (name, minutes, population) => {
      const selected = minutes ?? 10080;
      probe.calls.push({ kind: "scan", name, minutes: selected, population, language: language() });
      const report = fixture.executionGroupReport(base, name, selected, population);
      probe.cache.set(key(name, selected, population), report);
      return structuredClone(report);
    };
    const overview = fixture.executionGroupReport(base, state.active!, 10080, null);
    store.setState({ view: "queries", workloadPopulation: null, windowMinutes: 10080, query: null, finding: null, changes: null,
      report: { status: "ready", report: overview, scanning: false, scanError: null } });
  }, {
    apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts", fixturePath: "/src/test/populations.ts",
    identityPath: "/src/lib/native-identity.ts", languagePath: "/src/lib/i18n.ts",
  });
}

async function chooseGroup(page: Page, label: RegExp): Promise<void> {
  await page.locator(".population-picker > button").click();
  await page.locator(".population-menu").getByRole("option", { name: label }).click();
}

async function state(page: Page): Promise<{ name: string | null; group: NativeIdentity | null; minutes: number; report: string | null; query: string | null }> {
  return page.evaluate(async (path) => {
    const { getState } = await import(path) as { getState: () => State };
    const current = getState();
    return { name: current.active, group: current.workloadPopulation, minutes: current.windowMinutes,
      report: current.report.status === "ready" ? current.report.report.snapshot.id : null, query: current.query };
  }, "/src/lib/store.ts");
}

test("Query history keeps an overview and exact group/window caches, preferences and escaped source labels", async ({ page }, info) => {
  await controlledGroups(page);
  const picker = page.locator(".population-picker > button");
  await expect(picker).toContainText(english["ui.population_picker.default"]);
  await expect(page.locator("main .empty h3")).toHaveText(english["ui.population_picker.label"]);
  await expect(page.locator("main .empty")).toContainText(english["ui.population_picker.required"]);
  await expect(page.locator(".topbar .context")).toContainText(english["ui.population_picker.required"]);
  await expect(page.locator("main")).not.toContainText(english["ui.queries.no_history_detail"]);
  await picker.click();
  const menu = page.locator(".population-menu");
  await expect(menu.getByRole("option")).toHaveCount(3);
  await expect(menu).toContainText(english["ui.population_picker.required"]);
  await expect(menu).toContainText("<img");
  await expect(page.locator("#population-html")).toHaveCount(0);
  expect(await page.evaluate(() => (window as ProbeWindow).populationHtml ?? false)).toBe(false);
  await info.attach("query-history-choices", { body: await page.screenshot(), contentType: "image/png" });
  await menu.getByRole("option", { name: /^Write group/ }).click();
  await expect.poll(async () => (await state(page)).group?.id).toBe("17");
  await expect(page.locator("main")).toContainText("execution_group_17");
  await page.locator(".topbar .menu-anchor:not(.population-picker) > .picker").click();
  await page.getByRole("option", { name: english["ui.window_picker.last_hour"], exact: false }).click();
  await expect.poll(async () => (await state(page)).minutes).toBe(60);
  await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-17-60");

  await chooseGroup(page, /^Read group/);
  await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-18-10080");
  await expect(page.locator("main")).toContainText("execution_group_18");
  await expect(page.locator("main")).not.toContainText("execution_group_17");
  await page.locator(".topbar .menu-anchor:not(.population-picker) > .picker").click();
  await page.getByRole("option", { name: english["ui.window_picker.last_6_hours"], exact: false }).click();
  await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-18-360");
  await chooseGroup(page, /^Write group/);
  await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-17-60");
  await chooseGroup(page, /^Overview$/);
  await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-overview-10080");

  await page.evaluate(async (path) => {
    const store = await import(path) as typeof import("../src/lib/store");
    await store.openConnection("mssql-demo-staging", { rescan: false });
    store.setState({ view: "queries" });
  }, "/src/lib/store.ts");
  await expect.poll(async () => (await state(page)).group).toBe(null);
  await chooseGroup(page, /^Read group/);
  await expect.poll(async () => (await state(page)).minutes).toBe(10080);
  await page.evaluate(async (path) => {
    const store = await import(path) as typeof import("../src/lib/store");
    await store.openConnection("mssql-demo", { rescan: false });
    store.setState({ view: "queries" });
  }, "/src/lib/store.ts");
  await expect.poll(async () => (await state(page)).group).toBe(null);
  await chooseGroup(page, /^Write group/);
  await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-17-60");
  const scans = await page.evaluate(() => (window as ProbeWindow).populationProbe.calls.filter((read) => read.kind === "scan"));
  expect(scans.map((read) => [read.name, read.population?.id, read.minutes])).toEqual([
    ["mssql-demo", "17", 60], ["mssql-demo", "18", 360],
  ]);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await info.attach("query-history-selected", { body: await page.screenshot(), contentType: "image/png" });
});

test("Finding actions and the verified view use exact saved references instead of newer latest reports", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await controlledGroups(page);
  await chooseGroup(page, /^Write group/);
  await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-17-10080");
  await page.evaluate(async ({ apiPath, storePath }) => {
    const client = await (await import(apiPath)).api() as Api;
    const store = await import(storePath) as typeof import("../src/lib/store");
    const current = store.getState();
    if (current.report.status !== "ready") throw new Error("No selected group report");
    const probe = (window as ProbeWindow).populationProbe;
    const report = structuredClone(current.report.report);
    const finding = { ...structuredClone(probe.finding), id: "fixture:shared-finding", title: "Scoped action fixture", queries: [], actionability: "needs-attention" as const };
    report.findings = [finding];
    store.setState({ view: "overview", finding: finding.id, report: { ...current.report, report } });
    client.brief = async (_name, reportId) => { probe.actions.push({ kind: "brief", reportId }); return "Scoped brief fixture"; };
    client.exportFinding = async (_name, reportId) => { probe.actions.push({ kind: "export", reportId }); return { name: "fixture.md", contents: "Scoped export fixture", format: "markdown" }; };
    client.verifyFindings = async (name, reportId, ids) => {
      probe.actions.push({ kind: "verify", reportId });
      const after = structuredClone(report);
      after.snapshot.id = `verified-${reportId}`;
      probe.cache.set(`exact:${after.snapshot.id}`, after);
      // A concurrent later capture must not replace the verification's exact result.
      client.latestReport = async () => ({ ...structuredClone(after), snapshot: { ...after.snapshot, id: "newer-latest" } });
      return { connection: name, before_report_id: reportId, after_report_id: after.snapshot.id,
        before_taken_at: report.snapshot.taken_at, after_taken_at: after.snapshot.taken_at, summary: "Scoped verification fixture",
        findings: [{ id: ids[0]!, title: "Scoped action fixture", status: "unknown", before: null, after: null, note: null }], new_findings: [] };
    };
    client.reportAt = async (name, reportId) => {
      probe.actions.push({ kind: "exact", reportId });
      const found = [...probe.cache.values()].find((value) => value.snapshot.connection === name && value.snapshot.id === reportId);
      if (!found) throw new Error("Exact report missing");
      return structuredClone(found);
    };
  }, { apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts" });
  await page.getByRole("button", { name: new RegExp(english["ui.finding_detail.copy_for_agent"]) }).click();
  await page.getByRole("button", { name: english["ui.share_menu.share"], exact: true }).click();
  await page.getByRole("menuitem", { name: english["ui.finding_share.copy_pr"], exact: false }).click();
  await page.getByRole("button", { name: new RegExp(english["ui.finding_share.verify"]) }).click();
  await page.getByRole("button", { name: english["ui.finding_share.show_new_scan"], exact: true }).click();
  await expect.poll(async () => (await state(page)).report).toBe("verified-fixture-mssql-demo-17-10080");
  const actions = await page.evaluate(() => (window as ProbeWindow).populationProbe.actions);
  expect(actions).toEqual([
    { kind: "brief", reportId: "fixture-mssql-demo-17-10080" },
    { kind: "export", reportId: "fixture-mssql-demo-17-10080" },
    { kind: "verify", reportId: "fixture-mssql-demo-17-10080" },
    { kind: "exact", reportId: "verified-fixture-mssql-demo-17-10080" },
  ]);
});

for (const refusal of [
  {
    name: "An unqualified execution-group actual plan",
    detail: "actual-plan-execution-population-unverified",
    message: english["engine.execution.actual_population_unverified"],
    hint: english["engine.execution.actual_population_unverified_hint"],
  },
  {
    name: "An actual plan without saved native identity",
    detail: "actual-plan-identity-unverified",
    message: english["engine.execution.actual_identity_unverified"],
    hint: english["engine.execution.actual_identity_unverified_hint"],
  },
] as const) {
  test(`${refusal.name} offers no measure-once or setting workaround`, async ({ page }) => {
    await controlledGroups(page);
    await chooseGroup(page, /^Write group/);
    await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-17-10080");
    await page.evaluate(async ({ path, message, hint, detail }) => {
      const client = await (await import(path)).api() as Api;
      client.lastActualPlan = async () => { throw { code: "unsupported", message, hint, detail }; };
    }, { path: "/src/lib/api.ts", ...refusal });
    await page.getByRole("tab", { name: english["ui.queries.tab.plan"], exact: false }).click();
    await page.getByRole("tab", { name: english["ui.common.last_run"], exact: true }).click();
    const missing = page.locator(".actual-missing");
    await expect(missing).toContainText(refusal.message);
    await expect(missing).toContainText(refusal.hint);
    await expect(missing.getByRole("button", { name: english["ui.actual_run.measure"], exact: false })).toHaveCount(0);
    await expect(missing.locator(".am-code")).toHaveCount(0);
    await expect(missing.getByRole("button", { name: english["ui.common.open_in_editor"], exact: true })).toBeVisible();
  });
}

test("A late or failing group cache and a language reread cannot replace another exact selection", async ({ page }) => {
  await controlledGroups(page);
  await page.evaluate(() => { (window as ProbeWindow).populationProbe.holdFirst = true; });
  await chooseGroup(page, /^Write group/);
  await expect.poll(async () => page.evaluate(() => (window as ProbeWindow).populationProbe.pending.length)).toBe(1);
  await chooseGroup(page, /^Read group/);
  await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-18-10080");
  await page.evaluate(() => {
    const probe = (window as ProbeWindow).populationProbe;
    probe.holdFirst = false;
    probe.pending.splice(0).forEach((pending) => pending.fail());
  });
  await expect(page.locator("main")).toContainText("execution_group_18");
  await expect(page.locator(".scan-error-badge")).toHaveCount(0);
  await page.locator(".language-select").selectOption("fr");
  await expect(page.locator("html")).toHaveAttribute("lang", "fr");
  await expect.poll(async () => page.evaluate(() => (window as ProbeWindow).populationProbe.calls.some((read) => read.kind === "cache" && read.population?.id === "18" && read.language === "fr"))).toBe(true);
  await expect.poll(async () => (await state(page)).report).toBe("fixture-mssql-demo-18-10080");
  expect(await page.evaluate(() => (window as ProbeWindow).populationProbe.calls.filter((read) => read.kind === "scan").length)).toBe(0);
  await expect(page.locator("#population-html")).toHaveCount(0);
});
