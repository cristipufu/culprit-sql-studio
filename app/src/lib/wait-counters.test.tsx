import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { WaitCounters } from "../components/WaitCounters";
import type { Pressure } from "../gen/Pressure";
import type { Report } from "../gen/Report";
import type { WaitCounterReport } from "../gen/WaitCounterReport";
import { partial } from "../test/fixtures";
import { t } from "./i18n";
import { healthReportFixture } from "../test/health-report";

function report(): WaitCounterReport {
  return {
    context: { kind: "collector", observed_from: "2026-11-17T12:00:00.000000001Z", observed_until: "2026-11-17T12:00:00.000000009Z" },
    sources: [{ source: "waits", state: "partial", reason: "Original native visibility is partial." }],
    read_complete: false, native_rows: 10, observed_rows: 1, presented_rows: 1, entries_limited: false,
    title: t("view.wait_counters.title"), summary: t("view.wait_counters.summary.partial"),
    limitations: [t("view.wait_counters.limit.client_clock"), t("view.wait_counters.limit.read")],
    entries: [{
      observation: { source: "waits", wait_type: "<img src=x onerror=alert(1)>", mechanism: null, scope: "server", reset: { kind: "unknown" },
        waiting_tasks_count: "9007199254740993", wait_time_ms: "1", signal_wait_time_ms: "2", max_wait_time_ms: "7" },
      resource_wait_time_ms: null, issues: ["signal-exceeds-total"],
      title: "<img src=x onerror=alert(1)>", explanation: t("view.wait_counters.issue.signal_exceeds_total"),
      metrics: [
        { key: "view.wait_counters.metric.tasks", label: t("view.wait_counters.metric.tasks"), value: 0, unit: "text", text: "9007199254740993" },
        { key: "view.wait_counters.metric.resource", label: t("view.wait_counters.metric.resource"), value: 0, unit: "text", text: t("view.wait_counters.unknown") },
      ],
    }],
  };
}

test("wait details are folded and render exact counts, acquisition nanoseconds and inconsistent raw values as text", () => {
  const html = renderToStaticMarkup(<WaitCounters report={report()} />);
  expect(html).toContain("9007199254740993"); expect(html).not.toContain("9007199254740992");
  expect(html).toContain("2026-11-17T12:00:00.000000001Z"); expect(html).toContain("2026-11-17T12:00:00.000000009Z");
  expect(html).toContain(t("view.wait_counters.issue.signal_exceeds_total"));
  expect(html).toContain(t("view.wait_counters.unknown"));
  expect(html).toContain(t("view.wait_counters.limit.client_clock"));
  expect(html).not.toContain("<details open");
  expect(html).toContain("&lt;img"); expect(html).not.toContain("<img");
});

test("empty, unavailable and partial body states retain their distinct shared explanations", () => {
  const empty = report(); empty.entries = []; empty.native_rows = 0; empty.observed_rows = 0; empty.presented_rows = 0;
  empty.read_complete = true; empty.sources = [{ source: "waits", state: "empty", reason: null }];
  empty.summary = t("view.wait_counters.summary.empty");
  const emptyHtml = renderToStaticMarkup(<WaitCounters report={empty} />);
  expect(emptyHtml).toContain(t("view.wait_counters.summary.empty"));
  expect(emptyHtml).not.toContain("9007199254740993");
  const denied = { ...empty, summary: t("view.wait_counters.summary.partial"),
    sources: [{ source: "waits", state: "unavailable" as const, reason: t("view.wait_counters.source.unavailable") }] };
  const deniedHtml = renderToStaticMarkup(<WaitCounters report={denied} />);
  expect(deniedHtml).toContain(t("view.wait_counters.source.unavailable"));
  expect(deniedHtml).not.toContain(t("view.wait_counters.summary.empty"));
  const partialHtml = renderToStaticMarkup(<WaitCounters report={report()} />);
  expect(partialHtml).toContain(t("ui.common.read_partial"));
  expect(partialHtml).toContain(t("view.wait_counters.limit.read"));
});

test("missing acquisition authority never borrows a native clock from another response", () => {
  const point = report(); point.context = null;
  const html = renderToStaticMarkup(<WaitCounters report={point} />);
  expect(html).not.toContain("2026-11-17T12:00:00.000000001Z");
  expect(html).toContain("9007199254740993");
});

