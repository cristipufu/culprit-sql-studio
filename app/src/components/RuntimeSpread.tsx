import type { Engine } from "../gen/Engine";
import type { QueryStat } from "../gen/QueryStat";
import type { WorkloadKind } from "../gen/WorkloadKind";
import { count, duration, percent } from "../lib/format";
import { planColor } from "./Trends";
import { serverName, statementMetrics } from "../lib/engines";
import { t } from "../lib/i18n";

/**
 * How a query's run times spread, lane by lane: a plan per lane where the engine keeps plans (SQL Server), the whole
 * statement otherwise. Whisker = fastest to slowest run, band = one standard deviation around the average, dot = the
 * average (or the median where a histogram exists). One log scale for every lane, so plans compare at a glance.
 */

type Lane = {
  key: string;
  label: string;
  /** The label inside a sentence ("plan 3 takes …"). */
  inSentence: string;
  color: string;
  runs: number;
  mean: number;
  min?: number | null;
  max?: number | null;
  stddev?: number | null;
  p50?: number | null;
  p95?: number | null;
  p99?: number | null;
  histogram?: [number, number, number][];
  compiled?: string | null;
};

/** Showplan's `@c = NULL, @t = (2)` as the values that were sniffed: `@t = 2`. */
function tidy(compiled?: string | null): string | null {
  if (!compiled) return null;
  const parts = compiled
    .split(/,\s*(?=@)/)
    .map((p) => p.trim())
    .filter((p) => !/=\s*NULL$/i.test(p))
    .map((p) => p.replace(/=\s*\((.*)\)$/, "= $1"));
  return parts.length ? parts.join(", ") : null;
}

function lanes(q: QueryStat): Lane[] {
  const spread = q.spread;
  if (q.plans.length > 0 && q.plan_history?.kind !== "cache") {
    return q.plans
      .filter((p) => p.executions > 0)
      .map((p, i) => ({
        key: `${p.variant_query ?? ""}-${p.id}`,
        label: p.variant_query ? t("ui.runtime_spread.variant_plan", { id: p.id }) : t("ui.runtime_spread.plan", { id: p.id }),
        inSentence: p.variant_query ? t("ui.runtime_spread.variant_plan_in_sentence", { id: p.id }) : t("ui.runtime_spread.plan_in_sentence", { id: p.id }),
        color: planColor(q.plans.indexOf(p)),
        runs: p.executions,
        mean: p.mean_time_ms,
        min: p.min_time_ms,
        max: p.max_time_ms,
        stddev: p.stddev_time_ms,
        compiled: tidy(p.compiled) ?? (i === 0 ? tidy(q.plan?.props?.Parameters) : null),
      }));
  }
  return [
    {
      key: q.id,
      label: t("ui.runtime_spread.all_runs"),
      inSentence: t("ui.runtime_spread.all_runs_in_sentence"),
      color: planColor(0),
      runs: q.executions,
      mean: q.mean_time_ms,
      min: spread?.min_ms,
      max: q.max_time_ms,
      stddev: q.stddev_time_ms,
      p50: underMax(spread?.p50_ms, q.max_time_ms),
      p95: underMax(spread?.p95_ms, q.max_time_ms),
      p99: underMax(spread?.p99_ms, q.max_time_ms),
      histogram: spread?.histogram as [number, number, number][] | undefined,
    },
  ];
}

/** A percentile never above the slowest run (MySQL reports bucket upper edges, which can overshoot it). */
function underMax(p: number | null | undefined, max: number | null | undefined): number | null | undefined {
  return p != null && max != null && max > 0 ? Math.min(p, max) : p;
}

/** 1, 2 and 5 times each power of ten, for log-axis ticks. */
const STEPS = [0.001, 0.01, 0.1, 1, 10, 100, 1_000, 10_000, 100_000].flatMap((d) => [d, d * 2, d * 5]);
const DECADES = [0.001, 0.01, 0.1, 1, 10, 100, 1_000, 10_000, 100_000];

/** An axis label: round steps read "1 s" and "10 s", not "1.0 s". */
function tick(ms: number): string {
  return duration(ms).replace(/\.0 (s)$/, " $1");
}

