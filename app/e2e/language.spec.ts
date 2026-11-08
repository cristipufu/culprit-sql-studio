import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";
import { openConnection } from "./demo";
import type { IndexAdvice } from "../src/gen/IndexAdvice";
import type { ConnectionInfo } from "../src/gen/ConnectionInfo";
import type { Report } from "../src/gen/Report";
import type { Api } from "../src/lib/api";

type AdviceCacheProbeWindow = typeof window & { adviceRequests: string[] };
const pgAdvice = (JSON.parse(readFileSync(new URL("../../fixtures/index-advice.sample.json", import.meta.url), "utf8")) as Record<string, IndexAdvice>)["pg-demo"]!;

test("The language picked in the sidebar is the page's, and stays across a reload", async ({ page }) => {
  await openConnection(page, "pg-demo");
  const picker = page.getByRole("complementary").getByRole("combobox");
  await expect(picker).toHaveValue("en");
  await expect(picker.locator("option")).toHaveText(["English", "Deutsch", "Español", "Français"]);
  await picker.selectOption("es");
  await expect(page.locator("html")).toHaveAttribute("lang", "es");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("lang", "es");
  await expect(page.getByRole("complementary").getByRole("combobox")).toHaveValue("es");
});

// Every catalog the build found, each read as the app shows it: the sidebar's own words come from that file.
const catalog = (code: string) => JSON.parse(readFileSync(new URL(`../../locales/${code}.json`, import.meta.url), "utf8")) as Record<string, string>;

for (const code of ["de", "es", "fr"]) {
  test(`The app reads in ${code} once it is picked`, async ({ page }) => {
    const words = catalog(code);
    await openConnection(page, "pg-demo");
    await page.getByRole("complementary").getByRole("combobox").selectOption(code);
    const sidebar = page.getByRole("complementary");
    const startsWith = (text: string) => new RegExp(`^${text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`);
    await expect(sidebar.getByRole("combobox", { name: words["ui.sidebar.language"] })).toBeVisible();
    await expect(sidebar.getByRole("button", { name: startsWith(words["ui.common.view.findings"]!) })).toBeVisible();
    await expect(sidebar.getByRole("button", { name: startsWith(catalog("en")["ui.common.view.findings"]!) })).toHaveCount(0);
  });
}

test("Index advice belongs to the selected language, including a cached report", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await page.evaluate(async ({ apiPath, languagePath, storePath, advice }) => {
    const requests: string[] = [];
    (window as AdviceCacheProbeWindow).adviceRequests = requests;
    const client = await (await import(apiPath!)).api() as { indexAdvice: (name: string, id: string) => Promise<IndexAdvice> };
    const { language } = await import(languagePath!);
    client.indexAdvice = async (name) => {
      const code = language() as string;
      requests.push(code);
      return { ...advice, connection: name, counters: `advice:${code}` };
    };
    (await import(storePath!)).setState({ view: "schema", advisor: true });
  }, { apiPath: "/src/lib/api.ts", languagePath: "/src/lib/i18n.ts", storePath: "/src/lib/store.ts", advice: pgAdvice });
  await expect(page.locator(".ia")).toContainText("advice:en");
  const requests = () => page.evaluate(() => (window as AdviceCacheProbeWindow).adviceRequests);
  const englishRequests = (await requests()).filter((code) => code === "en").length;
  const picker = page.locator(".language-select");
  await picker.selectOption("fr");
  await expect(page.locator(".ia")).toContainText("advice:fr");
  await expect(page.locator(".ia")).not.toContainText("advice:en");
  await picker.selectOption("en");
  await expect(page.locator(".ia")).toContainText("advice:en");
  expect(new Set(await requests())).toEqual(new Set(["en", "fr"]));
  expect((await requests()).filter((code) => code === "en")).toHaveLength(englishRequests);
});

type LanguageProbe = {
  backend: string;
  requests: string[];
  pending: { code: string; finish: () => void; fail: () => void }[];
};
type ProbeWindow = typeof window & { languageProbe: LanguageProbe };

async function controlledAdvice(page: Page): Promise<void> {
  await openConnection(page, "pg-demo");
  await page.evaluate(async ({ apiPath, storePath, advice }) => {
    const client = await (await import(apiPath!)).api() as Api;
    const probe: LanguageProbe = { backend: "en", requests: [], pending: [] };
    (window as ProbeWindow).languageProbe = probe;
    client.setLanguage = (code) => new Promise<void>((resolve, reject) => {
      const pending: LanguageProbe["pending"][number] = {
        code,
        finish: () => {
          probe.pending = probe.pending.filter((entry) => entry !== pending);
          probe.backend = code;
          resolve();
        },
        fail: () => {
          probe.pending = probe.pending.filter((entry) => entry !== pending);
          reject(new Error("Language application failed"));
        },
      };
      probe.pending.push(pending);
    });
    client.indexAdvice = async (name) => {
      const code = probe.backend;
      probe.requests.push(code);
      return { ...advice, connection: name, counters: `backend-advice:${code}` };
    };
    (await import(storePath!)).setState({ view: "schema", advisor: true });
  }, { apiPath: "/src/lib/api.ts", storePath: "/src/lib/store.ts", advice: pgAdvice });
  await expect(page.locator(".ia")).toContainText("backend-advice:en");
}

async function settleLanguage(page: Page, code: string, fail = false): Promise<void> {
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).languageProbe.pending[0]?.code)).toBe(code);
  await page.evaluate(({ code, fail }) => {
    const pending = (window as ProbeWindow).languageProbe.pending.find((entry) => entry.code === code)!;
    if (fail) pending.fail();
    else pending.finish();
  }, { code, fail });
}

