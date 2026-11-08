import type { QueryStat } from "../gen/QueryStat";
import type { StatementActivity } from "../gen/StatementActivity";
import type { Report } from "../gen/Report";
import type { Workload } from "../gen/Workload";
import type { WorkloadKind } from "../gen/WorkloadKind";
import { windowSpan } from "./format";
import { t } from "./i18n";

/** What a workload was read from, as its provider said. */
export function workloadKind(w: Workload): WorkloadKind | null {
  return w.kind ?? null;
}

/** Below this, a per-day figure would be a forecast from a sliver of data; show what was measured instead. */
export const PER_DAY_MIN_HOURS = 20;

export type Honest = {
  /** True when values are per day; false when they are totals over the measured span. */
  perDay: boolean;
  /** "per day", or "over the last 2 minutes" / "over the 5 hours since statistics were reset". */
  suffix: string;
  /** Column header for counts: "Runs/day" or "Runs". */
  runsHeader: string;
  value: (total: number) => number;
};

/** Whether nobody can say how long the workload covers: a source that didn't say, or a log without timestamps. The
 * core's `Workload::window_unknown`. */
export function windowUnknown(w: Workload): boolean {
  return !(w.window_hours > 0);
}

/** "the last 7 days", "the 35 minutes since statistics were reset", "the 50 minutes the Rails log covers": the core's phrase. */
export function windowPhrase(w: Workload): string {
  const unknown = windowUnknown(w);
  const span = unknown ? "" : windowSpan(w.window_hours);
  if (w.log != null) return unknown ? t("ui.rates.window.log_period", { log: w.log }) : t("ui.rates.window.log_span", { span, log: w.log });
  if (w.windowed) return unknown ? t("ui.rates.window.recorded") : t("ui.rates.window.last", { span });
  return unknown ? t("ui.rates.window.since_last_reset") : t("ui.rates.window.since_reset", { span });
}

/** How the runs of statements a rate a day would misstate are said, by query id: those that came in bursts ("750 runs
 * on Oct 11, 15:51–15:55 UTC") and those that ran fewer times than the window has days (the core's `Summary::runs_said`,
 * from `Workload::cadence`). */
export function runsSaid(summary: { runs_said: { query: string; runs: string }[] }): Map<string, string> {
  return new Map(summary.runs_said.map((r) => [r.query, r.runs]));
}

/** Per-day rates for windows long enough to mean something, measured totals otherwise. */
export function honest(w: Workload): Honest {
  const { perDay, value } = rate(w);
  return perDay
    ? { perDay, suffix: t("ui.rates.per_day"), runsHeader: t("ui.rates.runs_per_day"), value }
    : { perDay, suffix: t("ui.rates.over", { window: windowPhrase(w) }), runsHeader: t("ui.rates.runs"), value };
}

/** `honest` without its words, for figures worked out per statement. */
function rate(w: Workload): Pick<Honest, "perDay" | "value"> {
  return w.window_hours >= PER_DAY_MIN_HOURS ? { perDay: true, value: (v) => (v * 24) / w.window_hours } : { perDay: false, value: (v) => v };
}

/** A statement's share of database time, runs and average run as every surface states them. */
export type Figures = {
  share: number;
  /** Runs a day where `perDay`, otherwise the runs counted (see `Honest`, `said`). */
  runs: number;
  perDay: boolean;
  /** How its runs are said where a rate a day would misstate them (`Summary::runs_said`): "750 runs on Oct 11,
   * 15:51–15:55 UTC". `runs` is then that count, never a rate. */
  said: string | null;
  mean: number;
  /** "89 hours of Culprit's scans" where the figures are what those scans measured; null: the workload's window. */
  over: string | null;
  /** The totals since the source began counting, where they differ twofold from the scans': "about 214 runs a day over
   * the 1295 days since statistics were reset". */
  totals: string | null;
  activity: StatementActivity | null;
};

type Summary = Report["summary"];
type FigureIndex = {
  scansSource: Summary["over_scans"];
  saidSource: Summary["runs_said"];
  scans: Map<string, Summary["over_scans"][number]>;
  said: Map<string, string>;
};
const figureIndexes = new WeakMap<Summary, FigureIndex>();

function figureIndex(summary: Summary): FigureIndex {
  const cached = figureIndexes.get(summary);
  if (cached && cached.scansSource === summary.over_scans && cached.saidSource === summary.runs_said) return cached;
  const scans = new Map<string, Summary["over_scans"][number]>();
  for (const scan of summary.over_scans) {
    // Preserve the first matching entry, as the earlier array lookup did.
    if (!scans.has(scan.query)) scans.set(scan.query, scan);
  }
  const index = { scansSource: summary.over_scans, saidSource: summary.runs_said, scans, said: runsSaid(summary) };
  figureIndexes.set(summary, index);
  return index;
}

/** What `report` states of statement `q` (culprit-core's `Workload::cost_of`): on a source of running totals, what
 * Culprit's own scans measured where they cover a day or more (`summary.over_scans`), its totals over the window
 * otherwise; runs a rate a day would misstate (a burst, a few runs) as their count and how they came
 * (`summary.runs_said`). Both worked out by the core, never again here. */
export function figures(report: Report, q: QueryStat): Figures {
  const w = report.snapshot.workload!;
  const rates = rate(w);
  const index = figureIndex(report.summary);
  const scans = index.scans.get(q.id);
  const said = index.said.get(q.id) ?? null;
  const share = scans?.share ?? (w.total_time_ms > 0 ? q.total_time_ms / w.total_time_ms : 0);
  const mean = scans?.mean_ms ?? q.mean_time_ms;
  const over = scans?.over ?? null;
  const totals = scans?.totals ?? null;
  const activity = scans?.activity ?? null;
  if (activity) return { share, runs: activity.runs, perDay: false, said: scans!.runs, mean, over, totals, activity };
  // A burst, or fewer runs than days: the count, never a rate.
  if (said != null) return { share, runs: scans?.counted ?? q.executions, perDay: false, said, mean, over, totals, activity };
  if (scans?.runs_per_day != null) return { share, runs: scans.runs_per_day, perDay: true, said, mean, over, totals, activity };
  return { share, runs: rates.value(q.executions), perDay: rates.perDay, said, mean, over, totals, activity };
}
