import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ChangeCaptureHistory, CDC_HISTORY_SOURCES, CHANGE_FEED_HISTORY_SOURCES } from "../components/ChangeCaptureHistory";
import { HealthTab } from "../components/Health";
import { cdcHistoryCase, changeFeedHistoryCase, changeCaptureUnavailable } from "../test/change-capture";
import { pageServerReadHealthReport } from "../test/page-server-reads";
import { t } from "./i18n";

function health() { const report = pageServerReadHealthReport(null); report.snapshot.sources = []; return report; }

test("both canonical native histories preserve exact values, separate original clocks, actions and nullable fields", () => {
  const cdc = cdcHistoryCase("cdc_exact");
  const feed = changeFeedHistoryCase("change_feed_exact");
  for (const report of [cdc, feed]) {
    const html = renderToStaticMarkup(<ChangeCaptureHistory title="Saved native history" reports={[report]} />);
    expect(html).toContain(report.summary);
    expect(html).toContain(report.context!.observed_from);
    expect(html).toContain(report.context!.observed_until);
    expect(html).toContain(report.entries[0]!.observed_at);
    for (const entry of report.entries) { expect(html).toContain(entry.title); expect(html).toContain(entry.investigation); expect(html).toContain(entry.verify); }
    expect(html).toContain(t("ui.memory_failures.not_reported"));
    expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
    expect(html).not.toContain("original.invalid");expect(html).not.toContain("original-instance");
    expect(html).not.toContain("00000016:00000058:0001");
  }
  const html = renderToStaticMarkup(<ChangeCaptureHistory title="Original change feed" reports={[feed]} />);
  expect(html).toContain("9007199254740993");
  expect(html).toContain("2019-12-31T23:00:00.999");
  const nulls = changeFeedHistoryCase("change_feed_null");
  const scan = nulls.entries.find((entry) => entry.kind === "scan-session")!;
  expect(scan.fields.find((field) => field.key === "tran_count")!.value).toBeNull();
  expect(scan.fields.find((field) => field.key === "duration")!.value).toBe("0");
});

test("entry and capture lists are independently folded and paged without merging error populations", () => {
  for (const report of [cdcHistoryCase("cdc_many"), changeFeedHistoryCase("change_feed_many")]) {
    expect(report.entries.length).toBeGreaterThan(10);
    const html = renderToStaticMarkup(<ChangeCaptureHistory title="Saved histories" reports={[report]} />);
    expect(html.match(/class="operational-card change-capture-entry"/g)).toHaveLength(10);
    expect(html).toContain(t("ui.termination_history.next"));
    expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  }
});

test("native fields render as bounded escaped text and preserve NULL rather than a fabricated zero", () => {
  const report = cdcHistoryCase("cdc_exact");
  const entry = report.entries[0]!;
  entry.fields[0]!.value = '<img src=x onerror="history_canary()">\n\x1b' + "界".repeat(1024);
  entry.fields[1]!.value = null;
  const html = renderToStaticMarkup(<ChangeCaptureHistory title="Saved histories" reports={[report]} />);
  expect(html).toContain("&lt;img");expect(html).not.toContain("<img");expect(html).not.toContain("\x1b");
  expect(html).not.toContain("界".repeat(513));expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).not.toContain("href=");
});

test("native partial, denied, empty and disabled histories never disappear behind Health healthy-empty", () => {
  for (const name of ["cdc_empty", "cdc_disabled", "cdc_denied", "cdc_partial"] as const) {
    const report = health();report.cdc = [cdcHistoryCase(name)];
    const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
    expect(html).toContain(report.cdc[0]!.summary);expect(html).not.toContain(t("ui.health.empty"));
  }
  for (const name of ["change_feed_empty", "change_feed_denied", "change_feed_partial"] as const) {
    const report = health();report.change_feed = [changeFeedHistoryCase(name)];
    const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
    expect(html).toContain(report.change_feed[0]!.summary);expect(html).not.toContain(t("ui.health.empty"));
  }
});

test("original evidence without an admitted explanation and each source-only outcome stays explicit", () => {
  for (const family of ["cdc", "change_feed"] as const) {
    const report = health();report[family] = undefined;
    report.snapshot.operations[family === "cdc" ? "cdc_history" : "change_feed_history"] = [changeCaptureUnavailable()];
    let html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
    expect(html).toContain(t("view.change_capture.history.missing"));expect(html).not.toContain(t("ui.health.empty"));
    report.snapshot.operations.cdc_history = [];report.snapshot.operations.change_feed_history = [];
    const names = family === "cdc" ? CDC_HISTORY_SOURCES : CHANGE_FEED_HISTORY_SOURCES;
    for (const state of ["empty", "partial", "unavailable"] as const) {
      report.snapshot.sources = [{ source: names[1]!, state, reason: null }];
      html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
      expect(html).toContain(t("view.change_capture.history.missing"));expect(html).not.toContain(t("ui.health.empty"));
    }
  }
});

test("sharing withdrawal keeps missing original scope and has no synthetic native clock", () => {
  const report = cdcHistoryCase("withdrawn");
  expect(report.context).toBeNull();expect(report.entries).toHaveLength(0);expect(report.read_complete).toBe(false);
  const html = renderToStaticMarkup(<ChangeCaptureHistory title="Saved histories" reports={[report]} />);
  expect(html).toContain(report.summary);expect(html).toContain(t("ui.dashboard.status.unavailable"));
  expect(html).not.toContain("2020-01-01");expect(html).not.toContain("1970-01-01");
});

test("Health retains its ordinary empty state when neither family was acquired", () => {
  const report = health();
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(t("ui.health.empty"));
  expect(html).not.toContain(t("view.change_capture.history.missing"));
});
