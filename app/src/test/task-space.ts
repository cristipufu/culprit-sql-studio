import type { TaskSpaceObservation } from "../gen/TaskSpaceObservation";
import type { TaskSpaceReport } from "../gen/TaskSpaceReport";
import type { TaskSpaceSnapshot } from "../gen/TaskSpaceSnapshot";
import { pageWait } from "./page-waits";

export const TASK_SOURCE = "task space";

export function taskSpace(execution = "0"): TaskSpaceObservation {
  const request = pageWait().request;
  request.wait = null;
  return {
    source: TASK_SOURCE, observed_at: "2026-11-13T10:00:00.200000Z",
    task: { session_id: "42", request_id: "0", execution_context_id: execution },
    native_database_id: "2", bytes_per_page: 8192,
    user_allocated_pages: "9223372036854775807", user_deallocated_pages: "0",
    internal_allocated_pages: null, internal_deallocated_pages: "9007199254740993",
    request,
    before_check: { observed_at: "2026-11-13T10:00:00.300000Z", state: "same-statement" },
    after_check: { observed_at: "2026-11-13T10:00:00.700000Z", state: "same-statement" },
    statement_identity: { id: "task-native-statement", qualifiers: {} },
    plan_identity: { id: "task-native-plan", qualifiers: {} },
  };
}

export function taskSpaceEvidence(observations = [taskSpace()]): TaskSpaceSnapshot {
  return { source: TASK_SOURCE, observed_from: "2026-11-13T10:00:00.100000Z", observed_until: "2026-11-13T10:00:00.900000Z", observations };
}

export function taskSpaceReport(observations = [taskSpace()]): TaskSpaceReport {
  return {
    source_state: "partial", source_reason: "The task read was bounded.",
    title: "Task tempdb activity", summary: `${observations.length} native task observations were retained.`,
    limitations: ["Current request context does not identify the allocating statement."],
    entries: observations.map((row, index) => ({ observation_key: `opaque-task-${index}`, task: structuredClone(row.task),
      title: `Native task ${index}`, explanation: "Allocation and deallocation counts remain independent of current SQL context.",
      investigation: "Measure a representative workload before changing memory settings.",
      verify: "Do not subtract capture-local counters between reads.",
    })),
  };
}
