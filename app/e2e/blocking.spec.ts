import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { blockingEvidence } from "../src/test/blocking";
import { actionCalls, showLiveActions } from "./live-actions";

test("An unqualified blocker keeps its independent row but cannot supply context, navigation or actions to the chain", async ({ page }) => {
  const live = blockingEvidence();
  const chain = live.blocking!.chains[0]!;
  chain.head_kind = "unknown";
  chain.head_ms = null;
  chain.transaction_ms = null;
  chain.tree.context = null;
  await showLiveActions(page, live);
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
  expect(await actionCalls(page)).toEqual([]);
});

test("A qualified blocker renders its captured SQL and sends only its projected identity after explicit confirmation", async ({ page }) => {
  const live = blockingEvidence();
  live.sessions[0]!.sql = "DELETE FROM independent_later_request";
  live.sessions[0]!.application = "independent-later-program";
  await showLiveActions(page, live);
  const card = page.locator(".block-card");
  await expect(card).toContainText("original_request_30");
  await expect(card).toContainText("native-program-30");
  await expect(card).not.toContainText("independent_later_request");
  await expect(page.locator('tr[data-session="30"]')).toContainText("independent_later_request");
  await card.getByRole("button", { name: english["ui.blocking_chains.cancel.button"], exact: true }).click();
  expect(await actionCalls(page)).toEqual([]);
  await card.getByRole("button", { name: english["ui.blocking_chains.cancel.confirm"].replace("{session}", "30"), exact: true }).click();
  await expect.poll(() => actionCalls(page)).toEqual([{ route: live.session_action_route!, target: live.blocking!.chains[0]!.tree.context!.action_target!, action: "cancel" }]);
});
