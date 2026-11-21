// The existing Rust sidecar owns every field and qualification; this loader performs no diagnosis.
import samples from "../../../fixtures/instance-resources.sample.json";
import type { InstanceResourceReport } from "../gen/InstanceResourceReport";
import { fromRust } from "./fixtures";
export function numaNodeReport(name: Extract<keyof typeof samples, `numa_${string}`>): InstanceResourceReport {
  return fromRust<{ report: InstanceResourceReport }>(samples[name]).report;
}
