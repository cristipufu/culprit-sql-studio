import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ConfiguredLogRates } from "../components/ConfiguredLogRates";
import { HealthTab } from "../components/Health";
import { configuredLogRateHealthReport, configuredLogRateReport } from "../test/configured-log-rates";
import { t } from "./i18n";

test("configuration scopes stay folded and render exact Rust text, zero and unknown without pressure grading", () => {
  const report = configuredLogRateReport();
  report.entries[0]!.title = "<img src=x onerror=alert(1)>";
  const html = renderToStaticMarkup(<ConfiguredLogRates report={report} />);
  expect(html).toContain("9007199254740993 B/s");
  expect(html).toContain("0 B/s");
  expect(html).toContain(t("view.configured_log_rates.unknown"));
  expect(html.match(/<article/g)).toHaveLength(3);
  expect(html).not.toContain("<details open");
  expect(html).not.toContain("<img");
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("sig-dot");
  for (const qualification of report.limitations) expect(html).toContain(qualification);
});

test("Health retains configuration-only evidence and source-only failure instead of healthy emptiness", () => {
  for (const view of [configuredLogRateReport(), null]) {
    const html = renderToStaticMarkup(<HealthTab report={configuredLogRateHealthReport(view)} pressure={null} resources={[]} engineLabel="SQL Server" />);
    expect(html).toContain(t("view.configured_log_rates.title"));
    expect(html).not.toContain(t("ui.health.empty"));
    if (view === null) {
      expect(html).toContain(t("ui.dashboard.status.unavailable"));
      expect(html).not.toContain("9007199254740993");
    }
  }
});
