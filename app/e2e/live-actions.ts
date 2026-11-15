import type { Page } from "@playwright/test";
import type { LiveState } from "../src/gen/LiveState";
import type { SessionActionRequest } from "../src/gen/SessionActionRequest";
import type { Api } from "../src/lib/api";
import { openConnection, waitForReport } from "./demo";

export type ActionProbe = {
  frames: Record<string, LiveState>;
  calls: SessionActionRequest[];
  reads: string[];
  heldConnections: string[];
  pendingReads: { connection: string; resolve: (value: LiveState) => void }[];
};
export type ActionWindow = typeof window & { liveActionProbe: ActionProbe };

/** Script only the existing API boundary; these synthetic native targets never contact a database. */
export async function showLiveActions(page: Page, live: LiveState): Promise<void> {
  await openConnection(page, "pg-demo");
  await waitForReport(page);
  await page.evaluate(async ({ live, apiPath, storePath }) => {
    const client = await (await import(apiPath)).api() as Api;
    const probe: ActionProbe = { frames: { "pg-demo": live }, calls: [], reads: [], heldConnections: [], pendingReads: [] };
    (window as ActionWindow).liveActionProbe = probe;
    client.live = async (connection) => {
      probe.reads.push(connection);
      const frame = probe.frames[connection];
      if (!frame) throw new Error("The test has no native frame for this route");
      if (probe.heldConnections.includes(connection)) return new Promise<LiveState>((resolve) => probe.pendingReads.push({ connection, resolve }));
      return structuredClone(frame);
    };
    client.interruptSession = async (request) => {
      probe.calls.push(structuredClone(request));
      return "signal-sent";
    };
    (await import(storePath)).setState({ view: "live" });
  }, { live, apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts" });
}

export function actionCalls(page: Page): Promise<SessionActionRequest[]> {
  return page.evaluate(() => (window as ActionWindow).liveActionProbe.calls);
}
