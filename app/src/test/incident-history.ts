// Rust's canonical JSONL intake produces this controlled sidecar; no native collection/qualification is implied.
import sample from "../../../fixtures/incident-history.sample.json";
import type { IncidentHistoryReport } from "../gen/IncidentHistoryReport";
import { fromRust } from "./fixtures";
export function incidentHistoryCase(name: keyof typeof sample = "exact"): IncidentHistoryReport {
  return structuredClone(fromRust<IncidentHistoryReport>(sample[name]));
}
