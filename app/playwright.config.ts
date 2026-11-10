import { defineConfig, devices } from "@playwright/test";

// Interaction tests: the UI in a real browser, against the demo `bun run dev` serves (the fixtures, no database).
// `bun run e2e` runs them; `bunx playwright install --only-shell chromium` fetches the browser once.
const port = process.env.CULPRIT_E2E_PORT ?? "1420";
const url = `http://localhost:${port}`;

export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: "list",
  use: { baseURL: url, trace: "retain-on-failure" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1400, height: 900 } } }],
  webServer: { command: `bun run dev --port ${port}`, url, reuseExistingServer: !process.env.CI, timeout: 60_000 },
});