function waitOnlyHealthReport(): Report {
  return healthReportFixture({
    snapshot: {
      id: "wait-only-snapshot", server: { engine: "mssql" }, pressure: null, sources: [],
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null },
    },
  });
}

function renderHealth(data: Report): string {
  return renderToStaticMarkup(<HealthTab report={data} pressure={data.snapshot.pressure} resources={[]} engineLabel="SQL Server" />);
}

test("Health renders wait-only evidence when no other pressure, resources or operational facts are present", () => {
  const data = waitOnlyHealthReport();
  const view = report();
  data.wait_counters = view;
  const html = renderHealth(data);
  expect(html).toContain(t("view.wait_counters.title"));
  expect(html).toContain(view.summary);
  expect(html).toContain("9007199254740993");
  expect(html).toContain(t("view.wait_counters.limit.read"));
  expect(html).not.toContain(t("ui.health.empty"));
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
});

test("Health keeps raw wait evidence visible when its exact explanation is unavailable", () => {
  const data = waitOnlyHealthReport();
  const view = report();
  data.snapshot.pressure = partial<Pressure>({
    signals: [],
    wait_counters: {
      pool_current: null,
      context: view.context,
      sources: [{ source: "waits", state: "partial", records: 10, complete_scope: false, error_code: null, stopped: null }],
      observations: view.entries.map((entry) => entry.observation), read_complete: false,
    },
  });
  const html = renderHealth(data);
  expect(html).toContain(t("view.wait_counters.title"));
  expect(html).toContain(t("view.wait_counters.invalid"));
  expect(html).not.toContain(t("ui.health.empty"));
  expect(html).not.toContain("9007199254740993");
  expect(html).not.toContain("<article");
});

test("Health preserves the wait source marker after sharing has withheld the original body", () => {
  const data = waitOnlyHealthReport();
  data.snapshot.sources = [{
    source: "waits", state: "unavailable", reason: t("view.wait_counters.invalid"),
    reason_message: { kind: "catalog", key: "view.wait_counters.invalid", arguments: {} },
  }];
  const html = renderHealth(data);
  expect(html).toContain(t("view.wait_counters.title"));
  expect(html).toContain(t("view.wait_counters.invalid"));
  expect(html).toContain(t("ui.dashboard.status.unavailable"));
  expect(html).not.toContain(t("ui.health.empty"));
  expect(html).not.toContain("9007199254740993");
  expect(html).not.toContain("<article");
});

test("pool scope and native-tier refusal stay folded in shared wait details and unavailable never becomes Health empty", () => {
  const view = report();
  const entry = view.entries[0];
  if (!entry) throw new Error("The fixture must contain an observed wait");
  entry.observation.scope = "pool";
  entry.metrics.push({ key: "scope", label: t("view.wait_counters.metric.scope"), value: 0, unit: "text", text: t("view.wait_counters.scope.pool") });
  view.limitations = [t("view.pool_waits.scope", { tier: "General Purpose" }), t("view.pool_waits.original")];
  const html = renderToStaticMarkup(<WaitCounters report={view} />);
  expect(html).toContain(t("view.wait_counters.scope.pool"));
  expect(html).toContain(t("view.pool_waits.scope", { tier: "General Purpose" }));
  expect(html).toContain(t("view.pool_waits.original"));
  expect(html).not.toContain("<details open");
  const denied = { ...view, entries: [], native_rows: 0, observed_rows: 0, presented_rows: 0,
    sources: [{ source: "waits", state: "unavailable" as const, reason: t("view.pool_waits.tier_unqualified", { tier: "Hyperscale" }) }],
    summary: t("view.wait_counters.summary.partial"), limitations: [t("view.pool_waits.tier_unqualified", { tier: "Hyperscale" }), t("view.pool_waits.original")] };
  const data = waitOnlyHealthReport(); data.wait_counters = denied;
  const health = renderHealth(data);
  expect(health).toContain(t("view.pool_waits.tier_unqualified", { tier: "Hyperscale" }));
  expect(health).not.toContain(t("view.wait_counters.summary.empty"));
  expect(health).not.toContain(t("ui.health.empty"));
});
