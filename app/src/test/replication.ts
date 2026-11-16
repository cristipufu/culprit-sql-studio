import examples from "../../../fixtures/replication.sample.json" with { type: "json" };
import type { ReplicationReport } from "../gen/ReplicationReport";
import type { ReplicationField } from "../gen/ReplicationField";
import type { Report } from "../gen/Report";
import { fromRust, partial } from "./fixtures";

const reports = fromRust<Record<string, ReplicationReport>>(examples);

/** The native cases and their explanations come from the Rust corpus and its shared pure report. */
export function replicationReport(name: string): ReplicationReport {
  const report = reports[name];
  if (report == null) throw new Error(`Missing Rust replication case: ${name}`);
  return structuredClone(report);
}

export function replicationFields(report: ReplicationReport): ReplicationField[] {
  return report.entries.flatMap((entry) => [...entry.fields, ...entry.groups.flatMap((group) => group.fields)]);
}

/** A surface containing only the supplied derived view; this supplies no new native evidence or diagnosis. */
export function replicationOnlyReport(replication: ReplicationReport): Report {
  return partial<Report>({
    findings: [], replication,
    snapshot: {
      id: "replication-ui-test", server: { engine: "mssql" }, sources: [], pressure: null, resources: [],
      operations: { log_failures: [], spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null },
    },
  });
}
