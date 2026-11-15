import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import { blockingEvidence } from "../src/test/blocking";
import { actionCalls, showLiveActions, type ActionWindow } from "./live-actions";

test("a same-ID replacement retires the blocker confirmation without signalling the new native session", async ({ page }) => {
  const live = blockingEvidence();
  await showLiveActions(page, live);
  const card = page.locator(".block-card");
  await card.getByRole("button", { name: english["ui.blocking_chains.cancel.button"], exact: true }).click();
  await page.evaluate(() => {
    const frame = (window as ActionWindow).liveActionProbe.frames["pg-demo"]!;
    for (const session of [frame.sessions[0]!, frame.blocking!.chains[0]!.tree.context!]) {
      session.action_target!.activity.session.generation = "replacement-native-session";
      session.sql = "SELECT replacement_session";
    }
  });
  await expect(card).toContainText("replacement_session");
  await expect(card).toContainText(english["ui.session_actions.changed"]);
  await expect(card.getByRole("button", { name: english["ui.blocking_chains.cancel.confirm"].replace("{session}", "30"), exact: true })).toHaveCount(0);
  expect(await actionCalls(page)).toEqual([]);
});

test("a new request in the same session retires row cancellation rather than adopting the latest request", async ({ page }) => {
  const live = blockingEvidence();
  live.blocking = null;
  await showLiveActions(page, live);
  const row = page.locator('tr[data-session="30"]');
  await row.getByRole("button", { name: english["ui.common.cancel"], exact: true }).click();
  await page.evaluate(() => {
    const session = (window as ActionWindow).liveActionProbe.frames["pg-demo"]!.sessions[0]!;
    session.action_target!.activity.request!.generation = "next-native-request";
    session.action_target!.statement!.id = "next-native-statement";
    session.sql = "SELECT next_request";
  });
  await expect(row).toContainText("next_request");
  await expect(row).toContainText(english["ui.session_actions.changed"]);
  await expect(row.getByRole("button", { name: english["ui.live.action.cancel_confirm"].replace("{session}", "30"), exact: true })).toHaveCount(0);
  expect(await actionCalls(page)).toEqual([]);
});

test("whole-session termination keeps the original request and route when only its current request advances", async ({ page }) => {
  const live = blockingEvidence();
  live.blocking = null;
  await showLiveActions(page, live);
  const row = page.locator('tr[data-session="30"]');
  await row.getByRole("button", { name: english["ui.live.action.end"], exact: true }).click();
  await page.evaluate(() => {
    const session = (window as ActionWindow).liveActionProbe.frames["pg-demo"]!.sessions[0]!;
    session.action_target!.activity.request!.generation = "next-native-request";
    session.action_target!.statement!.id = "next-native-statement";
    session.action_target!.context.observed_from = "2026-11-15T10:02:00Z";
    session.action_target!.context.observed_until = "2026-11-15T10:02:01Z";
    session.sql = "SELECT next_request";
  });
  await expect(row).toContainText("next_request");
  await row.getByRole("button", { name: english["ui.live.action.end_confirm"].replace("{session}", "30"), exact: true }).click();
  await expect.poll(() => actionCalls(page)).toEqual([{ route: live.session_action_route!, target: live.sessions[0]!.action_target!, action: "terminate" }]);
});

test("changing connections cannot carry a same-ID confirmation or old rows onto the new route", async ({ page }) => {
  const live = blockingEvidence();
  live.blocking = null;
  await showLiveActions(page, live);
  const row = page.locator('tr[data-session="30"]');
  await row.getByRole("button", { name: english["ui.common.cancel"], exact: true }).click();
  await page.evaluate(async (storePath) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const probe = (window as ActionWindow).liveActionProbe;
    const next = structuredClone(probe.frames["pg-demo"]!);
    next.session_action_route = { connection: "second-native-route", revision: "b".repeat(64) };
    next.sessions[0]!.sql = "SELECT second_route_request";
    probe.frames["second-native-route"] = next;
    const state = getState();
    const original = state.connections.find((connection) => connection.name === "pg-demo")!;
    setState({ connections: [...state.connections, { ...original, name: "second-native-route" }], active: "second-native-route", view: "live" });
  }, "/src/lib/store.ts");
  await expect(row).toContainText("second_route_request");
  await expect(row.getByRole("button", { name: english["ui.live.action.cancel_confirm"].replace("{session}", "30"), exact: true })).toHaveCount(0);
  expect(await actionCalls(page)).toEqual([]);
  await row.getByRole("button", { name: english["ui.common.cancel"], exact: true }).click();
  await row.getByRole("button", { name: english["ui.live.action.cancel_confirm"].replace("{session}", "30"), exact: true }).click();
  await expect.poll(() => actionCalls(page)).toHaveLength(1);
  expect((await actionCalls(page))[0]!.route.connection).toBe("second-native-route");
});

test("a same-name connection edit withholds the original frame while its new native read is pending", async ({ page }) => {
  const live = blockingEvidence();
  live.blocking = null;
  await showLiveActions(page, live);
  await page.locator('tr[data-session="30"]').getByRole("button", { name: english["ui.common.cancel"], exact: true }).click();
  await page.evaluate(async (storePath) => {
    const { getState, setState } = await import(storePath) as typeof import("../src/lib/store");
    const probe = (window as ActionWindow).liveActionProbe;
    probe.heldConnections.push("pg-demo");
    probe.frames["pg-demo"]!.session_action_route!.revision = "c".repeat(64);
    probe.frames["pg-demo"]!.sessions[0]!.sql = "SELECT edited_route_request";
    setState({ connections: getState().connections.map((connection) => connection.name === "pg-demo" ? { ...connection, host: "edited.fixture.invalid" } : connection) });
  }, "/src/lib/store.ts");
  await expect(page.locator('tr[data-session="30"]')).toHaveCount(0);
  await expect.poll(() => page.evaluate(() => (window as ActionWindow).liveActionProbe.pendingReads.length)).toBeGreaterThan(0);
  expect(await actionCalls(page)).toEqual([]);
  await page.evaluate(() => {
    const probe = (window as ActionWindow).liveActionProbe;
    probe.heldConnections = [];
    for (const pending of probe.pendingReads.splice(0)) pending.resolve(structuredClone(probe.frames[pending.connection]!));
  });
  const row = page.locator('tr[data-session="30"]');
  await expect(row).toContainText("edited_route_request");
  await expect(row.getByRole("button", { name: english["ui.live.action.cancel_confirm"].replace("{session}", "30"), exact: true })).toHaveCount(0);
  expect(await actionCalls(page)).toEqual([]);
});
