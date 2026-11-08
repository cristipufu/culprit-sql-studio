import { useEffect, useState, useSyncExternalStore } from "react";
import type { ChangeEvent } from "../gen/ChangeEvent";
import type { CloudSignals } from "../gen/CloudSignals";
import type { EventKind } from "../gen/EventKind";
import type { EventLog } from "../gen/EventLog";
import type { EventsRequest } from "../gen/EventsRequest";
import { api, errorMessage, type ApiError } from "./api";
import { dateLocale } from "./format";
import { t } from "./i18n";

/** What each kind of change is called in a legend or a chip, read in the app's language each time it is shown. */
export const KIND_LABEL: Record<EventKind, string> = {
  get mark() {
    return t("ui.events.kind.mark");
  },
  get index() {
    return t("ui.events.kind.index");
  },
  get table() {
    return t("ui.events.kind.table");
  },
  get code() {
    return t("ui.events.kind.code");
  },
  get setting() {
    return t("ui.events.kind.setting");
  },
  get version() {
    return t("ui.events.kind.version");
  },
  get restart() {
    return t("ui.events.kind.restart");
  },
  get "stats-reset"() {
    return t("ui.events.kind.stats_reset");
  },
  get statistics() {
    return t("ui.events.kind.statistics");
  },
  get throttle() {
    return t("ui.events.kind.throttle");
  },
};

const CLOCKS = {
  clock: { hour: "numeric", minute: "2-digit" },
  dayClock: { weekday: "short", hour: "numeric", minute: "2-digit" },
  dateClock: { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" },
} satisfies Record<string, Intl.DateTimeFormatOptions>;
const clocks = new Map<string, Intl.DateTimeFormat>();

/** A moment as `kind` writes it, in the app's language (one formatter per language and kind). */
function clock(kind: keyof typeof CLOCKS, at: number): string {
  const locale = dateLocale();
  const key = `${kind} ${locale}`;
  let format = clocks.get(key);
  if (!format) {
    format = new Intl.DateTimeFormat(locale, CLOCKS[kind]);
    clocks.set(key, format);
  }
  return format.format(at);
}

/** "Tue 14:05" within the week, "Oct 3, 14:05" before. */
export function stamp(at: number): string {
  return clock(Date.now() - at < 6 * 86_400_000 ? "dayClock" : "dateClock", at);
}

/** When an event happened, in local time and as exactly as its source knows. */
export function eventTime(e: ChangeEvent): string {
  const at = Date.parse(e.at);
  if (e.after) return t("ui.events.between_scans", { from: stamp(Date.parse(e.after)), to: clock("clock", at) });
  if (e.until) return `${stamp(at)}–${clock("clock", Date.parse(e.until))}`;
  return stamp(at);
}

/** Earliest and latest moment it may have happened (or the episode's span), in ms. */
export function eventSpan(e: ChangeEvent): { from: number; to: number } {
  const at = Date.parse(e.at);
  return { from: e.after ? Date.parse(e.after) : at, to: e.until ? Date.parse(e.until) : at };
}

// Marks change from several places (the chart, the palette, What changed); anything showing events refetches.
let version = 0;
const listeners = new Set<() => void>();
export function eventsChanged(): void {
  version += 1;
  listeners.forEach((l) => l());
}
export function useEventsVersion(): number {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => version,
  );
}

export function eventsRequest(fields: Partial<EventsRequest> = {}): EventsRequest {
  return { start: null, end: null, hours: null, cloud: null, refresh: false, ...fields };
}

/** Events between `from` and `to` (ms). Saved scans and marks answer at once; cloud metrics follow when they arrive. */
export function useEvents(connection: string | null, from: number, to: number): EventLog | null {
  const [log, setLog] = useState<EventLog | null>(null);
  const v = useEventsVersion();
  // Minutes are precise enough for a chart, and keep one fetch per render burst.
  const start = Number.isFinite(from) ? new Date(Math.floor(from / 60_000) * 60_000).toISOString() : null;
  const end = Number.isFinite(to) ? new Date(Math.ceil(to / 60_000) * 60_000).toISOString() : null;
  useEffect(() => {
    if (!connection || !start || !end || start >= end) return;
    let cancelled = false;
    const load = (cloud: boolean) =>
      api()
        .then((client) => client.events(connection, eventsRequest({ start, end, cloud })))
        .then((next) => !cancelled && setLog(next))
        .catch(() => {});
    // Local history first, then again with the cloud's metrics (seconds on the first call, cached after).
    void load(false).then(() => load(true));
    return () => {
      cancelled = true;
    };
  }, [connection, start, end, v]);
  return log;
}

export type CloudState = { status: "loading" } | { status: "ready"; signals: CloudSignals } | { status: "none" } | { status: "error"; error: ApiError };

/** What the cloud measured over the last `hours`; `none` when the connection isn't a cloud database. */
export function useCloud(connection: string, hours: number, refresh: number): CloudState {
  const request = JSON.stringify([connection, hours]);
  const [state, setState] = useState<{ request: string; value: CloudState }>({ request, value: { status: "loading" } });
  useEffect(() => {
    let cancelled = false;
    setState((s) => (s.request === request && s.value.status === "ready" ? s : { request, value: { status: "loading" } }));
    api()
      .then((client) => client.cloud(connection, eventsRequest({ hours, refresh: refresh > 0 })))
      .then((signals) => !cancelled && setState({ request, value: { status: "ready", signals } }))
      .catch((e) => {
        if (cancelled) return;
        const error = errorMessage(e);
        setState({ request, value: error.code === "unsupported" ? { status: "none" } : { status: "error", error } });
      });
    return () => {
      cancelled = true;
    };
  }, [connection, hours, refresh, request]);
  return state.request === request ? state.value : { status: "loading" };
}
