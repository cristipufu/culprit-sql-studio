import { expect, test, type Page } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import type { LiveState } from "../src/gen/LiveState";
import type { SessionAction } from "../src/gen/SessionAction";
import type { Api } from "../src/lib/api";
import { blockingEvidence } from "../src/test/blocking";
import { openConnection, waitForReport } from "./demo";

type BlockingCall = { session: string; action: SessionAction };
type ProbeWindow = typeof window & { blockingCalls: BlockingCall[] };

async function showBlocking(page: Page, live: LiveState): Promise<void> {
  await openConnection(page, "pg-demo");
  await waitForReport(page);
  await page.evaluate(async ({ live, apiPath, storePath }) => {
    const client = await (await import(apiPath)).api() as Api;
    const calls: BlockingCall[] = [];
    (window as ProbeWindow).blockingCalls = calls;
    client.live = async () => structuredClone(live);
    client.interruptSession = async (_connection, session, action) => { calls.push({ session, action }); };
    (await import(storePath)).setState({ view: "live" });
  }, { live, apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts" });
}

test("An unqualified blocker keeps its independent row but cannot supply context, navigation or actions to the chain", async ({ page }) => {
  const live = blockingEvidence();
  const chain = live.blocking!.chains[0]!;
  chain.head_kind = "unknown";
  chain.head_ms = null;
  chain.transaction_ms = null;
  chain.tree.context = null;
  await showBlocking(page, live);
  const card = page.locator(".block-card");
  const originalRow = page.locator('tr[data-session="30"]');
  await expect(originalRow).toContainText("original_request_30");
  await expect(card).toContainText(english["ui.blocking_chains.not_visible"]);
  await expect(card).not.toContainText("original_request_30");
  await expect(card).not.toContainText("native-program-30");
  await expect(card.getByRole("button", { name: english["ui.blocking_chains.cancel.button"], exact: true })).toHaveCount(0);
  await expect(card.getByRole("button", { name: english["ui.blocking_chains.terminate.button"], exact: true })).toHaveCount(0);
  const head = card.getByRole("treeitem").first();
  await head.click();
  await head.press("Enter");
  await expect(originalRow).not.toHaveClass(/located/);
  const knownWaiter = card.getByRole("treeitem").filter({ hasText: "original_request_10" });
  await knownWaiter.click();
  await expect(page.locator('tr[data-session="10"]')).toHaveClass(/located/);
  expect(await page.evaluate(() => (window as ProbeWindow).blockingCalls)).toEqual([]);
});

test("A qualified blocker renders its captured SQL and sends only its projected identity after explicit confirmation", async ({ page }) => {
  const live = blockingEvidence();
  live.sessions[0]!.sql = "DELETE FROM independent_later_request";
  live.sessions[0]!.application = "independent-later-program";
  await showBlocking(page, live);
  const card = page.locator(".block-card");
  await expect(card).toContainText("original_request_30");
  await expect(card).toContainText("native-program-30");
  await expect(card).not.toContainText("independent_later_request");
  await expect(page.locator('tr[data-session="30"]')).toContainText("independent_later_request");
  await card.getByRole("button", { name: english["ui.blocking_chains.cancel.button"], exact: true }).click();
  expect(await page.evaluate(() => (window as ProbeWindow).blockingCalls)).toEqual([]);
  await card.getByRole("button", { name: english["ui.blocking_chains.cancel.confirm"].replace("{session}", "30"), exact: true }).click();
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).blockingCalls)).toEqual([{ session: "30", action: "cancel" }]);
});