test("Advice waits for language application and skips superseded language choices", async ({ page }) => {
  await controlledAdvice(page);
  const picker = page.locator(".language-select");
  await picker.selectOption("fr");
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).languageProbe.pending[0]?.code)).toBe("fr");
  await expect(page.locator(".ia")).toHaveAttribute("aria-busy", "true");
  expect(await page.evaluate(() => (window as ProbeWindow).languageProbe.requests)).toEqual(["en"]);
  await settleLanguage(page, "fr");
  await expect(page.locator(".ia")).toContainText("backend-advice:fr");
  await page.evaluate(async (path) => (await import(path)).reread(), "/src/lib/store.ts");
  await expect(page.locator(".ia")).toContainText("backend-advice:fr");

  await picker.selectOption("de");
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).languageProbe.pending[0]?.code)).toBe("de");
  await picker.selectOption("es");
  await settleLanguage(page, "de");
  await settleLanguage(page, "es");
  await expect(page.locator(".ia")).toContainText("backend-advice:es");
  expect(await page.evaluate(() => (window as ProbeWindow).languageProbe.requests)).toEqual(["en", "fr", "es"]);

  await picker.selectOption("en");
  await settleLanguage(page, "en");
  await expect(page.locator(".ia")).toContainText("backend-advice:en");
  expect(await page.evaluate(() => (window as ProbeWindow).languageProbe.requests)).toEqual(["en", "fr", "es"]);
});

test("A failed language application cannot cache advice and a later choice recovers", async ({ page }) => {
  await controlledAdvice(page);
  await page.locator(".language-select").selectOption("fr");
  await settleLanguage(page, "fr", true);
  await expect(page.locator(".ia-note")).toContainText("Language application failed");
  expect(await page.evaluate(() => (window as ProbeWindow).languageProbe.requests)).toEqual(["en"]);
  await page.locator(".language-select").selectOption("de");
  await settleLanguage(page, "de");
  await expect(page.locator(".ia")).toContainText("backend-advice:de");
  expect(await page.evaluate(() => (window as ProbeWindow).languageProbe.requests)).toEqual(["en", "de"]);
});

test("Connection takeaways refresh when language changes with no report open", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".conn-card").first()).toBeVisible();
  await page.evaluate(async ([apiPath, languagePath]) => {
    const client = await (await import(apiPath!)).api() as { connections: () => Promise<ConnectionInfo[]> };
    const { language } = await import(languagePath!);
    const original = client.connections;
    client.connections = async () => (await original()).map((connection) => ({
      ...connection,
      last_report: connection.last_report && {
        ...connection.last_report, no_workload: null, takeaway: `takeaway:${language()}`,
      },
    }));
  }, ["/src/lib/api.ts", "/src/lib/i18n.ts"]);
  const picker = page.getByRole("complementary").getByRole("combobox");
  await picker.selectOption("fr");
  await expect(page.locator(".conn-summary").first()).toHaveText("takeaway:fr");
  await picker.selectOption("en");
  await expect(page.locator(".conn-summary").first()).toHaveText("takeaway:en");
});

test("Workload source labels refresh with the report without displaying its counter identity", async ({ page }) => {
  await openConnection(page, "pg-demo");
  await page.evaluate(async ([apiPath, languagePath]) => {
    const client = await (await import(apiPath!)).api() as { latestReport: (name: string, window: number | null) => Promise<Report | null> };
    const { language } = await import(languagePath!);
    const original = client.latestReport;
    client.latestReport = async (name, window) => {
      const report = await original(name, window);
      return report && {
        ...report,
        snapshot: {
          ...report.snapshot,
          workload: report.snapshot.workload && { ...report.snapshot.workload, source: "fixed-counter-source", source_label: `source:${language()}` },
        },
      };
    };
  }, ["/src/lib/api.ts", "/src/lib/i18n.ts"]);
  await page.keyboard.press("3");
  const picker = page.locator(".language-select");
  for (const code of ["fr", "de", "en"]) {
    await picker.selectOption(code);
    await expect(page.getByRole("main")).toContainText(`source:${code}`);
    await expect(page.getByRole("main")).not.toContainText("fixed-counter-source");
  }
});

test("A catalog selects RTL layout while SQL keeps its reading direction", async ({ page }) => {
  await openConnection(page, "pg-demo");
  const rtl = JSON.parse(readFileSync(new URL("../../locales/.tests/rtl.json", import.meta.url), "utf8"));
  await page.evaluate(async ({ path, catalog }) => {
    (await import(path)).registerCatalogs({ ar: catalog });
  }, { path: "/src/lib/i18n.ts", catalog: rtl });
  await page.locator(".language-select").selectOption("ar");
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await expect(page.locator("html")).toHaveAttribute("lang", "ar");
  const sidebar = await page.locator(".sidebar").boundingBox();
  const content = await page.getByRole("main").boundingBox();
  expect(sidebar!.x).toBeGreaterThan(content!.x);
  const editorLabel = await page.evaluate(async (path) => (await import(path)).t("ui.common.view.editor"), "/src/lib/i18n.ts");
  await page.locator(".sidebar").getByRole("button", { name: `${editorLabel} 6`, exact: true }).click();
  await expect(page.locator(".cm-content")).toHaveAttribute("dir", "ltr");
  await page.locator(".cm-content").click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.type("SELECT 'العربية' AS language;");
  await expect(page.locator(".cm-content")).toContainText("SELECT 'العربية' AS language;");
  await test.info().attach("rtl-editor", { body: await page.screenshot(), contentType: "image/png" });
  await page.locator(".language-select").selectOption("en");
  await expect(page.locator("html")).toHaveAttribute("dir", "ltr");
});
