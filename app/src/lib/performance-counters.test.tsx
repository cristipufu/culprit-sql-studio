import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { PerformanceCounters } from "../components/PerformanceCounters";
import { commonPerformanceCounterReport, performanceCounterHealthReport, performanceCounterReport, EXACT_PERFORMANCE_COUNTER } from "../test/performance-counters";
import spanish from "../../../locales/es.json" with { type: "json" };
import french from "../../../locales/fr.json" with { type: "json" };
import german from "../../../locales/de.json" with { type: "json" };
import { type Catalog, language, registerCatalogs, setLanguage, t } from "./i18n";

test("native group and pool IDs stay folded shared context with a separate unknown-offset statistics clock", () => {
  const report = performanceCounterReport(1);
  const entry = report.entries[0]!;
  const originalClock = entry.observed_at;
  entry.resource_governor = { kind: "workload-group", native_group_id: "3", native_pool_id: "2", statistics_started_without_offset: "2019-12-31T22:00:00.0000000" };
  entry.native_instance = "<img src=x onerror=alert(1)>";
  const html = renderToStaticMarkup(<PerformanceCounters report={report} />);
  for (const key of ["governor_group_id", "governor_pool_id", "governor_statistics_started"]) expect(html).toContain(t(`view.performance_counters.native.${key}`));
  expect(html).toContain("2019-12-31T22:00:00.0000000");
  expect(html).toContain(originalClock!);
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("<details open");
  expect(html).not.toContain("sig-dot");
  expect(report.database).toBeNull();
  expect(report.native_database_id).toBeNull();
  entry.resource_governor = { kind: "resource-pool", native_group_id: null, native_pool_id: null, statistics_started_without_offset: null };
  const unknown = renderToStaticMarkup(<PerformanceCounters report={report} />);
  expect(unknown).toContain(t("view.performance_counters.native.governor_pool_id"));
  expect(unknown).not.toContain(t("view.performance_counters.native.governor_group_id"));
  expect(unknown).toContain(t("view.performance_counters.unknown"));
  expect(unknown).not.toContain("2019-12-31T22:00:00");
  expect(entry.observed_at).toBe(originalClock);
});

test("native performance inputs stay folded, paged and exact without a pressure grade or HTML rendering", () => {
  const report = performanceCounterReport();
  report.entries[0]!.metric.label = "<img src=x onerror=alert(1)>";
  report.entries[0]!.native_object = "<script>secret()</script>";
  const html = renderToStaticMarkup(<PerformanceCounters report={report} />);
  expect(html).toContain(EXACT_PERFORMANCE_COUNTER); expect(html).toContain("0 KiB");
  expect(html.match(/<article/g)).toHaveLength(10); expect(html).not.toContain("<details open");
  expect(html).toContain("&lt;img"); expect(html).toContain("&lt;script"); expect(html).not.toContain("<img"); expect(html).not.toContain("<script");
  expect(html).toContain(t("view.performance_counters.unknown")); expect(html).not.toContain("sig-dot");
  for (const limitation of report.limitations) expect(html).toContain(limitation);
});

