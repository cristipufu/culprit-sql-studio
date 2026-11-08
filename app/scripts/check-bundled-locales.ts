// Every language in locales/ must be inside the built app, never read from disk at run time: its name and every one of
// its keys appear in the bundle. Run after `bun run build`: `bun scripts/check-bundled-locales.ts`.
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const locales = join(root, "../locales");
const assets = join(root, "dist/assets");
const bundle = readdirSync(assets)
  .filter((f) => f.endsWith(".js"))
  .map((f) => readFileSync(join(assets, f), "utf8"))
  .join("\n");
let missing = 0;
for (const file of readdirSync(locales).filter((f) => /^[a-z]{2,3}(-[A-Z][A-Za-z]{1,3})?\.json$/.test(f))) {
  const catalog = JSON.parse(readFileSync(join(locales, file), "utf8")) as Record<string, unknown> & { _meta: { name: string } };
  const absent = [catalog._meta.name, ...Object.keys(catalog).filter((k) => k !== "_meta")].filter((s) => !bundle.includes(s));
  if (absent.length) {
    missing += absent.length;
    console.error(`${file}: ${absent.length} not in the bundle, e.g. ${absent.slice(0, 5).join(", ")}`);
  } else {
    console.log(`${file}: bundled (${Object.keys(catalog).length - 1} messages)`);
  }
}
if (missing) process.exit(1);
