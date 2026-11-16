import samples from "../../../fixtures/actual.sample.json" with { type: "json" };
import type { CurrentRequestWorkReport } from "../gen/CurrentRequestWorkReport";
import type { LiveState } from "../gen/LiveState";
import type { Plan } from "../gen/Plan";
import type { QueryProgress } from "../gen/QueryProgress";
import type { SessionProgressContext } from "../gen/SessionProgressContext";
import { fromRust, partial } from "./fixtures";

export function progressPlan(label = "Original native plan"): Plan {
  const plan = fromRust<Plan>(samples.progress.mysql[0]!.plan);
  plan.root.op = label;
  return plan;
}

export function progressObservation(overrides: Partial<QueryProgress> = {}): QueryProgress {
  return {
    session: "42", engine: "mysql", taken_at: "2026-11-16T10:00:00Z", running: true,
    state: "executing", wait: null, wait_ms: null, elapsed_ms: 1500, cpu_ms: null, logical_reads: null,
    rows_examined: null, statement: "SELECT id FROM items WHERE state = ?", plan: null, nodes: [],
    percent: null, percent_basis: null, hot_node: null, phase: null, phase_percent: null, phase_percent_basis: null, note: null,
    current_work: null, current_work_report: null, session_context: null, request_key: "native-request-a",
    ...overrides,
  };
}

export function sessionProgressContext(): SessionProgressContext {
  return partial<SessionProgressContext>({
    source: "processlist", session: "42", database: "app", observed_at: "2026-11-16T10:00:01Z",
    context: { observed_from: "2026-11-16T10:00:01Z", observed_until: "2026-11-16T10:00:01Z" },
  });
}

export function currentWorkReport(): CurrentRequestWorkReport {
  return partial<CurrentRequestWorkReport>({
    source: "current request work", context: {
      observed_from: "2026-11-16T10:00:00Z", observed_until: "2026-11-16T10:00:01Z",
      target: { scope: "database", identity: { database: "private-route-canary" } },
    },
    source_state: "partial", source_reason: "Some native fields were not visible.", read_complete: true,
    native_rows: 1, observed_rows: 1, binding: "matched", membership: "observed",
    title: "Observed native request work", summary: "Temporary-table work was recorded for this request.",
    limitations: ["These counters do not establish elapsed spill time or an incident cause."],
    entries: [{
      observation_key: "exact-native-observation", observed_at: "2026-11-16T10:00:01Z",
      actor: { session: { id: "42", generation: "native-session-generation" }, request: { id: "8", generation: "native-request-generation" }, transaction: null },
      database: "app", scope: "request-database", statement_identity: { id: "private-statement-authority", qualifiers: {} },
      statement: { state: "unavailable", reason: "not-collected" },
      title: "Native stage <script>canary</script>", explanation: "Observed temporary-table count; allocation ownership remains bounded.",
      metrics: [{ label: "Native temporary rows", value: 0, unit: "text", text: "9007199254740993" }],
      investigation: "Compare this exact native request with its recorded mechanism.", verify: "Use the same native measurement after a tested change.",
    }],
  });
}

export function progressLiveState(): LiveState {
  return {
    taken_at: "2026-11-16T10:00:00Z", notes: [], waits: [], session_actions: [], session_actions_note: null, blocking: null,
    sessions: [{
      id: "42", user: "app", application: "native-client", host: null, database: "app", state: "active", command: "SELECT",
      wait: null, wait_ms: null, blocked_by: null, elapsed_ms: 1500, transaction_ms: null, cpu_ms: null, reads: null,
      sql: "SELECT id FROM items WHERE state = ?", waiting_on_lock: false,
    }],
  };
}