function logTicks(lo: number, hi: number): number[] {
  const all = STEPS.filter((t) => t >= lo && t <= hi);
  const decades = all.filter((t) => DECADES.includes(t));
  return decades.length >= 3 ? decades : all.length > 6 ? all.filter((_, i) => i % 2 === 0) : all;
}

function coverage(engine: Engine, hasHistogram: boolean, product?: string, kind?: WorkloadKind | null): string {
  if (kind === "slow-log") return hasHistogram ? t("ui.runtime_spread.slow_log_histogram") : t("ui.runtime_spread.slow_log");
  const metrics = statementMetrics(engine);
  const note = (kind === "plan-cache" ? metrics.runtime_cache_note : null)
    ?? (hasHistogram ? metrics.runtime_histogram_note : null)
    ?? metrics.runtime_note;
  return note.replaceAll("{engine}", serverName(engine, product));
}

function sentence(q: QueryStat, ls: Lane[]): string | null {
  const busy = ls.filter((l) => l.runs >= Math.max(10, q.executions * 0.01));
  if (busy.length >= 2) {
    const fast = busy.reduce((a, b) => (b.mean < a.mean ? b : a));
    const slow = busy.reduce((a, b) => (b.mean > a.mean ? b : a));
    if (slow.mean >= fast.mean * 3) {
      return t("ui.runtime_spread.sentence.plans", {
        fast: fast.label,
        fast_share: percent(fast.runs / q.executions),
        fast_time: duration(fast.mean),
        slow: slow.inSentence,
        slow_time: duration(slow.mean),
        slow_share: percent(slow.runs / q.executions),
      });
    }
  }
  const lane = ls[0];
  if (!lane) return null;
  // "1 in 100" needs a hundred runs to mean anything; fewer get the median alone.
  if (lane.p50 != null && lane.p99 != null && lane.runs >= 100) return t("ui.runtime_spread.sentence.p99", { p50: duration(lane.p50), p99: duration(lane.p99) });
  if (lane.p50 != null && lane.max != null && lane.runs >= 2) return t("ui.runtime_spread.sentence.slowest", { p50: duration(lane.p50), max: duration(lane.max) });
  if (lane.min != null && lane.max != null && lane.max > lane.mean * 3) {
    return lane.stddev != null
      ? t("ui.runtime_spread.sentence.range_deviation", { min: duration(lane.min), max: duration(lane.max), mean: duration(lane.mean), deviation: duration(lane.stddev) })
      : t("ui.runtime_spread.sentence.range", { min: duration(lane.min), max: duration(lane.max), mean: duration(lane.mean) });
  }
  return null;
}

