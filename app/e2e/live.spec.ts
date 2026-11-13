import { expect, test } from "@playwright/test";
import type { LiveState } from "../src/gen/LiveState";
import type { ObservedWaitResource } from "../src/gen/ObservedWaitResource";
import type { Api } from "../src/lib/api";
import { openConnection } from "./demo";

const cases: { connection: string; kind: ObservedWaitResource["kind"]; wait: string; id: string; english: string; spanish: string }[] = [
  { connection: "mssql-demo", kind: "memory-page", wait: "PAGELATCH_EX", id: "5:1:4144", english: "Memory page", spanish: "Página en memoria" },
  { connection: "mssql-demo", kind: "page-io", wait: "PAGEIOLATCH_SH", id: "5:2:91", english: "Page I/O", spanish: "E/S de página" },
  { connection: "mysql-demo", kind: "data-lock", wait: "row-lock", id: "opaque-native-lock:42", english: "Data lock", spanish: "Bloqueo de datos" },
];

for (const example of cases) {
  test(`The ${example.kind} wait target belongs to its request and translates without naming a blocker`, async ({ page }, info) => {
    await openConnection(page, example.connection);
    const live: LiveState = {
      taken_at: "2026-11-13T10:00:00Z", notes: [], waits: [], session_actions: [], session_actions_note: null, blocking: null,
      sessions: [{
        id: "42", user: "app", application: "example-app", host: null, database: "example", state: "suspended", command: "SELECT",
        wait: example.wait, wait_ms: 40000, blocked_by: null, elapsed_ms: 100000, transaction_ms: null, cpu_ms: 20, reads: 100,
        sql: "SELECT id FROM app.orders", waiting_on_lock: example.kind === "data-lock",
        wait_resources: [{ kind: example.kind, native: { id: example.id, qualifiers: {} } }],
      }],
    };
    await page.evaluate(async ({ live, apiPath, storePath }) => {
      const client = await (await import(apiPath)).api() as Api;
      client.live = async () => live;
      (await import(storePath)).setState({ view: "live" });
    }, { live, apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts" });
    const row = page.locator('tr[data-session="42"]');
    await expect(row).toContainText(`${example.english} ${example.id}`);
    await expect(row.locator("td").nth(4)).toHaveText("");
    await expect(row).toContainText("SELECT id FROM app.orders");
    await page.getByRole("complementary").getByRole("combobox").selectOption("es");
    await expect(row).toContainText(`${example.spanish} ${example.id}`);
    await expect(row.locator("td").nth(4)).toHaveText("");
    if (example.kind === "memory-page") await page.screenshot({ path: info.outputPath("live-wait-resource.png") });
  });
}
