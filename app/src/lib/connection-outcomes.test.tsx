import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ConnectionOutcomes } from "../components/ConnectionOutcomes";
import { HealthTab } from "../components/Health";
import type { ConnectionOutcomeReport } from "../gen/ConnectionOutcomeReport";
import { DISPLAY_SESSION_COUNT, EXACT_SESSION_COUNT, connectionOutcomeHealthReport, connectionOutcomeReport,
  unavailableConnectionOutcomes } from "../test/connection-outcomes";
import { t } from "./i18n";

function show(report: ConnectionOutcomeReport): string {
  return renderToStaticMarkup(<ConnectionOutcomes report={report} />);
}

function health(view: ConnectionOutcomeReport | null): string {
  return renderToStaticMarkup(<HealthTab report={connectionOutcomeHealthReport(view)} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
}

test("connection outcomes keep observations and definitions folded with bounded rows and independent original counts", () => {
  const view = connectionOutcomeReport(12);
  const html = show(view);
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain(t("ui.connection_outcomes.counts", { native: "17", read: "14", mapped: "12" }));
  expect(html).toContain(view.entries[9]!.title);
  expect(html).not.toContain(view.entries[10]!.title);
  expect(html).toContain("2026-11-17T12:00:00.123456Z");
  expect(html).toContain("2026-11-17T12:00:01.123456Z");
  expect(html).not.toContain("private-host-canary");
  expect(html).not.toContain("private-database-canary");
  expect(html).not.toContain("sig-dot");
});

test("Rust display values and exact native values stay separate without parsing integer text or deriving duration", () => {
  const html = show(connectionOutcomeReport());
  expect(html).toContain(`<dd>${DISPLAY_SESSION_COUNT}</dd>`);
  expect(html).toContain(`<dd>${EXACT_SESSION_COUNT}</dd>`);
  expect(html).not.toContain("9007199254740992");
  expect(html).toContain("<dd>2 min 3 s</dd>");
  expect(html).toContain("<dd>123456.125</dd>");
  expect(html).toContain(t("view.connection_outcomes.meaning.session_time"));
  expect(html).toContain(t("view.connection_outcomes.limit.current"));
  expect(html).toContain(t("view.connection_outcomes.limit.lifetime"));
});

test("a native zero remains measured while a missing outcome stays explicitly unreported", () => {
  const html = show(connectionOutcomeReport());
  expect(html).toContain(`<dt>${t("view.connection_outcomes.metric.sessions_lost_client")}</dt><dd>0</dd>`);
  expect(html).toContain(`<dt>${t("view.connection_outcomes.metric.sessions_fatal")}</dt><dd>${t("ui.memory_failures.not_reported")}</dd>`);
  expect(html).toContain(t("ui.statement_work.reset_unknown"));
  expect(html).not.toContain("sig-dot good");
});

test("Health preserves an unavailable source and an independently withheld explanation without healthy empty fallback", () => {
  const unavailable = unavailableConnectionOutcomes();
  const source = health(unavailable);
  expect(source).toContain(unavailable.title);
  expect(source).toContain(t("ui.dashboard.status.unavailable"));
  expect(source).toContain(t("view.connection_outcomes.limit.source"));
  expect(source).toContain(t("view.connection_outcomes.limit.no_facts"));
  expect(source).not.toContain("<article");
  expect(source).not.toContain(EXACT_SESSION_COUNT);
  expect(source).not.toContain(t("ui.health.empty"));

  const withheld = health(null);
  expect(withheld).toContain(t("view.connection_outcomes.title"));
  expect(withheld).toContain(t("view.connection_outcomes.invalid"));
  expect(withheld).not.toContain("<article");
  expect(withheld).not.toContain(EXACT_SESSION_COUNT);
  expect(withheld).not.toContain(t("ui.health.empty"));
});

test("Health keeps an admitted derived explanation when all other pressure and resource signals are absent", () => {
  const html = health(connectionOutcomeReport());
  expect(html).toContain(t("view.connection_outcomes.title"));
  expect(html).toContain(DISPLAY_SESSION_COUNT);
  expect(html).not.toContain(t("ui.health.empty"));
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
});

test("a withheld shared capsule keeps its source gap visible after the private body has been removed", () => {
  const report = connectionOutcomeHealthReport(null);
  report.snapshot.pressure = null;
  report.snapshot.sources = [{ source: "sessions", state: "unavailable", reason: t("view.connection_outcomes.invalid") }];
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(t("view.connection_outcomes.title"));
  expect(html).toContain(t("view.connection_outcomes.invalid"));
  expect(html).toContain(t("ui.dashboard.status.unavailable"));
  expect(html).not.toContain(t("ui.health.empty"));
  expect(html).not.toContain("<article");
  expect(html).not.toContain(EXACT_SESSION_COUNT);
});

test("source labels and derived database text stay escaped without links or executable elements", () => {
  const view = connectionOutcomeReport();
  const text = '<img src=x onerror="outcome_canary()"><script>outcome_canary()</script>https://fixture.invalid';
  const entry = view.entries[0]!;
  entry.title = text;
  entry.explanation = text;
  entry.investigation = text;
  entry.verify = text;
  entry.metrics[0]!.label = text;
  entry.metrics[0]!.native_key = text;
  entry.metrics[0]!.explanation = text;
  entry.metrics[0]!.display_value = text;
  view.sources[0]!.source = text;
  view.sources[0]!.reason = text;
  const html = show(view);
  expect(html).toContain("&lt;img");
  expect(html).toContain("&lt;script&gt;outcome_canary()&lt;/script&gt;");
  expect(html).toContain("https://fixture.invalid");
  expect(html).not.toContain("<script");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("<a ");
  expect(html).not.toContain("href=");
});