export function RuntimeSpread({
  query,
  engine,
  windowed,
  product,
  kind,
}: {
  query: QueryStat;
  engine: Engine;
  windowed: boolean;
  product?: string;
  kind?: WorkloadKind | null;
}) {
  const ls = lanes(query);
  const values = ls
    .flatMap((l) => [l.min, l.max, l.mean, l.p50, l.p99, ...(l.histogram ?? []).flatMap((h) => [h[0], h[1]])])
    .filter((v): v is number => v != null && v > 0);
  // Nothing beyond an average: no spread to draw.
  if (!ls.some((l) => l.max != null && l.max > l.mean) || values.length < 2) return null;
  const lo = Math.max(Math.min(...values) / 1.5, 0.001);
  const hi = Math.max(...values) * 1.5;
  const pos = (ms: number) => ((Math.log10(Math.max(ms, lo)) - Math.log10(lo)) / (Math.log10(hi) - Math.log10(lo))) * 100;
  const ticks = logTicks(lo, hi);
  const hasHistogram = ls.some((l) => (l.histogram?.length ?? 0) > 0);
  const words = sentence(query, ls);
  return (
    <section className="rs">
      <h3>
        {t("ui.runtime_spread.title")}
        <span className="muted rs-sub">{windowed ? t("ui.runtime_spread.over_window") : t("ui.runtime_spread.since_reset")}</span>
      </h3>
      {words && <p className="rs-sentence">{words}</p>}
      <div className="rs-lanes">
        {ls.map((l) => {
          const histMax = Math.max(...(l.histogram ?? []).map((h) => h[2]), 1);
          const sdLo = l.stddev != null ? Math.max(l.mean - l.stddev, l.min ?? lo, lo) : null;
          const sdHi = l.stddev != null ? Math.min(l.mean + l.stddev, l.max ?? hi) : null;
          // Percentiles of a handful of runs are just the slowest run under another name: mark them only with enough runs.
          const p95 = l.runs >= 20 ? l.p95 : null;
          const p99 = l.runs >= 100 ? l.p99 : null;
          const tail = p95 != null && p99 != null && Math.abs(pos(p99) - pos(p95)) < 7;
          return (
            <div key={l.key} className="rs-lane">
              <div className="rs-label">
                <span className="rs-name">
                  <i style={{ background: l.color }} />
                  {l.label}
                </span>
                <span className="muted">
                  {t("ui.runtime_spread.runs", { runs: count(l.runs) })}{query.executions > 0 && ls.length > 1 ? ` · ${percent(l.runs / query.executions)}` : ""}
                </span>
                {l.compiled && (
                  <span className="rs-compiled" title={t("ui.runtime_spread.compiled_title")}>
                    {t("ui.runtime_spread.compiled_for", { values: l.compiled })}
                  </span>
                )}
              </div>
              <div className="rs-track" style={{ ["--lane" as string]: l.color }}>
                {(l.histogram ?? []).map((h, i) => (
                  <span
                    key={i}
                    className="rs-hist"
                    style={{ left: `${pos(h[0] || lo)}%`, width: `max(2px, calc(${pos(h[1])}% - ${pos(h[0] || lo)}%))`, height: `${Math.max(8, (h[2] / histMax) * 100)}%` }}
                    title={t("ui.runtime_spread.bucket_title", { runs: count(h[2]), from: duration(h[0]), to: duration(h[1]) })}
                  />
                ))}
                {l.min != null && l.max != null && <span className="rs-whisker" style={{ left: `${pos(l.min)}%`, width: `${pos(l.max) - pos(l.min)}%` }} />}
                {sdLo != null && sdHi != null && sdHi > sdLo && <span className="rs-band" style={{ left: `${pos(sdLo)}%`, width: `${pos(sdHi) - pos(sdLo)}%` }} />}
                {l.min != null && <span className="rs-cap" style={{ left: `${pos(l.min)}%` }} title={t("ui.runtime_spread.fastest", { time: duration(l.min) })} />}
                {l.max != null && <span className="rs-cap" style={{ left: `${pos(l.max)}%` }} title={t("ui.runtime_spread.slowest", { time: duration(l.max) })} />}
                {tail ? (
                  <span className="rs-q" style={{ left: `${pos(p99!)}%` }} title={t("ui.runtime_spread.p95_p99_title", { p95: duration(p95!), p99: duration(p99!) })}>
                    p95·99
                  </span>
                ) : (
                  <>
                    {p95 != null && (
                      <span className="rs-q" style={{ left: `${pos(p95)}%` }} title={t("ui.runtime_spread.p95_title", { p95: duration(p95) })}>
                        p95
                      </span>
                    )}
                    {p99 != null && (
                      <span className="rs-q" style={{ left: `${pos(p99)}%` }} title={t("ui.runtime_spread.p99_title", { p99: duration(p99) })}>
                        p99
                      </span>
                    )}
                  </>
                )}
                <span className="rs-dot" style={{ left: `${pos(l.p50 ?? l.mean)}%` }} title={l.p50 != null ? t("ui.runtime_spread.median", { time: duration(l.p50) }) : t("ui.runtime_spread.average", { time: duration(l.mean) })} />
              </div>
              <div className="rs-values">
                <span>{l.min != null ? duration(l.min) : "—"}</span>
                <b>{duration(l.p50 ?? l.mean)}</b>
                <span>{l.max != null ? duration(l.max) : "—"}</span>
              </div>
            </div>
          );
        })}
        <div className="rs-axis">
          <span />
          <div className="rs-ticks">
            {ticks.map((t) => (
              <span key={t} style={{ left: `${pos(t)}%`, transform: `translateX(${pos(t) > 88 ? -100 : pos(t) < 8 ? 0 : -50}%)` }}>
                {tick(t)}
              </span>
            ))}
          </div>
          <span className="muted rs-legend">{hasHistogram ? t("ui.runtime_spread.legend_median") : t("ui.runtime_spread.legend_average")}</span>
        </div>
      </div>
      <p className="rs-note">{coverage(engine, hasHistogram, product, kind)}</p>
    </section>
  );
}
