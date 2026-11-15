import { useSyncExternalStore } from "react";
import type { ChangeRequest } from "../gen/ChangeRequest";
import type { ConnectionInfo } from "../gen/ConnectionInfo";
import type { NativeIdentity } from "../gen/NativeIdentity";
import type { Report } from "../gen/Report";
import type { RuntimePopulationCatalog } from "../gen/RuntimePopulationCatalog";
import type { SavedQuery } from "../gen/SavedQuery";
import { api } from "./api";
import { removeEditorTabs, renameEditorTabs } from "./editor-tabs";
import { ReportRequests, type ReportState, withConnectionName } from "./report-requests";
import { LatestRequest } from "./requests";
import { language } from "./i18n";
import { populationKey, samePopulation, type ReportSelection } from "./report-selection";
export type { ReportState } from "./report-requests";

/** `dashboard` is where a connection opens; `overview` is the ranked findings list (titled "Findings"). */
export type View = "dashboard" | "overview" | "queries" | "live" | "schema" | "editor" | "plans" | "connections";

export type State = {
  connections: ConnectionInfo[];
  active: string | null;
  view: View;
  report: ReportState;
  finding: string | null;
  query: string | null;
  /** "What changed" is open, comparing these periods. */
  changes: ChangeRequest | null;
  palette: boolean;
  toast: string | null;
  editorSql: string | null;
  /** A saved query to open in the editor (and run, from the palette). */
  editorSaved: { name: string; run: boolean } | null;
  /** Saved queries, loaded on first use. */
  saved: SavedQuery[] | null;
  /** Open Schema on the index advisor (from the dashboard). */
  advisor: boolean;
  /** Open Schema on this table (`schema.table`, from the palette). */
  schemaTable: string | null;
  dismissed: Record<string, true>;
  windowMinutes: WindowMinutes;
  workloadPopulation: NativeIdentity | null;
  populationCatalog: RuntimePopulationCatalog | null;
};

/** A "What changed" request with only the given fields set; the rest fall back to the defaults. */
export function changeRequest(fields: Partial<ChangeRequest> = {}): ChangeRequest {
  return {
    since: null,
    recent_start: null,
    recent_end: null,
    baseline_start: null,
    baseline_end: null,
    recent_hours: null,
    min_executions: null,
    threshold: null,
    cloud: null,
    workload_population: displayedPopulation(getState()),
    ...fields,
  };
}

/** The workload windows the picker offers, in minutes: an hour, 6 hours, 24 hours, 7 days and 30 days. */
export const WINDOW_OPTIONS = [60, 360, 1440, 10080, 43200] as const;
export type WindowMinutes = (typeof WINDOW_OPTIONS)[number];
const DEFAULT_WINDOW: WindowMinutes = 10080;

const WINDOW_PREFIX = "culprit.window.";
function windowKey(name: string, population: NativeIdentity | null): string {
  return `${WINDOW_PREFIX}${JSON.stringify([name, populationKey(population)])}`;
}

function rememberedWindow(name: string, population: NativeIdentity | null): WindowMinutes {
  try {
    const v = Number(localStorage.getItem(windowKey(name, population)));
    return (WINDOW_OPTIONS as readonly number[]).includes(v) ? (v as WindowMinutes) : DEFAULT_WINDOW;
  } catch {
    return DEFAULT_WINDOW;
  }
}

function populationPreferenceKey(name: string): string {
  return `culprit.population.${name}`;
}

function rememberedPopulation(name: string): NativeIdentity | null {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(populationPreferenceKey(name)) ?? "null");
    if (typeof value !== "object" || value === null || !("id" in value) || typeof value.id !== "string"
      || !("qualifiers" in value) || typeof value.qualifiers !== "object" || value.qualifiers === null
      || Array.isArray(value.qualifiers) || !Object.values(value.qualifiers).every((entry) => typeof entry === "string")) return null;
    return { id: value.id, qualifiers: value.qualifiers as Record<string, string> };
  } catch { return null; }
}

/** Move or remove every exact population's preference without touching report caches or native identities. */
function moveReportPreferences(old: string, next: string | null): void {
  try {
    const keys = Array.from({ length: localStorage.length }, (_, index) => localStorage.key(index)).filter((key): key is string => key !== null);
    for (const key of keys.filter((key) => key.startsWith(WINDOW_PREFIX))) {
      let scope: unknown;
      try { scope = JSON.parse(key.slice(WINDOW_PREFIX.length)); } catch { continue; }
      if (!Array.isArray(scope) || scope.length !== 2 || scope[0] !== old || typeof scope[1] !== "string") continue;
      const value = localStorage.getItem(key);
      if (next !== null && value !== null) localStorage.setItem(`${WINDOW_PREFIX}${JSON.stringify([next, scope[1]])}`, value);
      localStorage.removeItem(key);
    }
    const population = localStorage.getItem(populationPreferenceKey(old));
    if (next !== null && population !== null) localStorage.setItem(populationPreferenceKey(next), population);
    localStorage.removeItem(populationPreferenceKey(old));
  } catch {}
}

