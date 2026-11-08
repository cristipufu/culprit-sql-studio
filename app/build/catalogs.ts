import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { normalizePath, type Plugin } from "vite";

type Catalog = Record<string, unknown> & { _meta: Record<string, unknown> };

/** UI sentences plus keys carried by generated engine facts and explicit frontend references. */
export function frontendKeys(source: string, english: Catalog): Set<string> {
  const keys = new Set(Object.keys(english).filter((key) => key.startsWith("ui.") || key.startsWith("common.")));
  const visit = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) visit(path);
      else if (/\.(tsx?|json)$/.test(entry.name) && !entry.name.includes(".test.")) {
        for (const match of readFileSync(path, "utf8").matchAll(/["']([a-z][a-z0-9_.-]+)["']/g)) {
          if (typeof english[match[1]!] === "string") keys.add(match[1]!);
        }
      }
    }
  };
  visit(source);
  return keys;
}

/** Backend prose is rendered by Rust. Its translations and contributor metadata don't belong in browser assets. */
export function frontendCatalog(catalog: Catalog, keys: Set<string>): Catalog {
  const { name, english_name, plural, decimal, group, direction } = catalog._meta;
  return { _meta: { name, english_name, plural, decimal, group, direction }, ...Object.fromEntries(Object.entries(catalog).filter(([key]) => keys.has(key))) };
}

export function uiCatalogs(root: string): Plugin {
  const locales = normalizePath(join(root, "../locales"));
  let keys: Set<string> | undefined;
  return {
    name: "culprit-ui-catalogs",
    enforce: "pre",
    handleHotUpdate({ file }) {
      const path = normalizePath(file);
      if (path.startsWith(`${normalizePath(join(root, "src"))}/`) || path === `${locales}/en.json`) keys = undefined;
    },
    load(id) {
      const path = normalizePath(id.split("?")[0]!);
      if (!path.startsWith(`${locales}/`) || !path.endsWith(".json") || path.slice(locales.length + 1).includes("/")) return;
      const english = JSON.parse(readFileSync(join(locales, "en.json"), "utf8")) as Catalog;
      keys ??= frontendKeys(join(root, "src"), english);
      const catalog = JSON.parse(readFileSync(path, "utf8")) as Catalog;
      return JSON.stringify(frontendCatalog(catalog, keys));
    },
  };
}
