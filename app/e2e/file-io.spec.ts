import { expect, test, type Page } from "@playwright/test";
import type { FileIo } from "../src/gen/FileIo";
import type { State } from "../src/lib/store";
import { openConnection, waitForReport } from "./demo";

const nativeFile: FileIo = {
  name: "native-data", kind: "data", database: "shop",
  context: { read: "storage-operation", write: "storage-operation", replica_id: null },
  reads: 1000, read_ms: 1000, read_bytes: 8192000, writes: 0, write_ms: null, write_bytes: null,
  queued_read_ms: "9007199254740993", queued_write_ms: "0",
  logical_size: { kind: "bytes", bytes: "67108864" }, maximum_size: { kind: "unlimited" },
  scope: "database", stats_reset: "2026-11-16T10:00:00Z",
};

async function inject(page: Page, file: FileIo) {
  await page.evaluate(async ({ path, file }) => {
    const { getState, setState } = await import(path) as { getState: () => State; setState: (patch: Partial<State>) => void };
    const state = getState();
    if (state.report.status !== "ready") throw new Error("A settled report is required");
    const report = structuredClone(state.report.report);
    if (!report.snapshot.pressure) throw new Error("The fixture must retain its native pressure context");
    report.snapshot.pressure.files = [file];
    setState({ report: { ...state.report, report } });
  }, { path: "/src/lib/store.ts", file });
}

test("File details stay collapsed, preserve exact native values and disappear when metadata is unavailable", async ({ page }, info) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await inject(page, nativeFile);
  await page.locator('[data-tile="storage"]').click();
  const dialog = page.getByRole("dialog", { name: "Database health", exact: true });
  await expect(dialog).toBeVisible();
  const facts = dialog.locator(".io-facts");
  await expect(facts).toHaveCount(1);
  await expect(facts).not.toHaveAttribute("open");
  const exact = facts.getByText(/9007199254740993/);
  await expect(exact).not.toBeVisible();
  await page.screenshot({ path: info.outputPath("file-details-collapsed.png"), animations: "disabled" });
  const summary = facts.locator("summary");
  await summary.focus();
  await summary.press("Enter");
  await expect(facts).toHaveAttribute("open", "");
  await expect(exact).toBeVisible();
  await expect(facts).toContainText("67108864 bytes");
  await expect(facts.locator("li")).toHaveCount(4);
  const bounds = await dialog.evaluate((element) => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await page.screenshot({ path: info.outputPath("file-details-expanded.png"), animations: "disabled" });
  await summary.press("Enter");
  await expect(exact).not.toBeVisible();
  const unknown = { ...nativeFile };
  delete unknown.queued_read_ms;
  delete unknown.queued_write_ms;
  delete unknown.logical_size;
  delete unknown.maximum_size;
  await inject(page, unknown);
  await expect(dialog.locator(".io-facts")).toHaveCount(0);
});
