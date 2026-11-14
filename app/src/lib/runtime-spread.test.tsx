import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { RuntimeSpread } from "../components/RuntimeSpread";
import type { Plan } from "../gen/Plan";
import type { PlanVariant } from "../gen/PlanVariant";
import type { QueryStat } from "../gen/QueryStat";
import type { RuntimeDistribution } from "../gen/RuntimeDistribution";
import { partial } from "../test/fixtures";

function plan(id: string, compiled: string | null): PlanVariant {
  return partial<PlanVariant>({
    id, compiled, executions: 100, total_time_ms: 1000, mean_time_ms: 10, min_time_ms: 1, max_time_ms: 100,
    is_forced: false,
  });
}

function show(query: QueryStat): string {
  return renderToStaticMarkup(<RuntimeSpread query={query} engine="mssql" windowed kind="query-store" />);
}

test("compiled context retains native NULL and quoted commas as escaped text", () => {
  const compiled = "@optional = NULL, @text = (N'first, @fake = NULL'), @markup = (N'<b>private</b>')";
  const markup = show(partial<QueryStat>({ plans: [plan("7", compiled)], executions: 100 }));
  expect(markup).toContain("@optional = NULL");
  expect(markup).toContain("first, @fake = NULL");
  expect(markup).toContain("&lt;b&gt;private&lt;/b&gt;");
  expect(markup).not.toContain("<b>private</b>");
  expect(markup).toContain("compiled for");
  expect(markup).not.toContain("runtime parameter");
});

test("captured plan compilation context stays with its exact plan rather than the first lane", () => {
  const query = partial<QueryStat>({
    plans: [plan("old", null), plan("current", null)], executions: 200, plan_id: "current",
    plan: partial<Plan>({ props: { Parameters: "@optional = NULL, @text = (N'a,b')" } }),
  });
  const markup = show(query);
  const oldLane = markup.slice(markup.indexOf('class="rs-lane"'), markup.indexOf('class="rs-lane"', markup.indexOf('class="rs-lane"') + 1));
  expect(oldLane).not.toContain("@optional");
  expect(markup).toContain("@optional = NULL");
  expect(markup).toContain("a,b");
});

test("unknown compiled context is not borrowed from a plan without a matching identity", () => {
  const markup = show(partial<QueryStat>({
    plans: [plan("7", null)], executions: 100, plan_id: null,
    plan: partial<Plan>({ props: { Parameters: "@unjoined = NULL" } }),
  }));
  expect(markup).not.toContain("@unjoined");
});

const ms = (value: number) => ({ unit: "milliseconds" as const, value });
function histogram(): RuntimeDistribution {
  return {
    source: "native.latency_histogram", origin: "native-histogram", population: { kind: "native-counters", sample_count: "1000" },
    p50: { kind: "histogram-upper-bound", lower: ms(0), upper: ms(2) },
    p95: { kind: "histogram-upper-bound", lower: ms(0), upper: ms(2) },
    p99: { kind: "histogram-upper-bound", lower: ms(500), upper: ms(1000) },
    histogram: [{ lower: ms(0), upper: ms(2), samples: "980" }, { lower: ms(500), upper: ms(1000), samples: "20" }], issue: null,
  };
}
function observed(...distributions: RuntimeDistribution[]): QueryStat {
  return partial<QueryStat>({
    id: "query", plans: [], executions: 1, mean_time_ms: 1, max_time_ms: 1, stddev_time_ms: 0,
    spread: { min_ms: 1, lock_ms: null, distributions },
  });
}

test("native histogram bounds stay visible beyond a different summary maximum without borrowing its sample count", () => {
  const markup = show(observed(histogram()));
  expect(markup).toContain("Native histogram observation");
  expect(markup).toContain("Observed bucket samples: 1,000");
  expect(markup).toContain("p99 bucket: 500 ms to 1.0 s");
  const lane = markup.slice(markup.indexOf("Native histogram observation"));
  expect(lane).not.toContain('title="slowest 1 ms"');
  expect(lane).not.toContain('title="average 1 ms"');
  expect(markup).not.toContain("1 in 100");
  expect(markup).toContain("Native counters may reset independently");
});

test("native summary and later histogram estimates retain separate lanes and unknown populations", () => {
  const summary: RuntimeDistribution = {
    source: "native.summary", origin: "statement-summary", population: { kind: "native-counters", sample_count: null },
    p50: null, p95: null, p99: { kind: "histogram-upper-bound", lower: null, upper: ms(900) }, histogram: [], issue: null,
  };
  const markup = show(observed(summary, histogram()));
  expect(markup).toContain("Native summary quantiles");
  expect(markup).toContain("Sample count unavailable");
  expect(markup).toContain("p99 upper estimate: 900 ms");
  expect(markup).toContain("p99 bucket: 500 ms to 1.0 s");
  const summaryLane = markup.slice(markup.indexOf("Native summary quantiles"), markup.indexOf("Native histogram observation"));
  expect(summaryLane).not.toContain("Observed bucket samples: 1,000");
  expect(summaryLane).not.toContain('class="rs-dot"');
});

test("retained sample percentiles use their own count even when fewer than one hundred runs were retained", () => {
  const sample: RuntimeDistribution = {
    source: "retained.log", origin: "captured-runs", population: { kind: "captured-samples", sample_count: "80" },
    p50: { kind: "sample-value", value: ms(150) }, p95: null, p99: { kind: "sample-value", value: ms(4000) }, histogram: [], issue: null,
  };
  const markup = show(observed(sample));
  expect(markup).toContain("Retained samples: 80");
  expect(markup).toContain("p99 of retained samples: 4.0 s");
  expect(markup).toContain("may omit other executions");
  expect(markup).not.toContain("1 in 100");
  expect(markup).not.toContain("Native histogram observation");
});

test("unavailable populations remain explicit beside valid independent estimates", () => {
  const missing: RuntimeDistribution = {
    source: "native.histogram", origin: "native-histogram", population: { kind: "native-counters", sample_count: null },
    p50: null, p95: null, p99: null, histogram: [], issue: "read-unavailable",
  };
  const markup = show(observed(missing, histogram()));
  expect(markup).toContain("This distribution could not be read");
  expect(markup).toContain("p99 bucket: 500 ms to 1.0 s");
  expect(markup).not.toContain("NaN");
});

test("exact native counts and picosecond bounds survive finite presentation without integer aliasing", () => {
  const huge = histogram();
  huge.population.sample_count = "9007199254740993";
  huge.histogram = [{ lower: { unit: "picoseconds", value: "9007199254740993" }, upper: { unit: "picoseconds", value: "18446744073709551615" }, samples: "9007199254740993" }];
  huge.p50 = huge.p95 = huge.p99 = { kind: "histogram-upper-bound", lower: huge.histogram[0]!.lower, upper: huge.histogram[0]!.upper };
  const markup = show(observed(huge));
  expect(markup).toContain("Observed bucket samples: 9,007,199,254,740,993");
  expect(markup).not.toContain("9,007,199,254,740,992");
  expect(markup).not.toContain("Infinity");
  expect(huge.histogram[0]!.upper).toEqual({ unit: "picoseconds", value: "18446744073709551615" });
});
