import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { PerformanceCounters } from "../components/PerformanceCounters";
import { performanceCounterHealthReport, performanceCounterReport, EXACT_PERFORMANCE_COUNTER } from "../test/performance-counters";
import spanish from "../../../locales/es.json" with { type: "json" };
import french from "../../../locales/fr.json" with { type: "json" };
import german from "../../../locales/de.json" with { type: "json" };
import { type Catalog, language, registerCatalogs, setLanguage, t } from "./i18n";

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
