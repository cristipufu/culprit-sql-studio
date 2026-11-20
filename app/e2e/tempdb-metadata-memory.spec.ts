import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

async function inject(page: Page, mode: "usage" | "unknown", id: string) {
  await waitForReport(page);
  await page.evaluate(async ({ mode, id, storePath, fixturePath }) => {
    const { getState, setState } = await import(storePath) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const { tempdbMetadataHealthReport, tempdbMetadataReport } = await import(fixturePath);
    const state = getState(); if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    const fixture = tempdbMetadataHealthReport(tempdbMetadataReport(mode));
    report.snapshot.id = id; report.tempdb_metadata_memory = fixture.tempdb_metadata_memory;
    report.snapshot.sources = fixture.snapshot.sources;
    setState({ report: { ...state.report, report } });
  }, { mode, id, storePath: "/src/lib/store.ts", fixturePath: "/src/test/tempdb-metadata-memory.ts" });
}

test("metadata context stays folded, pages exact native consumers and resets with a different immutable report", async ({ page }) => {
  await openConnection(page, "mssql-demo"); await inject(page, "usage", "metadata-original-a");
  await page.locator('[data-tile="health"]').click();
  const region = page.getByRole("region", { name: english["view.tempdb_metadata_memory.title"], exact: true });
  const cards = region.locator(".operational-card");
  await expect(region).toBeVisible(); await expect(region.locator("details[open]")).toHaveCount(0);
  await expect(cards).toHaveCount(10); await expect(cards.first()).not.toBeVisible();
  await region.getByText(english["ui.retained_failures.observations"], { exact: true }).click();
  await expect(cards.first().getByText("9007199254740993", { exact: true })).toBeVisible();
  await expect(cards.first().getByText("0", { exact: true })).toBeVisible();
  await region.getByRole("button", { name: english["ui.termination_history.next"], exact: true }).click();
  await expect(cards).toHaveCount(2); await expect(cards.first()).toContainText("consumer-10");
  await inject(page, "unknown", "metadata-original-b");
  await expect(cards).toHaveCount(0); await expect(region).not.toContainText("9007199254740993");
  await expect(region).toContainText(english["view.tempdb_metadata_memory.summary.unavailable"]);
  await inject(page, "usage", "metadata-original-c");
  await expect(cards).toHaveCount(10); await expect(cards.first()).not.toBeVisible();
  await expect(region.locator("details[open]")).toHaveCount(0);
});
