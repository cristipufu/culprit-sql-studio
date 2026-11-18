import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { JOBS_SOURCE, jobPoint, jobsEvidence, jobsReport, agentJobEvidence, agentJobsReport } from "../src/test/jobs";
import { openConnection, waitForReport } from "./demo";

test("recorded native jobs disclose bounded exact points and limits without worker or SQL actions", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await waitForReport(page);
  const rows = Array.from({ length: 12 }, (_, index) => jobPoint(index));
  const report = jobsReport(rows);
  rows[1]!.source = "unrelated-native-source";
  await page.evaluate(async ({ evidence, report, source, path }) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("Ready fixture report required");
    const next = structuredClone(state.report.report);
    next.snapshot.operations.jobs = evidence;
    next.snapshot.sources = [{ source, state: "partial", reason: "Native job visibility is partial." }];
    next.jobs = report;
    setState({ report: { ...state.report, report: next } });
  }, { evidence: jobsEvidence(rows), report, source: JOBS_SOURCE, path: "/src/lib/store.ts" });
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: report.title, exact: true });
  await expect(region).toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
  const cards = region.locator(".observation-grid > .operational-card");
  await expect(cards).toHaveCount(10);
  await expect(cards.first()).not.toBeVisible();
  await region.getByText(english["ui.jobs.inspect"], { exact: true }).click();
  await expect(cards.first()).toContainText("9007199254740993");
  await expect(cards.first()).not.toContainText("9007199254740992");
  await expect(cards.nth(1)).toContainText(english["ui.memory_failures.missing_row"]);
  await expect(cards.nth(1)).not.toContainText("9007199254740993");
  await expect(cards.first().locator("script")).toHaveCount(0);
  await cards.first().getByText(english["ui.memory_failures.investigation"], { exact: true }).click();
  await expect(cards.first()).toContainText("without executing the job");
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(cards).toHaveCount(2);
  await expect(cards.first().getByRole("heading")).toHaveText("Native job 1010");
  await region.getByText(english["ui.memory_failures.limitations"], { exact: true }).click();
  await expect(region).toContainText("Job history retention and logging remain incomplete.");
  const html = await region.innerHTML();
  expect(html).not.toContain("private-read-host-canary");
  expect(html).not.toContain("private-read-database-canary");
  await expect(region.getByRole("button", { name: /cancel|kill|terminate|execute|run job/i })).toHaveCount(0);
});

test("Agent retained history opens as exact native text with independent local-time and current-definition limits", async ({ page }) => {
  await openConnection(page, "mssql-demo");await waitForReport(page);
  const evidence=agentJobEvidence();const report=agentJobsReport(evidence);
  await page.evaluate(async ({evidence,report,path}) => {
    const {getState,setState}=await import(path) as {getState:()=>State;setState:(patch:Partial<State>)=>void};
    const state=getState();if (state.report.status!=="ready") throw new Error("Ready fixture report required");
    const next=structuredClone(state.report.report);next.snapshot.operations.jobs=evidence;next.snapshot.sources=[{source:"mssql-agent-job-history",state:"partial",reason:"Retained history is partial."}];next.jobs=report;
    setState({report:{...state.report,report:next}});
  }, {evidence,report,path:"/src/lib/store.ts"});
  await page.locator('[data-tile="health"]').click();const region=page.getByRole("region",{name:report.title,exact:true});await expect(region).toBeVisible();await expect(region.locator("details[open]")).toHaveCount(0);
  const card=region.locator(".observation-grid > .operational-card").first();await expect(card).not.toBeVisible();await region.getByText(english["ui.jobs.inspect"],{exact:true}).click();
  await expect(card).toContainText("2147483647");await expect(card).toContainText("250101");await expect(card.locator("script")).toHaveCount(0);
  await card.getByText(english["ui.memory_failures.investigation"],{exact:true}).click();await expect(card).toContainText("without executing jobs");
  await region.getByText(english["ui.memory_failures.limitations"],{exact:true}).click();await expect(region).toContainText("Job local times have no recorded offset.");await expect(region).toContainText("An old failure does not establish a current failing job.");
  await expect(region.getByRole("button",{name:/execute|run job|kill|terminate/i})).toHaveCount(0);
});
