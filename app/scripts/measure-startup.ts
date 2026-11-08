// Measure an existing production build over loopback, with a fresh browser context per navigation.
// Run after bun run build: bun scripts/measure-startup.ts [number of navigations].
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { gzipSync } from "node:zlib";
import { chromium } from "@playwright/test";

const root = join(import.meta.dir, "..");
const html = readFileSync(join(root, "dist/index.html"), "utf8");
const entry = html.match(/src="([^"]+\.js)"/)?.[1];
if (!entry) throw new Error("Build the UI before measuring startup");
const entryBytes = readFileSync(join(root, "dist", entry));
const server = Bun.spawn(["bunx", "vite", "preview", "--host", "127.0.0.1", "--port", "1422", "--strictPort"], { cwd: root, stdout: "ignore", stderr: "pipe" });
const url = "http://127.0.0.1:1422";
const browser = await chromium.launch();
try {
  let ready = false;
  for (let attempt = 0; attempt < 100; attempt++) {
    if (server.exitCode !== null) throw new Error(await new Response(server.stderr).text());
    try { ready = (await fetch(url)).ok; } catch { /* The owned preview server is still starting. */ }
    if (ready) break;
    await Bun.sleep(100);
  }
  if (!ready) throw new Error("Preview did not start");
  const samples = [];
  for (let run = 0; run < Number(process.argv[2] ?? 5); run++) {
    const context = await browser.newContext({ locale: "en-US" });
    const page = await context.newPage();
    await page.goto(url);
    await page.getByRole("complementary").getByRole("combobox").waitFor();
    samples.push(await page.evaluate(() => ({
      readyMs: performance.now(),
      domContentLoadedMs: (performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming).domContentLoadedEventEnd,
      scripts: performance.getEntriesByType("resource").filter((r) => r.name.endsWith(".js")).length,
    })));
    await context.close();
  }
  console.log(JSON.stringify({ entry: { file: entry, bytes: entryBytes.length, gzipBytes: gzipSync(entryBytes).length }, chunks: readdirSync(join(root, "dist/assets")).filter((f) => f.endsWith(".js")).length, samples }, null, 2));
} finally {
  await browser.close();
  server.kill();
  await server.exited;
}
