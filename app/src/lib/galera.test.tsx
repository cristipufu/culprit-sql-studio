import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Galera } from "../components/Galera";
import { HealthTab } from "../components/Health";
import { ReplicationFields } from "../components/ReplicationEvidence";
import { galeraHealthReport, galeraReport } from "../test/galera";
import { t } from "./i18n";

test("historical native node observations fold and page with their own clocks and original qualifications", () => {
  const reports = Array.from({ length: 12 }, (_, i) => galeraReport(i));
  const html = renderToStaticMarkup(<Galera reports={reports} />);
  expect(html.match(/<article/g)).toHaveLength(10); expect(html).not.toContain("<details open");
  expect(html.match(/class="replication-entry"/g)).toHaveLength(10);
  expect(html).not.toContain("native-state-0"); expect(html).not.toContain("<dl");
  expect(html).toContain("2020-01-01T00:00:00Z"); expect(html).toContain(t("view.galera.partial"));
  expect(html).toContain(t("view.galera.reset")); expect(html).not.toContain("private-galera-process");
  expect(html).not.toContain("private-native-instance"); expect(html).not.toContain("sig-dot");
});
test("exact wide totals, zero and native NULL stay distinct and native fields render as escaped text", () => {
  const entry = galeraReport().entries[0]!;
  entry.fields[0]!.value = "<img src=x onerror=alert(1)>🌿";
  const html = renderToStaticMarkup(<ReplicationFields fields={[...entry.fields, ...entry.groups.flatMap((group) => group.fields)]} />);
  expect(html).toContain("9007199254740993"); expect(html).toContain(">0<"); expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain("&lt;img"); expect(html).not.toContain("<img"); expect(html).toContain("🌿"); expect(html).not.toContain("�");
});
test("Health keeps independent Galera originals while a withdrawn original cannot become a healthy zero", () => {
  const report = galeraReport();
  expect(renderToStaticMarkup(<HealthTab report={galeraHealthReport([report])} pressure={null} resources={[]} engineLabel="PostgreSQL" />)).toContain(t("view.galera.title"));
  report.original = null; report.native_rows = null; report.observed_rows = 0; report.presented_rows = 0; report.entries = []; report.read_complete = false;
  report.summary = t("view.galera.sharing_withheld"); for (const source of report.sources) { source.state = "unavailable"; source.reason = report.summary; }
  const html = renderToStaticMarkup(<Galera reports={[report]} />);
  expect(html).toContain(report.summary); expect(html).toContain(t("ui.dashboard.status.unavailable"));
  expect(html).not.toContain("native-state"); expect(html).not.toContain("9007199254740993"); expect(html).not.toContain(">0<");
});
