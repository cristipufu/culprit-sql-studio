import { useSyncExternalStore } from "react";
import { remember, rememberedChoice } from "./prefs";

export type Theme = "system" | "light" | "dark";

const KEY = "culprit.theme";

function read(): Theme {
  return rememberedChoice<Theme>(KEY, "system", ["light", "dark"]);
}

function apply(theme: Theme): void {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  remember(KEY, theme === "system" ? null : theme);
}

// One theme for the whole app: every control that shows or changes it reads the same value.
let current: Theme = read();
const listeners = new Set<() => void>();

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function setTheme(next: Theme): void {
  if (next === current) return;
  current = next;
  apply(next);
  listeners.forEach((l) => l());
}

export function useTheme(): [Theme, () => void, (theme: Theme) => void] {
  const theme = useSyncExternalStore(subscribe, () => current, () => current);
  const cycle = () => setTheme(theme === "system" ? "light" : theme === "light" ? "dark" : "system");
  return [theme, cycle, setTheme];
}
