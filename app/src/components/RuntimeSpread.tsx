import type { Engine } from "../gen/Engine";
import type { QueryStat } from "../gen/QueryStat";
import type { WorkloadKind } from "../gen/WorkloadKind";
import type { RuntimeDistribution } from "../gen/RuntimeDistribution";
import type { RuntimeQuantile } from "../gen/RuntimeQuantile";
import { exactSamples, MAX_RUNTIME_BUCKETS, MAX_RUNTIME_DISTRIBUTIONS, quantileLower, quantileUpper, runtimeMilliseconds, sampleCount } from "../lib/runtime-distribution";
import { count, duration, percent } from "../lib/format";
import { planColor } from "./Trends";
import { serverName, statementMetrics } from "../lib/engines";
import { key, t } from "../lib/i18n";

/** Plans, statement statistics and independently observed distributions retain separate lanes and populations. */
type Lane = {
  key: string;
  label: string;
  inSentence: string;
  color: string;
  kind: "plan" | "statement" | "distribution";
  runs?: number;
  mean?: number | null;
  min?: number | null;
  max?: number | null;
  stddev?: number | null;
  p50?: RuntimeQuantile | null;
  p95?: RuntimeQuantile | null;
  p99?: RuntimeQuantile | null;
  observed?: RuntimeDistribution;
  compiled?: string | null;
};
const ORIGIN_LABEL = {
  "statement-summary": key("ui.runtime_spread.origin.summary"),
  "native-histogram": key("ui.runtime_spread.origin.histogram"),
  "captured-runs": key("ui.runtime_spread.origin.samples"),
};
const ISSUE_LABEL = {
  "read-unavailable": key("ui.runtime_spread.issue.unavailable"),
  "invalid-native-values": key("ui.runtime_spread.issue.invalid"),
  "inconsistent-population": key("ui.runtime_spread.issue.population"),
};
function finite(value: number | null | undefined): number | null {
  return value != null && Number.isFinite(value) && value >= 0 ? value : null;
}
function lanes(q: QueryStat): Lane[] {
  const original: Lane[] = q.plans.length > 0 && q.plan_history?.kind !== "cache"
    ? q.plans.filter((p) => p.executions > 0).map((p) => ({
      key: `plan:${p.id}`, kind: "plan", label: q.variant_family ? t("ui.runtime_spread.variant_plan", { id: p.id }) : t("ui.runtime_spread.plan", { id: p.id }),
      inSentence: q.variant_family ? t("ui.runtime_spread.variant_plan_in_sentence", { id: p.id }) : t("ui.runtime_spread.plan_in_sentence", { id: p.id }),
      color: planColor(q.plans.indexOf(p)), runs: finite(p.executions) ?? undefined,
      mean: finite(p.mean_time_ms), min: finite(p.min_time_ms), max: finite(p.max_time_ms), stddev: finite(p.stddev_time_ms),
      compiled: p.compiled ?? (q.plan_id === p.id ? q.plan?.props?.Parameters : null),
    }))
    : [{
      key: `statement:${q.id}`, kind: "statement", label: t("ui.runtime_spread.statement_statistics"), inSentence: "", color: planColor(0),
      runs: finite(q.executions) ?? undefined, mean: finite(q.mean_time_ms), min: finite(q.spread?.min_ms), max: finite(q.max_time_ms), stddev: finite(q.stddev_time_ms),
    }];
  const distributions = q.spread?.distributions ?? [];
  if (distributions.length > MAX_RUNTIME_DISTRIBUTIONS) return original;
  return original.concat(distributions.map((observed, index) => ({
    key: `distribution:${index}:${observed.source}`, kind: "distribution" as const,
    label: t(ORIGIN_LABEL[observed.origin]), inSentence: "", color: planColor(index + 1),
    p50: observed.p50, p95: observed.p95, p99: observed.p99, observed,
  })));
}
function quantileTitle(label: string, value: RuntimeQuantile): string {
  const upper = duration(quantileUpper(value));
  if (value.kind === "sample-value") return t("ui.runtime_spread.quantile.sample", { label, value: upper });
  const lower = quantileLower(value);
  return lower == null
    ? t("ui.runtime_spread.quantile.upper", { label, upper })
    : t("ui.runtime_spread.quantile.bucket", { label, lower: duration(lower), upper });
}
function population(observed: RuntimeDistribution): string {
  const value = observed.population.sample_count;
  if (value == null || sampleCount(value) == null) return t("ui.runtime_spread.population.unknown");
  const samples = exactSamples(value);
  return observed.population.kind === "captured-samples"
    ? t("ui.runtime_spread.population.samples", { samples })
    : t("ui.runtime_spread.population.native", { samples });
}

/** 1, 2 and 5 times each power of ten, for log-axis ticks. */
const STEPS = [0.001, 0.01, 0.1, 1, 10, 100, 1_000, 10_000, 100_000].flatMap((d) => [d, d * 2, d * 5]);
const DECADES = [0.001, 0.01, 0.1, 1, 10, 100, 1_000, 10_000, 100_000];

