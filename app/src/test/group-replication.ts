import type { GroupReplicationReport } from "../gen/GroupReplicationReport";
import { t } from "../lib/i18n";
import { tableMemoryHealthReport } from "./table-memory";
export function groupReplicationReport(index = 0): GroupReplicationReport {
  const member = { engine: "mysql" as const, kind: "instance" as const, namespace: { id: "private-native-namespace", qualifiers: {} }, native: { id: "private-observer-member", qualifiers: {} } };
  return {
    original: { context: { target: { scope: "shared", identity: member }, partition: { kind: "instance-local", member, epoch: "private-observer-process" },
      observed_from: "2020-01-01T00:00:00Z", observed_until: "2020-01-01T00:00:01Z", database_writable: null },
      metadata: { observed_at: "2020-01-01T00:00:00Z", group_id: `original-group-${index}`, plugin_status: "ACTIVE", members_layout: true, statistics_layout: true } },
    sources: [{ source: "group-replication-members", state: "collected", reason: null }, { source: "group-replication-statistics", state: "partial", reason: t("view.group_replication.partial") }],
    read_complete: true, native_rows: 2, observed_rows: 2, presented_rows: 1, entries_limited: false,
    title: t("view.group_replication.title"), summary: t("view.group_replication.summary", { n: 1, count: "1" }),
    limitations: [t("view.group_replication.limit.scope"), t("view.group_replication.limit.propagation"), t("view.group_replication.limit.reset"), t("view.group_replication.limit.history"), t("view.group_replication.limit.coverage")],
    entries: [{ reference: { source: "group-replication-members", observation_key: `original-member-${index}` }, scope: "server", conditions: [],
      title: t("view.group_replication.member_title"), explanation: t("view.group_replication.queued"),
      fields: [{ key: "member_id", label: t("view.group_replication.field.member"), value: `native-member-${index}` },
        { key: "state", label: t("view.group_replication.field.state"), value: "ONLINE" }, { key: "role", label: t("view.group_replication.field.role"), value: "SECONDARY" }],
      groups: [{ title: t("view.group_replication.gauges"), fields: [{ key: "certification_queue", label: t("view.group_replication.field.certification_queue"), value: "0" }, { key: "applier_queue", label: t("view.group_replication.field.applier_queue"), value: null }] },
        { title: t("view.group_replication.totals"), fields: [{ key: "checked", label: t("view.group_replication.field.checked"), value: "9007199254740993" }] }],
      session: null, unresolved_session_id: null, investigation: t("view.group_replication.investigate"), verify: t("view.group_replication.verify") }],
  };
}
export function groupReplicationHealthReport(reports: GroupReplicationReport[]) {
  const report = tableMemoryHealthReport([]); report.group_replication = reports; return report;
}
