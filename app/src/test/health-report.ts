import type { Report } from "../gen/Report";
import { type DeepPartial, partial } from "./fixtures";

/** Required Report collections shared by Health fixtures; each signal keeps its own source and evidence fields. */
export function healthReportFixture(fields: DeepPartial<Report>): Report {
  return partial<Report>({
    source_acquisitions: [],
    performance_counters: [],
    index_configuration: [],
    ...fields,
    snapshot: {
      ...fields.snapshot,
      operations: {
        source_acquisitions: [], spaces: [], index_configuration: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null, cdc_history: [], change_feed_history: [], numa_nodes: [],
        ...fields.snapshot?.operations,
      },
    },
  });
}
