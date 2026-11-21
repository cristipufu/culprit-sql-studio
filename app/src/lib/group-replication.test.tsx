import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { GroupReplication } from "../components/GroupReplication";
import { HealthTab } from "../components/Health";
import { ReplicationFields } from "../components/ReplicationEvidence";
import { groupReplicationHealthReport, groupReplicationReport } from "../test/group-replication";
import { t } from "./i18n";

test("group originals are folded and paged with independent clocks and source qualifications", () => {
  const reports = Array.from({ length: 12 }, (_, i) => groupReplicationReport(i));
  const html = renderToStaticMarkup(<GroupReplication reports={reports} />);
  expect(html.match(/<article/g)).toHaveLength(10); expect(html).not.toContain("<details open");
  expect(html).toContain("original-group-0"); expect(html).not.toContain("original-group-10");
  expect(html).toContain("2020-01-01T00:00:00Z"); expect(html).toContain(t("view.group_replication.partial"));
  expect(html).toContain(t("view.group_replication.limit.history")); expect(html).not.toContain("private-observer-process");
  expect(html).not.toContain("private-native-namespace"); expect(html).not.toContain("sig-dot");
});

test("shared replication fields retain exact wide integers and native NULL while rendering native text as text", () => {
  const report = groupReplicationReport(); const fields = report.entries[0]!.groups.flatMap((group) => group.fields);
  fields.push({ key: "native", label: "Native value", value: "<img src=x onerror=alert(1)>" });
  const html = renderToStaticMarkup(<ReplicationFields fields={fields} />);
  expect(html).toContain("9007199254740993"); expect(html).toContain(">0<"); expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain("&lt;img"); expect(html).not.toContain("<img");
});

test("Health shows historical group context independently of current engine and current database", () => {
  const report = groupReplicationHealthReport([groupReplicationReport()]);
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(t("view.group_replication.title")); expect(html).toContain(t("view.group_replication.saved_summary"));
  expect(html).not.toContain(t("ui.health.empty"));
});

test("withdrawn original stays unavailable and does not gain another member or a healthy zero", () => {
  const report = groupReplicationReport(); report.original = null; report.native_rows = null; report.observed_rows = 0; report.presented_rows = 0; report.entries = []; report.read_complete = false;
  report.summary = t("view.group_replication.sharing_withheld"); for (const source of report.sources) { source.state = "unavailable"; source.reason = report.summary; }
  const html = renderToStaticMarkup(<GroupReplication reports={[report]} />);
  expect(html).toContain(report.summary); expect(html).toContain(t("ui.dashboard.status.unavailable"));
  expect(html).not.toContain("original-group"); expect(html).not.toContain("native-member"); expect(html).not.toContain(">0<");
});
