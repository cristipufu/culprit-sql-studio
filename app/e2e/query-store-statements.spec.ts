import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { PageServerReadReport } from "../src/gen/PageServerReadReport";
import type { State } from "../src/lib/store";
import { pageServerStatementReport } from "../src/test/query-store-statements";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, view: PageServerReadReport) {
  await waitForReport(page);
  await page.evaluate(async ({view, path}) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    report.snapshot.pressure = null;
    report.snapshot.resources = [];
    report.snapshot.sources = [];
    report.page_server_reads = view;
    setState({report:{...state.report,report}});
  }, {view,path:"/src/lib/store.ts"});
}

test("historical SQL is folded text with independent definition gaps and no live lookup or executable markup", async ({page}) => {
  await openConnection(page,"pg-demo");
  const report = pageServerStatementReport();
  const statement = report.entries[0]!.statement!;
  statement.preview = '<script>query_store_canary()</script><img src=x onerror="query_store_canary()">';
  statement.preview_truncated = true;
  await inject(page,report);
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region",{name:report.title,exact:true});
  await expect(region).toContainText(english["view.page_server_reads.statement.limit.partial"]);
  await expect(region.locator("details[open]")).toHaveCount(0);
  await region.getByText(english["ui.retained_failures.observations"],{exact:true}).click();
  const disclosure = region.locator("article details").filter({has:page.getByText(english["ui.query_store_statement.inspect"],{exact:true})});
  await expect(disclosure.locator("pre")).not.toBeVisible();
  await disclosure.getByText(english["ui.query_store_statement.inspect"],{exact:true}).click();
  await expect(disclosure.locator("pre")).toHaveText(statement.preview!);
  await expect(disclosure).toContainText(english["view.query_store_statement.shortened"]);
  await expect(disclosure).toContainText("opaque-original-definition");
  await expect(disclosure.locator("a,button,input,textarea,img,script")).toHaveCount(0);
  await expect(region.getByText("9007199254740993",{exact:true})).toHaveCount(2);
  expect(await page.evaluate(() => "query_store_canary" in window)).toBe(false);
});
