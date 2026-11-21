import type { Report } from "../gen/Report";
import { type DeepPartial, partial } from "./fixtures";

/** Required Report collections shared by Health fixtures; each signal keeps its own source and evidence fields. */
export function healthReportFixture(fields: DeepPartial<Report>): Report {
  return partial<Report>({
    performance_counters: [],
    ...fields,
    snapshot: {
      ...fields.snapshot,
      operations: {
        spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null, cdc_history: [], change_feed_history: [],
        ...fields.snapshot?.operations,
      },
    },
  });
}
