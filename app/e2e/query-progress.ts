import { expect, type Page } from "@playwright/test";
import type { LiveState } from "../src/gen/LiveState";
import type { QueryProgress } from "../src/gen/QueryProgress";
import type { Api } from "../src/lib/api";
import { progressLiveState } from "../src/test/query-progress";
import { openConnection, waitForReport } from "./demo";

export type ProgressCall = { connection: string; session: string; withPlan: boolean; requestKey: string | null };
export type ProgressProbe = {
  frames: Record<string, QueryProgress>;
  live: LiveState;
  calls: ProgressCall[];
  heldConnections: string[];
  pending: { connection: string; resolve: (point: QueryProgress) => void }[];
};
export type ProgressWindow = typeof window & { queryProgressProbe: ProgressProbe };

/** Script native API points only. The production panel still owns polling, caching and rendering. */
export async function showProgress(page: Page, point: QueryProgress): Promise<void> {
  await openConnection(page, "mysql-demo");
  await waitForReport(page);
  await page.evaluate(async ({ point, live, apiPath, storePath }) => {
    const client = await (await import(apiPath)).api() as Api;
    const probe: ProgressProbe = { frames: { "mysql-demo": point }, live, calls: [], heldConnections: [], pending: [] };
    (window as ProgressWindow).queryProgressProbe = probe;
    client.live = async () => structuredClone(probe.live);
    client.queryProgress = async (connection, session, withPlan) => {
      const frame = probe.frames[connection];
      if (!frame) throw new Error("The progress test has no native point for this route");
      probe.calls.push({ connection, session, withPlan, requestKey: frame.request_key });
      if (probe.heldConnections.includes(connection)) return new Promise<QueryProgress>((resolve) => probe.pending.push({ connection, resolve }));
      return { ...structuredClone(frame), session, plan: withPlan ? structuredClone(frame.plan) : null };
    };
    (await import(storePath)).setState({ view: "live" });
  }, { point, live: progressLiveState(), apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts" });
  await page.locator('tr[data-session="42"]').click();
  await expect(page.locator(".qprog")).toBeVisible();
}

export async function replaceProgress(page: Page, point: QueryProgress, connection = "mysql-demo"): Promise<void> {
  await page.evaluate(({ point, connection }) => {
    (window as ProgressWindow).queryProgressProbe.frames[connection] = point;
  }, { point, connection });
}

export function progressCalls(page: Page): Promise<ProgressCall[]> {
  return page.evaluate(() => (window as ProgressWindow).queryProgressProbe.calls);
}
