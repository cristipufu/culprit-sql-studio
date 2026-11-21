import { expect, test } from "@playwright/test";
import english from "../../locales/en.json" with { type: "json" };
import german from "../../locales/de.json" with { type: "json" };
import { openConnection, waitForReport } from "./demo";

test("real history action imports a bounded Rust-derived artifact and folds/pages it by keyboard without Health actions", async ({
  page,
}, info) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.locator('[data-tile="health"]').click();
  const intake = page.getByRole("region", { name: english["view.incident_history.import.section"], exact: true });
  await intake.getByRole("button", { name: english["view.incident_history.import.open"], exact: true }).click();
  const history = intake.getByRole("region", { name: english["view.incident_history.title"], exact: true });
  await expect(history).toBeVisible();
  await expect(history).toContainText(english["view.incident_history.limit.imported"]);
  await expect(history.locator("details[open]")).toHaveCount(0);
  const entries = history.locator(":scope > details").last().locator(":scope > summary");
  await entries.focus();
  await page.keyboard.press("Enter");
  await expect(history.locator("article")).toHaveCount(10);
  const next = history.getByRole("button", { name: english["ui.termination_history.next"], exact: true });
  await next.focus();
  await page.keyboard.press("Space");
  await expect(history.locator("article")).toHaveCount(3);
  const firstRevision = history.locator("article").first();
  const originalIdentity = firstRevision.getByText(english["view.incident_history.field.episode"], { exact: true });
  await expect(originalIdentity).toBeHidden();
  await firstRevision.getByText(english["view.incident_history.evidence.details"], { exact: true }).click();
  await expect(originalIdentity).toBeVisible();
  await firstRevision.getByText(english["view.incident_history.evidence.details"], { exact: true }).click();
  await expect(history.locator(".sig-dot, .sev, button.danger")).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  await history.scrollIntoViewIfNeeded();
  expect(await history.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  await page.screenshot({ path: info.outputPath("selected-history-narrow.png") });
});

test("replaced imports cannot publish stale reports or failures", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.locator('[data-tile="health"]').click();
  await page.evaluate(
    async ({ apiPath, fixturePath }) => {
      const { api } = (await import(apiPath)) as typeof import("../src/lib/api");
      const { incidentHistoryCase } = (await import(fixturePath)) as typeof import("../src/test/incident-history");
      const client = await api();
      let calls = 0;
      client.openIncidentHistory = async () => {
        const call = ++calls;
        await new Promise((resolve) => setTimeout(resolve, call === 1 ? 200 : 10));
        const report = incidentHistoryCase(call === 1 ? "ongoing" : "resolved");
        report.summary = call === 1 ? "STALE_IMPORT" : "SELECTED_IMPORT";
        return report;
      };
    },
    { apiPath: "/src/lib/api.ts", fixturePath: "/src/test/incident-history.ts" },
  );
  const intake = page.getByRole("region", { name: english["view.incident_history.import.section"], exact: true });
  await intake.getByRole("button").click();
  await intake.getByRole("button").click();
  await expect(intake).toContainText("SELECTED_IMPORT");
  await page.waitForTimeout(250);
  await expect(intake).not.toContainText("STALE_IMPORT");
});


test("language changes retire both the selected artifact and pending imports", async ({ page }) => {
  await openConnection(page, "mssql-demo");
  await waitForReport(page);
  await page.locator('[data-tile="health"]').click();
  await page.evaluate(
    async ({ apiPath, fixturePath, languagePath }) => {
      const { api } = (await import(apiPath)) as typeof import("../src/lib/api");
      const { incidentHistoryCase } = (await import(fixturePath)) as typeof import("../src/test/incident-history");
      const { language } = (await import(languagePath)) as typeof import("../src/lib/i18n");
      const client = await api();
      let calls = 0;
      client.openIncidentHistory = async () => {
        const call = ++calls;
        const code = language();
        await new Promise((resolve) => setTimeout(resolve, call === 2 ? 200 : 0));
        const report = incidentHistoryCase("resolved");
        report.summary = `ARTIFACT:${code}:${call}`;
        return report;
      };
    },
    { apiPath: "/src/lib/api.ts", fixturePath: "/src/test/incident-history.ts", languagePath: "/src/lib/i18n.ts" },
  );
  const intake = page.getByRole("region", { name: english["view.incident_history.import.section"], exact: true });
  await intake.getByRole("button").click();
  await expect(intake).toContainText("ARTIFACT:en:1");
  await intake.getByRole("button", { name: english["view.incident_history.import.open"], exact: true }).click();
  await page.locator(".language-select").selectOption("de");
  const translated = page.getByRole("region", { name: german["view.incident_history.import.section"], exact: true });
  await expect(translated).toBeVisible();
  await page.waitForTimeout(250);
  await expect(translated).not.toContainText("ARTIFACT:en:");
  await translated.getByRole("button").click();
  await expect(translated).toContainText("ARTIFACT:de:3");
});
