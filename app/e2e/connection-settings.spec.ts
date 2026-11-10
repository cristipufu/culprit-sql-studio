import { expect, test, type Page } from "@playwright/test";
import type { Engine } from "../src/gen/Engine";
import type { NewConnection } from "../src/gen/NewConnection";
import type { ConnectionSettings } from "../src/gen/ConnectionSettings";

/** Tests exercise the real forms. Only database probes and the demo's unavailable parser are supplied here. */
async function installTestApi(page: Page) {
  await page.evaluate(async () => {
    const apiModule = "/src/lib/api.ts";
    const factsModule = "/src/lib/engines.ts";
    const { api } = await import(apiModule);
    const { connectionSecurityFacts } = await import(factsModule);
    const client = await api();
    const observed = { newDrafts: [] as NewConnection[], edits: [] as ConnectionSettings[], aborted: false, slow: false };
    Object.assign(window, { connectionSettingsTest: observed });
    client.parseConnection = async (input: string, engine: Engine | null) => {
      const file = engine === "sqlite";
      const facts = engine ? connectionSecurityFacts(engine) : null;
      const url = file ? null : new URL(input);
      const mode = url?.searchParams.get("ssl-mode") ?? url?.searchParams.get("sslmode") ?? null;
      return {
        engine, host: file ? "" : url!.hostname, port: url?.port ? Number(url.port) : null,
        database: file ? input : url!.pathname.slice(1), user: file ? null : url!.username,
        auth: file ? "none" : "password", has_password: false, display: input,
        suggested_name: "test-connection", environment: file ? "local" : "development",
        security: facts ? { settings: { mode, ca_certificate: null, trust_server_certificate: false, certificate_hostname: null }, default_mode: facts.default_mode, effective_mode: mode ?? facts.default_mode, requires_verified_tls: false } : undefined,
      };
    };
    client.testConnection = async (input: NewConnection) => {
      observed.newDrafts.push(structuredClone(input));
      if (input.security?.mode !== "DISABLED") throw { code: "connection_failed", message: "The server does not support TLS", hint: "Review this connection’s TLS settings.", detail: "connection-security" };
      return { product: "MariaDB", database: "orders" };
    };
    client.testConnectionSettings = async (_name: string, settings: ConnectionSettings, signal?: AbortSignal) => {
      observed.edits.push(structuredClone(settings));
      if (observed.slow) await new Promise<void>((_resolve, reject) => signal?.addEventListener("abort", () => { observed.aborted = true; reject(new DOMException("Canceled", "AbortError")); }, { once: true }));
      return { product: "MariaDB", database: "orders" };
    };
  });
}

async function openSettings(page: Page, name = "mysql-demo") {
  await page.getByRole("button", { name: `Edit connection settings for ${name}`, exact: true }).click();
  return page.getByRole("dialog", { name: "Edit connection", exact: true });
}

for (const [engine, label, input, options] of [
  ["mssql", "SQL Server", "mssql://reader@db.example.com/orders", 4],
  ["postgres", "PostgreSQL", "postgres://reader@db.example.com/orders", 6],
  ["yugabyte", "YugabyteDB", "yugabyte://reader@db.example.com/orders", 6],
  ["mysql", "MySQL / MariaDB", "mariadb://reader@db.example.com/orders", 6],
  ["sqlite", "SQLite", "/tmp/orders.db", 0],
] as const) {
  test(`New ${label} connections expose only their supported security controls`, async ({ page }) => {
    await page.goto("/"); await installTestApi(page);
    await page.getByRole("button", { name: "New connection", exact: true }).click();
    await page.getByRole("button", { name: "Database type", exact: true }).click();
    await page.getByRole("option").filter({ hasText: label }).click();
    await page.getByLabel(engine === "sqlite" ? "Database file" : "Connection string", { exact: true }).fill(input);
    if (engine === "sqlite") {
      await expect(page.getByText("SQLite opens a local file. Network TLS settings do not apply.")).toBeVisible();
      await expect(page.getByLabel("TLS mode", { exact: true })).toHaveCount(0);
    } else {
      await expect(page.getByLabel("TLS mode", { exact: true })).toBeVisible();
      await expect(page.getByLabel("TLS mode", { exact: true }).locator("option")).toHaveCount(options);
    }
  });
}

test("A new connection can recover from TLS failure and override its pasted mode", async ({ page }) => {
  await page.goto("/"); await installTestApi(page);
  await page.getByRole("button", { name: "New connection", exact: true }).click();
  await page.getByRole("button", { name: "Database type", exact: true }).click(); await page.getByRole("option").filter({ hasText: "MySQL / MariaDB" }).click();
  await page.getByLabel("Connection string", { exact: true }).fill("mariadb://reader@db.example.com/orders?ssl-mode=REQUIRED");
  const mode = page.getByLabel("TLS mode", { exact: true });
  await expect(mode).toHaveValue("REQUIRED");
  await page.getByRole("button", { name: "Test", exact: true }).click();
  await page.getByRole("button", { name: "Configure TLS", exact: true }).click();
  await expect(mode).toBeFocused();
  await mode.selectOption("DISABLED");
  await expect(page.getByText("Database traffic will not be encrypted. Choose this only when you trust the network.")).toBeVisible();
  await page.getByRole("button", { name: "Test", exact: true }).click();
  await expect(page.locator(".new-conn .tag.good")).toContainText("MariaDB");
  expect(await page.evaluate(() => (window as unknown as { connectionSettingsTest: { newDrafts: NewConnection[] } }).connectionSettingsTest.newDrafts.at(-1)?.security?.mode)).toBe("DISABLED");
});