const DISMISSED_KEY = "culprit.dismissed";

function loadDismissed(): Record<string, true> {
  try {
    return JSON.parse(localStorage.getItem(DISMISSED_KEY) ?? "{}");
  } catch {
    return {};
  }
}

let state: State = {
  connections: [],
  active: null,
  view: "connections",
  report: { status: "none" },
  finding: null,
  query: null,
  changes: null,
  palette: false,
  toast: null,
  editorSql: null,
  editorSaved: null,
  saved: null,
  advisor: false,
  schemaTable: null,
  dismissed: loadDismissed(),
  windowMinutes: DEFAULT_WINDOW,
  workloadPopulation: null,
  populationCatalog: null,
};

const listeners = new Set<() => void>();

export function getState(): State {
  return state;
}

export function setState(patch: Partial<State> | ((s: State) => Partial<State>)): void {
  const next = typeof patch === "function" ? patch(state) : patch;
  if ((next.active !== undefined && next.active !== state.active)
    || (next.windowMinutes !== undefined && next.windowMinutes !== state.windowMinutes)
    || (next.workloadPopulation !== undefined && !samePopulation(next.workloadPopulation, state.workloadPopulation))) reports.invalidate();
  const catalog = next.report?.status === "ready"
    ? next.report.report.snapshot.runtime_populations ?? next.report.report.snapshot.workload?.population_catalog ?? null
    : next.active !== undefined && next.active !== state.active ? null : state.populationCatalog;
  state = { ...state, ...next, populationCatalog: catalog };
  listeners.forEach((l) => l());
}

export function useStore<T>(select: (s: State) => T): T {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => select(state),
  );
}

const reports = new ReportRequests({ client: api, read: getState, publish: (update) => {
  const { selection: adopted, ...next } = update;
  if (!adopted) { setState(next); return; }
  rememberReportView(adopted.name, adopted.population, adopted.windowMinutes);
  setState({ ...next, workloadPopulation: adopted.population, windowMinutes: adopted.windowMinutes as WindowMinutes, query: null, changes: null });
}, collected: loadConnections, language });

export function invalidateReportRequests(): void {
  reports.invalidate();
}

function selection(name: string): ReportSelection {
  const { windowMinutes, workloadPopulation } = getState();
  return { name, windowMinutes, population: workloadPopulation };
}

function displayedPopulation(current: State): NativeIdentity | null {
  return current.report.status === "ready"
    ? current.report.report.snapshot.workload?.population ?? current.workloadPopulation : current.workloadPopulation;
}

function rememberReportView(name: string, population: NativeIdentity | null, windowMinutes: number): void {
  const current = getState();
  try {
    if (current.active === name) localStorage.setItem(windowKey(name, current.workloadPopulation), String(current.windowMinutes));
    localStorage.setItem(populationPreferenceKey(name), JSON.stringify(population));
    localStorage.setItem(windowKey(name, population), String(windowMinutes));
  } catch {}
}

/** Adopt the exact report returned by verification only while its original displayed report still owns the view. */
export async function showVerifiedReport(before: Report, afterId: string): Promise<void> {
  const current = getState();
  if (current.active !== before.snapshot.connection || current.report.status !== "ready"
    || current.report.report.snapshot.id !== before.snapshot.id) return;
  await reports.showVerified(selection(current.active), before.snapshot.id, afterId);
}
const connectionRequests = new LatestRequest();
const savedRequests = new LatestRequest();

let toastTimer: ReturnType<typeof setTimeout> | undefined;

/** Load (or reload) the saved queries into the store. */
export async function loadSaved(): Promise<SavedQuery[]> {
  const owns = savedRequests.start();
  try {
    const saved = await (await api()).savedQueries();
    if (owns()) setState({ saved });
    return saved;
  } catch {
    if (owns()) setState({ saved: getState().saved ?? [] });
    return getState().saved ?? [];
  }
}

export function toast(message: string): void {
  clearTimeout(toastTimer);
  setState({ toast: message });
  toastTimer = setTimeout(() => setState({ toast: null }), 1800);
}

export async function loadConnections(): Promise<ConnectionInfo[]> {
  const owns = connectionRequests.start();
  const connections = await (await api()).connections();
  if (owns()) setState({ connections });
  return connections;
}