/** An axis label: round steps read "1 s" and "10 s", not "1.0 s". */
function tick(ms: number): string {
  const seconds = ms / 1000;
  return Number.isInteger(seconds) && seconds >= 1 && seconds < 60 ? t("ui.format.duration.s", { value: String(seconds) }) : duration(ms);
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
  return t(note, { engine: serverName(engine, product) });
}

function sentence(ls: Lane[]): string | null {
  const busy = ls.filter((lane) => lane.kind === "plan" && (lane.runs ?? 0) >= 10 && lane.mean != null);
  if (busy.length >= 2) {
    const fast = busy.reduce((a, b) => (b.mean! < a.mean! ? b : a));
    const slow = busy.reduce((a, b) => (b.mean! > a.mean! ? b : a));
    if (slow.mean! >= fast.mean! * 3) return t("ui.runtime_spread.sentence.observed_plans", {
      fast: fast.label, fast_time: duration(fast.mean), slow: slow.inSentence, slow_time: duration(slow.mean),
    });
  }
  const lane = ls.find((value) => value.kind === "statement");
  if (lane?.min != null && lane.max != null && lane.mean != null && lane.max > lane.mean * 3) return lane.stddev != null
    ? t("ui.runtime_spread.sentence.range_deviation", { min: duration(lane.min), max: duration(lane.max), mean: duration(lane.mean), deviation: duration(lane.stddev) })
    : t("ui.runtime_spread.sentence.range", { min: duration(lane.min), max: duration(lane.max), mean: duration(lane.mean) });
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
  const hasDistributions = (query.spread?.distributions?.length ?? 0) > 0;
  const values = ls.flatMap((lane) => [lane.min, lane.max, lane.mean, quantileUpper(lane.p50), quantileUpper(lane.p95), quantileUpper(lane.p99),
    ...((lane.observed?.histogram.length ?? 0) <= MAX_RUNTIME_BUCKETS ? (lane.observed?.histogram ?? []).flatMap((bucket) => [runtimeMilliseconds(bucket.lower), runtimeMilliseconds(bucket.upper)]) : []),
  ]).filter((value): value is number => value != null && value > 0 && Number.isFinite(value));
  const show = ls.some((lane) => (lane.max != null && lane.mean != null && lane.max > lane.mean) || lane.p50 != null || lane.p95 != null || lane.p99 != null || (lane.observed?.histogram.length ?? 0) > 0);
  const hasIssue = (query.spread?.distributions?.length ?? 0) > MAX_RUNTIME_DISTRIBUTIONS || ls.some((lane) => lane.observed?.issue != null);
  if (!show && !hasIssue) return null;
  const lo = Math.max((values.length ? Math.min(...values) : 1) / 1.5, 0.001);
  const hi = Math.max(...values, lo * 2) * 1.5;
  const pos = (ms: number) => ((Math.log10(Math.max(ms, lo)) - Math.log10(lo)) / (Math.log10(hi) - Math.log10(lo))) * 100;
  const ticks = logTicks(lo, hi);
  const hasHistogram = ls.some((lane) => (lane.observed?.histogram.length ?? 0) > 0);
  const words = sentence(ls);
  return (
    <section className="rs">
      <h3>{t("ui.runtime_spread.title")}<span className="muted rs-sub">{hasDistributions ? t("ui.runtime_spread.populations") : windowed ? t("ui.runtime_spread.over_window") : t("ui.runtime_spread.since_reset")}</span></h3>
      {words && <p className="rs-sentence">{words}</p>}
      {(query.spread?.distributions?.length ?? 0) > MAX_RUNTIME_DISTRIBUTIONS && <p className="rs-note">{t("ui.runtime_spread.issue.population")}</p>}
      <div className="rs-lanes">
        {ls.map((lane) => {
          const histogram = (lane.observed?.histogram.length ?? 0) <= MAX_RUNTIME_BUCKETS ? lane.observed?.histogram ?? [] : [];
          const plotted = histogram.map((bucket) => ({
            lower: runtimeMilliseconds(bucket.lower), upper: runtimeMilliseconds(bucket.upper), samples: sampleCount(bucket.samples), rawSamples: bucket.samples,
          })).filter((bucket): bucket is { lower: number; upper: number; samples: bigint; rawSamples: string } => bucket.lower != null && bucket.upper != null && bucket.upper > bucket.lower && bucket.samples != null && bucket.samples > 0);
          const histMax = Math.max(...plotted.map((bucket) => Number(bucket.samples)), 1);
          const sdLo = lane.mean != null && lane.stddev != null ? Math.max(lane.mean - lane.stddev, lane.min ?? lo, lo) : null;
          const sdHi = lane.mean != null && lane.stddev != null ? Math.min(lane.mean + lane.stddev, lane.max ?? hi) : null;
          const median = quantileUpper(lane.p50);
          const dot = median ?? lane.mean;
          const p95 = quantileUpper(lane.p95);
          const p99 = quantileUpper(lane.p99);
          const closeQuantiles = p95 != null && p99 != null && Math.abs(pos(p95) - pos(p99)) < 9;
          const issue = lane.observed?.issue;
          return (
            <div key={lane.key} className="rs-lane">
              <div className="rs-label">
                <span className="rs-name"><i style={{ background: lane.color }} />{lane.label}</span>
                <span className="muted rs-population">{lane.observed ? population(lane.observed) : lane.runs == null ? "—" : t("ui.runtime_spread.runs", { n: lane.runs, runs: count(lane.runs) })}{lane.kind === "plan" && query.executions > 0 && lane.runs != null ? ` · ${percent(lane.runs / query.executions)}` : ""}</span>
                {lane.compiled && <span className="rs-compiled" title={t("ui.runtime_spread.compiled_title")}>{t("ui.runtime_spread.compiled_for", { values: lane.compiled })}</span>}
                {lane.observed && <span className="muted rs-source" title={lane.observed.source}>{lane.observed.source}</span>}
                {issue && <span className="muted">{t(ISSUE_LABEL[issue])}</span>}
              </div>
              <div className="rs-track" style={{ ["--lane" as string]: lane.color }}>
                {plotted.map((bucket, index) => <span key={index} className="rs-hist" style={{ left: `${pos(bucket.lower || lo)}%`, width: `max(2px, calc(${pos(bucket.upper)}% - ${pos(bucket.lower || lo)}%))`, height: `${Math.max(8, Number(bucket.samples) / histMax * 100)}%` }} title={t("ui.runtime_spread.bucket_observed", { samples: exactSamples(bucket.rawSamples), from: duration(bucket.lower), to: duration(bucket.upper) })} />)}
                {lane.min != null && lane.max != null && <span className="rs-whisker" style={{ left: `${pos(lane.min)}%`, width: `${pos(lane.max) - pos(lane.min)}%` }} />}
                {sdLo != null && sdHi != null && sdHi > sdLo && <span className="rs-band" style={{ left: `${pos(sdLo)}%`, width: `${pos(sdHi) - pos(sdLo)}%` }} />}
                {lane.min != null && <span className="rs-cap" style={{ left: `${pos(lane.min)}%` }} title={t("ui.runtime_spread.fastest", { time: duration(lane.min) })} />}
                {lane.max != null && <span className="rs-cap" style={{ left: `${pos(lane.max)}%` }} title={t("ui.runtime_spread.slowest", { time: duration(lane.max) })} />}
                {closeQuantiles && lane.p95 != null && lane.p99 != null ? <>
                  <span className="rs-q rs-q-tick" style={{ left: `${pos(p95!)}%` }} title={quantileTitle("p95", lane.p95)} />
                  <span className="rs-q" style={{ left: `${pos(p99!)}%` }} title={[quantileTitle("p95", lane.p95), quantileTitle("p99", lane.p99)].join("\n")}>p95·99{lane.p95.kind === "histogram-upper-bound" && lane.p99.kind === "histogram-upper-bound" ? " ≤" : ""}</span>
                </> : ([['p95', lane.p95], ['p99', lane.p99]] as const).map(([label, quantile]) => {
                  const value = quantileUpper(quantile);
                  return quantile != null && value != null ? <span key={label} className="rs-q" style={{ left: `${pos(value)}%` }} title={quantileTitle(label, quantile)}>{label}{quantile.kind === "histogram-upper-bound" ? " ≤" : ""}</span> : null;
                })}
                {dot != null && <span className="rs-dot" style={{ left: `${pos(dot)}%` }} title={lane.p50 != null ? quantileTitle("p50", lane.p50) : t("ui.runtime_spread.average", { time: duration(lane.mean) })} />}
              </div>
              <div className="rs-values"><span>{lane.min != null ? duration(lane.min) : "—"}</span><b>{dot != null ? `${lane.p50?.kind === "histogram-upper-bound" ? "≤ " : ""}${duration(dot)}` : "—"}</b><span>{lane.max != null ? duration(lane.max) : "—"}</span></div>
            </div>
          );
        })}
        <div className="rs-axis"><span /><div className="rs-ticks">{ticks.map((time) => <span key={time} style={{ left: `${pos(time)}%`, transform: `translateX(${pos(time) > 88 ? -100 : pos(time) < 8 ? 0 : -50}%)` }}>{tick(time)}</span>)}</div><span className="muted rs-legend">{hasDistributions ? t("ui.runtime_spread.legend.observed") : t("ui.runtime_spread.legend_average")}</span></div>
      </div>
      <p className="rs-note">{hasDistributions ? t("ui.runtime_spread.population_note") : coverage(engine, hasHistogram, product, kind)}</p>
      {ls.some((lane) => lane.observed?.population.kind === "captured-samples") && <p className="rs-note">{t("ui.runtime_spread.sample_note")}</p>}
    </section>
  );
}
