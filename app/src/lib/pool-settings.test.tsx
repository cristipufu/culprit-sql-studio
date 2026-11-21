import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { PoolSettingFields, PoolSettings } from "../components/PoolSettings";
import { databasePoolSettingsReport, poolSettingsHealthReport, poolSettingsReport } from "../test/pool-settings";
import { t } from "./i18n";

test("native pool configuration is fully folded and stays independent of the current scan engine", () => {
  const report = poolSettingsReport();
  const html = renderToStaticMarkup(<HealthTab report={poolSettingsHealthReport(report)} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(report.title); expect(html).toContain(report.summary);
  expect(html).not.toContain("<details open"); expect(html).not.toContain("min_cpu</code>"); expect(html).not.toContain("<dt>Minimum CPU");
  expect(renderToStaticMarkup(<PoolSettings report={report} />)).not.toContain("recorded-elastic-pool");
});
test("six native values preserve zero and unknown while text and extra supplied fields are bounded", () => {
  const report = poolSettingsReport(); report.fields[1]!.explanation = "<img src=x onerror=alert(1)>🌿"; report.fields[2]!.value = "x".repeat(5000); report.fields.push(...report.fields);
  const html = renderToStaticMarkup(<PoolSettingFields report={report} />);
  expect(html.match(/<article/g)).toHaveLength(6); expect(html).toContain(">0%<"); expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain("&lt;img"); expect(html).not.toContain("<img"); expect(html).toContain("🌿"); expect(html).not.toContain("�"); expect(html).not.toContain("x".repeat(513));
});
test("withdrawn native membership is unavailable and cannot render values or a known healthy zero", () => {
  const report = poolSettingsReport(); report.target = null; report.elastic_pool = null; report.elastic_pool_error = null; report.member_database = null; report.logical_server = null; report.context = null; report.native_source = null; report.source_state = "unavailable"; report.fields = []; report.read_complete = false; report.observed_at = null; report.summary = t("view.pool_settings.sharing_withheld");
  const html = renderToStaticMarkup(<PoolSettings report={report} />);
  expect(html).toContain(report.summary); expect(html).toContain(t("ui.dashboard.status.unavailable")); expect(html).not.toContain("recorded-elastic-pool"); expect(html).not.toContain("2048"); expect(html).not.toContain(">0<");
});

test("database settings remain visible when elastic membership is absent or unavailable", () => {
  for (const membership of ["none", "unavailable"] as const) {
    const report = databasePoolSettingsReport(membership);
    const html = renderToStaticMarkup(<HealthTab report={poolSettingsHealthReport(report)} pressure={null} resources={[]} engineLabel="SQL Server" />);
    expect(html).toContain(report.summary);
    expect(report.elastic_pool).toBeNull();
    const fields = renderToStaticMarkup(<PoolSettingFields report={report} />);
    expect(fields.match(/<article/g)).toHaveLength(6);
    expect(fields).toContain(">0%<");
    expect(fields).not.toContain("recorded-elastic-pool");
  }
});