/** Carry the current UI session and its preferences to the saved alias without changing the selected view. */
export async function renameConnection(old: string, next: string): Promise<ConnectionInfo> {
  const saved = await (await api()).renameConnection(old, next);
  connectionRequests.invalidate();
  const current = getState();
  const active = current.active === old;
  const previous = current.connections.find((c) => c.name === old) ?? { ...saved, name: old };
  const tabError = renameEditorTabs(previous, saved);
  if (tabError) toast(tabError);
  try {
    if (active) localStorage.setItem(windowKey(old, current.workloadPopulation), String(current.windowMinutes));
  } catch {}
  if (old !== saved.name) moveReportPreferences(old, saved.name);
  if (active || lastConnection() === old) rememberLast(saved.name);
  const resume = active && (current.report.status === "loading" || (current.report.status === "ready" && current.report.scanning));
  const connections = current.connections.some((c) => c.name === old)
    ? current.connections.map((c) => c.name === old ? saved : c)
    : [...current.connections.filter((c) => c.name !== saved.name), saved];
  setState({ connections, ...(active ? {
    active: saved.name,
    report: current.report.status === "ready"
      ? { ...current.report, report: withConnectionName(current.report.report, saved.name) }
      : current.report,
  } : {}) });
  if (resume) void reports.open(selection(saved.name), "refresh");
  // A failed metadata refresh must not turn a successful rename into a retry against the retired alias.
  void loadConnections().catch(() => {});
  return saved;
}

/** Forget a saved connection, its keychain secret and this device's scratch tabs; leave the workspace if it was open.
 * Saved reports stay on disk. */
export async function removeConnection(name: string): Promise<void> {
  const removed = getState().connections.find((c) => c.name === name);
  try {
    await (await api()).removeConnection(name);
  } catch (error) {
    // Show what is saved now: a connection the CLI or another window already removed is gone, not an error.
    const saved = await loadConnections().catch(() => null);
    if (!saved || saved.some((c) => c.name === name)) throw error;
  }
  connectionRequests.invalidate();
  const tabError = removed ? removeEditorTabs(removed) : null;
  if (tabError) toast(tabError);
  try {
    if (lastConnection() === name) localStorage.removeItem(LAST_KEY);
  } catch {}
  moveReportPreferences(name, null);
  setState((s) => ({
    connections: s.connections.filter((c) => c.name !== name),
    ...(s.active === name
      ? { active: null, view: "connections", report: { status: "none" }, finding: null, query: null, changes: null, editorSql: null, editorSaved: null, workloadPopulation: null }
      : {}),
  }));
  void loadConnections().catch(() => {});
}

/** Open a connection: show its last report for the remembered window instantly, then refresh it in place. */
export async function openConnection(name: string, options: { rescan?: boolean } = {}): Promise<void> {
  const rescan = options.rescan ?? true;
  const workloadPopulation = rememberedPopulation(name);
  const windowMinutes = rememberedWindow(name, workloadPopulation);
  setState({ active: name, view: "dashboard", finding: null, query: null, changes: null, editorSql: null, editorSaved: null, report: { status: "loading" }, windowMinutes, workloadPopulation });
  rememberLast(name);
  await reports.open(selection(name), rescan ? "refresh" : "open");
}

/** Switch the workload window (Query Store): cached report for that window if there is one, then a fresh scan. */
export async function setWindow(minutes: WindowMinutes): Promise<void> {
  const name = getState().active;
  if (!name || minutes === getState().windowMinutes) return;
  rememberReportView(name, getState().workloadPopulation, minutes);
  setState({ windowMinutes: minutes, report: { status: "loading" }, finding: null, query: null, changes: null });
  await reports.open(selection(name), "refresh");
}

/** Choose one exact source population; automatic acquisition has already filled its independent saved cache. */
export async function setPopulation(name: string, population: NativeIdentity | null): Promise<void> {
  const current = getState();
  if (name !== current.active || samePopulation(current.workloadPopulation, population)) return;
  if (population !== null && !current.populationCatalog?.populations.some((entry) => samePopulation(entry.identity, population))) return;
  const windowMinutes = rememberedWindow(name, population);
  rememberReportView(name, population, windowMinutes);
  setState({ workloadPopulation: population, windowMinutes, report: { status: "loading" }, finding: null, query: null, changes: null });
  await reports.open(selection(name), "open");
}

/** Read the open connection's report again, without scanning: reports are worded as they are read. */
export async function reread(): Promise<void> {
  const { active } = getState();
  if (active) await reports.open(selection(active), "reread");
}

/** Refresh only the current report session; a stale view cannot start work for a different connection. */
export async function scan(name: string): Promise<void> {
  await reports.scan(selection(name));
}

export function toggleDismissed(id: string): void {
  setState((s) => {
    const dismissed = { ...s.dismissed };
    if (dismissed[id]) delete dismissed[id];
    else dismissed[id] = true;
    try {
      localStorage.setItem(DISMISSED_KEY, JSON.stringify(dismissed));
    } catch {}
    return { dismissed };
  });
}

const LAST_KEY = "culprit.last";

function rememberLast(name: string): void {
  try {
    localStorage.setItem(LAST_KEY, name);
  } catch {}
}

export function lastConnection(): string | null {
  try {
    return localStorage.getItem(LAST_KEY);
  } catch {
    return null;
  }
}

export function currentReport(s: State): Report | null {
  return s.report.status === "ready" ? s.report.report : null;
}