test("Health retains counter-only and marker-only evidence without its empty-state branch", () => {
  for (const view of [performanceCounterReport(), null]) {
    const html = renderToStaticMarkup(<HealthTab report={performanceCounterHealthReport(view)} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
    expect(html).toContain(t("view.performance_counters.title")); expect(html).not.toContain(t("ui.health.empty"));
    if (view === null) { expect(html).toContain(t("ui.dashboard.status.unavailable")); expect(html).not.toContain(EXACT_PERFORMANCE_COUNTER); }
  }
});

test("withheld scalar descriptors remain an explanation with no invented zero entries", () => {
  const report = performanceCounterReport(0);
  report.fields_withheld = true; report.summary = t("view.performance_counters.limit.withheld");
  const html = renderToStaticMarkup(<PerformanceCounters report={report} />);
  expect(html).toContain(report.summary); expect(html).not.toContain("<article"); expect(html).not.toContain(EXACT_PERFORMANCE_COUNTER);
});

test("selected counters show their original database context as escaped text without borrowing a row label", () => {
  const report = performanceCounterReport(1);
  const database = "<img src=x onerror=alert(1)>";
  report.population = "selected-database";
  report.database = database;
  report.native_database_id = "7";
  const html = renderToStaticMarkup(<PerformanceCounters report={report} />);
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<img");
  expect(html).toContain(t("view.performance_counters.database", { database, id: "7" }).replace("<", "&lt;").replace(">", "&gt;"));
  expect(html).not.toContain(t("view.performance_counters.scope"));

  report.native_database_id = null;
  report.entries[0]!.native_instance = "some-other-database";
  const unknown = renderToStaticMarkup(<PerformanceCounters report={report} />);
  expect(unknown).toContain(t("view.performance_counters.selected_scope"));
  expect(unknown).not.toContain(t("view.performance_counters.scope"));
  expect(unknown).not.toContain(t("view.performance_counters.database", { database: "some-other-database", id: "7" }));
  expect(report.database).toBe(database);
});

test("language changes reword interface labels without recomputing retained Rust report text", () => {
  const previousLanguage = language();
  try {
    registerCatalogs({ es: spanish as unknown as Catalog, fr: french as unknown as Catalog, de: german as unknown as Catalog });
    setLanguage("en");
    const report = performanceCounterReport(1);
    const originalTitle = report.title;
    const originalSourceLabel = t("view.performance_counters.source");
    for (const code of ["es", "fr", "de"]) {
      setLanguage(code);
      const html = renderToStaticMarkup(<PerformanceCounters report={report} />);
      const sourceLabel = t("view.performance_counters.source");
      expect(sourceLabel).not.toBe(originalSourceLabel); expect(html).toContain(sourceLabel);
      // Backend reanalysis supplies new report wording; the UI renders an existing report verbatim.
      expect(report.title).toBe(originalTitle); expect(html).toContain(originalTitle);
      expect(report.entries[0]!.metric.text).toBe(EXACT_PERFORMANCE_COUNTER);
      expect(html).toContain(EXACT_PERFORMANCE_COUNTER);
    }
  } finally { setLanguage(previousLanguage); }
});


test("Health renders shared and selected counter populations independently with original times and exact text", () => {
  const shared = performanceCounterReport(1);
  const selected = performanceCounterReport(1);
  selected.population = "selected-database";
  selected.database = "original <database>";
  selected.native_database_id = "7";
  selected.context = { kind: "collector", observed_from: "2020-01-01T12:00:00Z", observed_until: "2020-01-01T12:00:01Z" };
  selected.entries[0]!.metric.text = "9007199254740993 B";
  selected.entries[0]!.scope = "database";
  selected.limitations = [t("view.performance_counters.limit.historical")];
  const report = performanceCounterHealthReport(shared);
  report.performance_counters = [shared, selected];
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(html.match(/class="operational-health observation-list"/g)).toHaveLength(2);
  expect(html).toContain(t("view.performance_counters.scope"));
  expect(html).toContain("original &lt;database&gt;");
  expect(html).toContain("2020-01-01T12:00:00Z");
  expect(html).toContain("2020-01-01T12:00:01Z");
  expect(html).toContain("9007199254740993 B");
  expect(html).toContain(EXACT_PERFORMANCE_COUNTER);
  expect(html).toContain(t("view.performance_counters.limit.historical"));
});

test("withdrawn selected counters stay an unavailable selected population alongside independent shared counters", () => {
  const shared = performanceCounterReport(1);
  const selected = performanceCounterReport(0);
  selected.population = "selected-database";
  selected.source_state = "unavailable";
  selected.summary = t("view.performance_counters.sharing_withheld");
  selected.native_counter_rows = null;
  selected.native_observations = null;
  selected.context = null;
  const report = performanceCounterHealthReport(shared);
  report.performance_counters = [shared, selected];
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(html).toContain(selected.summary);
  expect(html).toContain(t("view.performance_counters.selected_scope"));
  expect(html).toContain(t("ui.dashboard.status.unavailable"));
  expect(html).toContain(EXACT_PERFORMANCE_COUNTER);
  const withdrawn = renderToStaticMarkup(<PerformanceCounters report={selected} />);
  expect(withdrawn).not.toContain(t("view.performance_counters.scope"));
  expect(withdrawn).not.toContain("<article");
});

test("an empty counter collection does not itself suppress the Health empty state", () => {
  const report = performanceCounterHealthReport(null);
  report.snapshot.sources = [];
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(html).toContain(t("ui.health.empty"));
  expect(html).not.toContain(t("view.performance_counters.title"));
});

test("Common gauges, totals and the backend fraction stay folded and exact without a health grade or database owner", () => {
  const report = commonPerformanceCounterReport();
  report.entries[2]!.metric.label = "<img src=x onerror=alert(1)>表🦀";
  const original = JSON.stringify(report);
  const html = renderToStaticMarkup(<PerformanceCounters report={report} />);
  expect(html).toContain(EXACT_PERFORMANCE_COUNTER);
  expect(html).toContain("9007199254740993 pages");
  expect(html).toContain(t("view.counter_ratios.percent", { value: "75.00" }));
  expect(html).toContain(t("view.performance_counters.buffer_cache_ratio.explanation"));
  expect(html).toContain(t("view.performance_counters.explain.common_tempdb"));
  expect(html).toContain(t("view.performance_counters.scope"));
  expect(html).toContain("Info Errors");
  expect(html).toContain("&lt;img");
  expect(html).toContain("表🦀");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("<details open");
  expect(html).not.toContain("sig-dot");
  expect(report.database).toBeNull();
  expect(report.native_database_id).toBeNull();
  expect(JSON.stringify(report)).toBe(original);
});

test("Common missing base renders the canonical unknown input without reconstructing or defaulting a percentage", () => {
  const report = commonPerformanceCounterReport(true);
  const html = renderToStaticMarkup(<PerformanceCounters report={report} />);
  expect(html).toContain(t("view.performance_counters.buffer_cache_ratio.title"));
  expect(html).toContain(t("view.counter_ratios.unknown"));
  expect(html).not.toContain(t("view.counter_ratios.percent", { value: "75.00" }));
  expect(html).toContain(EXACT_PERFORMANCE_COUNTER);
  expect(report.ratios!.entries[0]!.issues).toEqual(["missing-base"]);
  expect(report.ratios!.entries[0]!.metrics).toHaveLength(2);
});
