import type { GaleraReport } from "../gen/GaleraReport";
import { t } from "../lib/i18n";
import { tableMemoryHealthReport } from "./table-memory";
export function galeraReport(index = 0): GaleraReport {
  const member = { engine: "mysql" as const, kind: "instance" as const, namespace: { id: "mariadb-server-uid", qualifiers: {} }, native: { id: "private-native-instance", qualifiers: {} } };
  return { original: { context: { target: { scope: "shared", identity: member }, partition: { kind: "instance-local", member, epoch: "private-galera-process" },
    observed_from: "2020-01-01T00:00:00Z", observed_until: "2020-01-01T00:00:01Z", database_writable: null },
    metadata: { observed_at: "2020-01-01T00:00:00Z", enabled: true } },
    sources: [{ source: "mariadb-galera", state: "partial", reason: t("view.galera.partial") }],
    read_complete: true, native_rows: 1, observed_rows: 1, presented_rows: 1, entries_limited: false,
    title: t("view.galera.title"), summary: t("view.galera.summary"),
    limitations: [t("view.galera.scope"), t("view.galera.reset"), t("view.galera.non_atomic"), t("view.galera.unknown")],
    entries: [{ reference: { source: "mariadb-galera", observation_key: `original-node-${index}` }, scope: "server", conditions: [],
      title: t("view.galera.node_title"), explanation: t("view.galera.node_context"),
      fields: [{ key: "wsrep_local_state_comment", label: t("view.galera.state_comment"), value: `native-state-${index}` }],
      groups: [{ title: t("view.galera.queues"), fields: [{ key: "wsrep_local_recv_queue", label: t("view.galera.receive_queue"), value: "0" },
        { key: "wsrep_local_send_queue", label: t("view.galera.send_queue"), value: null }] },
        { title: t("view.galera.totals"), fields: [{ key: "wsrep_local_cert_failures", label: t("view.galera.certification_failures"), value: "9007199254740993" }] }],
      session: null, unresolved_session_id: null, investigation: t("view.galera.investigation"), verify: t("view.galera.verify") }],
  };
}
export function galeraHealthReport(reports: GaleraReport[]) { const report = tableMemoryHealthReport([]); report.galera = reports; return report; }
