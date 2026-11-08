// Every language's UI messages must ship in the built app. Backend messages are rendered by Rust.
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { frontendCatalog, frontendKeys } from "../build/catalogs";
import { isLanguageCode } from "../src/lib/language-code";

const root = join(import.meta.dir, "..");
const locales = join(root, "../locales");
const assets = join(root, "dist/assets");
const english = JSON.parse(readFileSync(join(locales, "en.json"), "utf8"));
const keys = frontendKeys(join(root, "src"), english);
const bundle = readdirSync(assets)
  .filter((f) => f.endsWith(".js"))
  .map((f) => readFileSync(join(assets, f), "utf8"))
  .join("\n");
let missing = 0;
for (const file of readdirSync(locales).filter((f) => f.endsWith(".json") && isLanguageCode(f.slice(0, -5)))) {
  const catalog = frontendCatalog(JSON.parse(readFileSync(join(locales, file), "utf8")), keys);
  const absent = [String(catalog._meta.name), ...Object.keys(catalog).filter((k) => k !== "_meta")].filter((s) => !bundle.includes(s));
  if (absent.length) {
    missing += absent.length;
    console.error(`${file}: ${absent.length} not in the bundle, e.g. ${absent.slice(0, 5).join(", ")}`);
  } else {
    console.log(`${file}: bundled (${Object.keys(catalog).length - 1} messages)`);
  }
}
if (missing) process.exit(1);
