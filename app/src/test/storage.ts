import nativeReports from "../../../fixtures/storage.sample.json" with { type: "json" };
import type { StorageReport } from "../gen/StorageReport";
import type { Report } from "../gen/Report";
import { fromRust } from "./fixtures";
import { healthReportFixture } from "./health-report";

/** Actual Rust-view output over the shared native-shaped corpus; no JavaScript interpretation. */
export function storageReport(name: keyof typeof nativeReports = "exact"): StorageReport {
  return fromRust<StorageReport>(nativeReports[name]);
}
export function storageHealthReport(view: StorageReport | null = storageReport()): Report {
  return healthReportFixture({
    snapshot: {
      id: "storage-native-report",
      server: { engine: "mssql" },
      resources: [],
      pressure: null,
      sources: view ? [] : [{ source: "file_capacity", state: "unavailable", reason: null }],
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null },
    },
    storage: view ?? undefined,
  });
}
