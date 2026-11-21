import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { StorageEvidence } from "../components/StorageEvidence";
import { HealthTab } from "../components/Health";
import { storageHealthReport, storageReport } from "../test/storage";
import { t } from "./i18n";

test("Rust storage report preserves exact large bytes and measured zero with folded native rows", () => {
  const html = renderToStaticMarkup(<StorageEvidence report={storageReport()} />);
  expect(html).toContain("9007199254740993");
  expect(html).toContain(">0<");
  expect(html).not.toContain("<details open");
  expect(html.match(/<article/g)).toHaveLength(2);
  expect(html).not.toContain("sig-dot");
});
test("unknown size basis, unavailable values and hostile native names stay separate text-only facts", () => {
  const report = storageReport("unknown");
  report.entries[1]!.title = "<img src=x onerror=alert(1)>";
  const html = renderToStaticMarkup(<StorageEvidence report={report} />);
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain(t("view.storage.basis.unknown"));
  expect(html).not.toContain("<img");
  expect(html).toContain("&lt;img");
});
test("Health renders the shared storage report without an engine condition or healthy empty fallback", () => {
  const data = storageHealthReport(storageReport("partial"));
  const html = renderToStaticMarkup(
    <HealthTab report={data} pressure={null} resources={[]} engineLabel="PostgreSQL" />,
  );
  expect(html).toContain(t("view.storage.title"));
  expect(html).toContain(t("ui.common.read_partial"));
  expect(html).not.toContain(t("ui.health.empty"));
});

test("unavailable storage remains an explicit read gap without invented zero fields or a healthy empty state", () => {
  const native = renderToStaticMarkup(<StorageEvidence report={storageReport("unavailable")} />);
  expect(native).toContain(t("ui.dashboard.status.unavailable"));
  expect(native).not.toContain("<article");
  const data = storageHealthReport(null);
  const health = renderToStaticMarkup(
    <HealthTab report={data} pressure={null} resources={[]} engineLabel="SQL Server" />,
  );
  expect(health).toContain(t("view.storage.title"));
  expect(health).toContain(t("ui.dashboard.status.unavailable"));
  expect(health).not.toContain(t("ui.health.empty"));
});

test("file census is folded and preserves whole original counts and wide bytes before the native row display cap", () => {
  const report = storageReport("file_census_large");
  const summary = report.file_summary!;
  expect(summary.data_file_count).toBe(300);
  expect(summary.data_allocated_bytes).toBe("5277655810867200");
  expect(report.presented_rows).toBe(256);
  const html = renderToStaticMarkup(<StorageEvidence report={report} />);
  expect(html).toContain(summary.title);
  expect(html).toContain("5277655810867200");
  expect(html).toContain("300");
  expect(html).not.toContain("<details open");
  expect(html).toContain("2020-01-01T12:00:00.75Z");
  expect(html).toContain("196608");
  expect(html).not.toContain("sig-dot");
});
test("unavailable and partial file census counts stay unknown while independent native log observations remain", () => {
  for (const name of ["file_census_partial", "file_census_denied"] as const) {
    const report = storageReport(name);
    const summary = report.file_summary!;
    expect(summary.data_file_count).toBeNull();
    expect(summary.log_file_count).toBeNull();
    expect(summary.data_allocated_bytes).toBeNull();
    const html = renderToStaticMarkup(<StorageEvidence report={report} />);
    expect(html).toContain(summary.summary);
    expect(html).toContain(t("ui.memory_failures.not_reported"));
    expect(html).toContain("196608");
    expect(html).toContain("2020-01-01T12:00:00.75Z");
  }
});
test("known file counts survive unknown byte meanings and an independently unavailable DMV log source", () => {
  for (const name of ["file_census_unknown_usage", "file_census_unknown_basis", "file_census_log_denied"] as const) {
    const report = storageReport(name);
    const summary = report.file_summary!;
    expect(summary.data_file_count).toBe(2);
    expect(summary.log_file_count).toBe(1);
    expect(summary.other_file_count).toBe(1);
    const html = renderToStaticMarkup(<StorageEvidence report={report} />);
    expect(html).toContain(summary.title);
    expect(html).toContain(summary.summary);
    if (name === "file_census_log_denied") expect(html).toContain("262144");
    else expect(html).toContain(t("ui.memory_failures.not_reported"));
  }
});
test("original selected database text stays bounded escaped text in the folded census disclosure", () => {
  const report = storageReport("file_census");
  const summary = report.file_summary!;
  const field = summary.fields.find((field) => field.key === "database")!;
  field.value = `<img src=x onerror=alert(1)>\u001b\n${"😀".repeat(600)}`;
  const original = JSON.stringify(report);
  const html = renderToStaticMarkup(<StorageEvidence report={report} />);
  expect(html).not.toContain("<img");
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("\u001b");
  expect(html).not.toContain("😀".repeat(513));
  expect(JSON.stringify(report)).toBe(original);
});
