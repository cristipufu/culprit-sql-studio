import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { IncidentHistory, IncidentHistoryImport, IncidentRevision } from "../components/IncidentHistory";
import { HealthTab } from "../components/Health";
import { incidentHistoryCase } from "../test/incident-history";
import { healthReportFixture } from "../test/health-report";
import { t } from "./i18n";

test("Rust-derived history distinguishes active, uncertain and resolved revisions from independently unresolved analysis", () => {
  const report = incidentHistoryCase();
  expect(report.entries).toHaveLength(13);
  expect(report.entries[0]!.lifecycle).toBe("active");
  expect(report.entries[1]!.lifecycle).toBe("uncertain");
  const last = report.entries.at(-1)!;
  expect(last.lifecycle).toBe("resolved");
  expect(last.analysis).toBe("unresolved");
  const html = renderToStaticMarkup(<IncidentHistory report={report} />);
  expect(html).toContain(report.summary);
  expect(html).not.toContain("<details open");
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toContain(last.title);
  expect(html).toContain(t("view.incident_history.limit.imported"));
  expect(html).toContain(t("view.incident_history.limit.actions"));
});
test("history text and retained references escape markup and preserve original clocks without executable session actions", () => {
  const report = incidentHistoryCase("resolved");
  const entry = report.entries.at(-1)!;
  entry.source = "<img src=x onerror=alert(1)>🌿";
  entry.evidence[0]!.capture.capture.observation_id = "<script>private</script>";
  const html = renderToStaticMarkup(<IncidentRevision entry={entry} />);
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("<script>");
  expect(html).toContain("🌿");
  expect(html).toContain(entry.scope_label);
  expect(html).toContain(entry.first_seen_at);
  expect(html).toContain(entry.recorded_at);
  expect(html).not.toContain("<button");
  expect(html).not.toContain("sig-dot");
});
test("missing predecessor, unavailable reference and incomplete selection never render as a healthy empty incident history", () => {
  const report = incidentHistoryCase("prefix");
  expect(report.entries[0]!.predecessor_selected).toBe(false);
  const html = renderToStaticMarkup(<IncidentHistory report={report} />);
  expect(html).toContain(t("view.incident_history.limit.predecessor"));
  expect(html).toContain(t("view.incident_history.evidence.unverified"));
  expect(html).not.toContain(t("ui.health.empty"));
});
test("the actual Health surface offers explicit file intake even when the current scan has no health measurements", () => {
  const html = renderToStaticMarkup(
    <HealthTab
      pressure={null}
      resources={[]}
      report={healthReportFixture({ snapshot: { connection: "recorded", server: { engine: "sqlite" }, sources: [] } })}
      engineLabel="SQLite"
    />,
  );
  expect(html).toContain(t("view.incident_history.import.section"));
  expect(html).toContain(t("view.incident_history.import.open"));
  const standalone = renderToStaticMarkup(<IncidentHistoryImport />);
  expect(standalone).not.toContain("<article");
  expect(standalone).not.toContain("source_state");
});

test("tempdb context uses the existing historical disclosure with explicit competing and missing causes", () => {
  for (const name of ["temporary", "temporary_partial"] as const) {
    const entry = incidentHistoryCase(name).entries[0]!;
    const html = renderToStaticMarkup(<IncidentRevision entry={entry} />);
    expect(entry.analysis).toBe("unresolved");
    expect(html).toContain(t("view.tempdb_question.summary"));
    expect(html).toContain(t("view.tempdb_question.spill"));
    expect(html).toContain(t("view.tempdb_question.role"));
    expect(html).toContain(t("view.tempdb_question.coverage", { state: name === "temporary" ? "collected" : "partial" }));
    expect(html).toContain(entry.title);
    expect(html).toContain(t("view.incident_history.evidence.details"));
    expect(html).not.toContain("<button");
  }
});

test("the persisted compilation answer keeps exact lifetime context and competing unknowns in the shared history renderer", () => {
  for (const name of ["compilation", "compilation_partial", "compilation_changed", "compilation_unavailable"] as const) {
    const report = incidentHistoryCase(name);
    const entry = report.entries[0]!;
    const html = renderToStaticMarkup(<IncidentRevision entry={entry} />);
    expect(entry.analysis).toBe("unresolved");
    expect(entry.lifecycle).toBe("uncertain");
    expect(html).toContain(t("view.incident_history.compilation.independent"));
    expect(html).toContain(t("view.incident_history.compilation.gap.work"));
    expect(html).toContain(t("view.incident_history.compilation.gap.inputs"));
    expect(html).toContain(t("view.incident_history.compilation.gap.waits"));
    expect(html).not.toContain(t("ui.health.empty"));
    expect(html).not.toContain("<button");
  }
});
