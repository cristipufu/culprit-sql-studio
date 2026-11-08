import { type Catalog, registerCatalogs } from "./i18n";

/**
 * Every language catalog in locales/, found by the build: a new language is a new `locales/<code>.json`, picked up
 * without code. Called once at start-up (tests, which run without the build, read English alone).
 */
export function loadCatalogs(): void {
  const files = import.meta.glob<Catalog>("../../../locales/*.json", { eager: true, import: "default" });
  registerCatalogs(
    Object.fromEntries(Object.entries(files).map(([path, catalog]) => [path.split("/").pop()!.replace(/\.json$/, ""), catalog])),
  );
}
