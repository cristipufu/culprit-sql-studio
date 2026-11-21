import nativeCases from "../../../fixtures/instance-resources.sample.json" with { type: "json" };
import type { InstanceResourceRead } from "../gen/InstanceResourceRead";
import type { InstanceResourceReport } from "../gen/InstanceResourceReport";
import type { Report } from "../gen/Report";
import { fromRust } from "./fixtures";
import { healthReportFixture } from "./health-report";

/** Actual Rust output from the canonical controlled source; UI supplies no measurement or pressure policy. */
export function instanceResourceCase(name: keyof typeof nativeCases = "exact") {
  return fromRust<{ read: InstanceResourceRead; report: InstanceResourceReport }>(nativeCases[name]);
}

export function instanceResourceHealthReport(name: keyof typeof nativeCases = "exact"): Report {
  const { read, report } = instanceResourceCase(name);
  return healthReportFixture({
    instance_resources: report,
    snapshot: {
      id: "instance-resource-display",
      server: { engine: "mssql" },
      resources: [],
      sources: [{ source: "instance-resources", state: report.source_state, reason: null }],
      pressure: { instance_resources: read, signals: [], files: [], replicas: [], grants: [] },
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null },
    },
  });
}