test("Testing and cancelling a saved draft preserves it; saving persists the policy and environment", async ({ page }) => {
  await page.goto("/"); await installTestApi(page);
  let dialog = await openSettings(page);
  await dialog.getByLabel("TLS mode", { exact: true }).selectOption("DISABLED");
  await expect(dialog.getByText("Unsaved changes", { exact: true })).toBeVisible();
  await dialog.getByRole("button", { name: "Test connection", exact: true }).click();
  await expect(dialog.getByText("Connection succeeded · MariaDB · orders")).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel", exact: true }).last().click();
  dialog = await openSettings(page);
  await expect(dialog.getByLabel("TLS mode", { exact: true })).toHaveValue("");
  await dialog.getByLabel("TLS mode", { exact: true }).selectOption("DISABLED");
  await dialog.getByRole("radio", { name: "staging", exact: true }).click();
  await dialog.getByRole("button", { name: "Save changes", exact: true }).click();
  await expect(dialog).toBeHidden();
  dialog = await openSettings(page);
  await expect(dialog.getByLabel("TLS mode", { exact: true })).toHaveValue("DISABLED");
  await expect(dialog.getByRole("radio", { name: "staging", exact: true })).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("button", { name: "Edit connection settings for mysql-demo", exact: true })).toBeFocused();
});

test("Editing or closing a draft aborts its pending connection test", async ({ page }) => {
  await page.goto("/"); await installTestApi(page);
  await page.evaluate(() => { (window as unknown as { connectionSettingsTest: { slow: boolean } }).connectionSettingsTest.slow = true; });
  const dialog = await openSettings(page);
  await dialog.getByRole("button", { name: "Test connection", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "Testing…", exact: true })).toBeVisible();
  await dialog.getByLabel("TLS mode", { exact: true }).selectOption("DISABLED");
  await expect.poll(() => page.evaluate(() => (window as unknown as { connectionSettingsTest: { aborted: boolean } }).connectionSettingsTest.aborted)).toBe(true);
  await expect(dialog.getByRole("button", { name: "Test connection", exact: true })).toBeEnabled();
  await dialog.getByRole("button", { name: "Test connection", exact: true }).click();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

test("SQL Server certificate settings and SQLite's file state remain usable on a narrow screen", async ({ page }) => {
  await page.goto("/"); await page.setViewportSize({ width: 420, height: 760 });
  const dialog = await openSettings(page, "mssql-demo");
  await dialog.getByText("Certificate options", { exact: true }).click();
  await dialog.getByLabel("Trust server certificate", { exact: true }).check();
  await dialog.getByLabel("TLS mode", { exact: true }).selectOption("strict");
  await expect(dialog.getByLabel("Trust server certificate", { exact: true })).toBeDisabled();
  await expect(dialog.getByLabel("Trust server certificate", { exact: true })).not.toBeChecked();
  expect(await dialog.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
  await page.keyboard.press("Escape");
  const sqlite = await openSettings(page, "sqlite-demo");
  await expect(sqlite.getByLabel("TLS mode", { exact: true })).toHaveCount(0);
  await expect(sqlite.getByText("Database file", { exact: true })).toBeVisible();
  await expect(sqlite.getByText("Server", { exact: true })).toHaveCount(0);
  await expect(sqlite.getByText("SQLite opens a local file. Network TLS settings do not apply.")).toBeVisible();
});


test("Imported connections retain their mode and cloud connections enforce verified TLS in the editor", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(async () => {
    const apiModule = "/src/lib/api.ts"; const storeModule = "/src/lib/store.ts";
    const { api } = await import(apiModule); const { loadConnections } = await import(storeModule);
    const client = await api();
    await client.importConnections([{ alias: "imported-security", source: "dbeaver", origin: "Orders", file: "data-sources.json", engine: "mysql", environment: "development", host: "db.internal", port: 3306, database: "orders", user: "reader", auth: "password", options: { "ssl-mode": "REQUIRED" }, note: null, saved_as: null }]);
    await client.addDiscovered([{ alias: "cloud-security", cloud: "aws", engine: "postgres", environment: "staging", server: "orders", host: "orders.abc.us-east-1.rds.amazonaws.com", database: "orders", location: "us-east-1", subscription: "test", resource_group: "", port: 5432, user: "reader", region: "us-east-1", auth: "aws-iam", saved_as: null }]);
    await loadConnections();
  });
  let dialog = await openSettings(page, "imported-security");
  await expect(dialog.getByLabel("TLS mode", { exact: true })).toHaveValue("REQUIRED");
  await dialog.getByLabel("TLS mode", { exact: true }).selectOption("DISABLED");
  await dialog.getByRole("button", { name: "Save changes", exact: true }).click();
  await expect(dialog).toBeHidden();
  dialog = await openSettings(page, "imported-security");
  await expect(dialog.getByLabel("TLS mode", { exact: true })).toHaveValue("DISABLED");
  await page.keyboard.press("Escape");
  dialog = await openSettings(page, "cloud-security");
  await expect(dialog.getByText("This authentication method requires encryption with certificate and hostname verification.")).toBeVisible();
  await expect(dialog.getByLabel("TLS mode", { exact: true }).locator('option[value="disable"]')).toBeDisabled();
  await expect(dialog.getByLabel("TLS mode", { exact: true }).locator('option[value="require"]')).toBeDisabled();
});
