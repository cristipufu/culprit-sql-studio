import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

test("retained page history disclosure preserves local clock, no fake health and native statuses", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.evaluate(async ({ storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as {
      getState: () => State; setState: (patch: Partial<State>) => void;
    };
    const { suspectPageHistoryReport } = await import(fixturePath) as typeof import("../src/test/suspect-pages");
    const state = getState();
    if (state.report.status !== "ready") throw new Error("Settled report required");
    const history = suspectPageHistoryReport();
    history.database = "<img src=x onerror=alert(1)>🧪";
    const report = structuredClone(state.report.report);
    report.suspect_pages = history;
    setState({ report: { ...state.report, report } });
  }, { storePath: "/src/lib/store.ts", fixturePath: "/src/test/suspect-pages.ts" });
  await page.locator('[data-tile="health"]').click();
  const section = page.getByRole("region", { name: english["view.suspect_pages.title"] });
  const disclosure = section.locator("details");
  await expect(disclosure).not.toHaveAttribute("open", "");
  await section.getByText(english["view.suspect_pages.details"], { exact: true }).click();
  await expect(disclosure).toHaveAttribute("open", "");
  await expect(section.getByText("<img src=x onerror=alert(1)>🧪", { exact: true })).toBeVisible();
  await expect(section.locator("img")).toHaveCount(0);
  await expect(section.getByText(english["view.suspect_pages.limit.incarnation"], { exact: true })).toBeVisible();
});
