import english from "../../../locales/en.json" with { type: "json" };
import type { CompilerAdmissionSnapshot } from "../gen/CompilerAdmissionSnapshot";
import type { CompilerAdmissionReport } from "../gen/CompilerAdmissionReport";
import type { OperationalObservation } from "../gen/OperationalObservation";
import { partial } from "./fixtures";

export const COMPILER_ADMISSION_SOURCE = "compilation gateways";

export function compilerGateway(index = 0): OperationalObservation {
  return { source: COMPILER_ADMISSION_SOURCE, key: String(index).padStart(64, "b"), fact: { kind: "counter-set", data: {
    identity: { id: `Gateway ${index}`, qualifiers: { native_object: "sys.dm_exec_query_optimizer_memory_gateways", resource_pool_id: String(index + 2) } },
    scope: "server", properties: { threshold: "9007199254740993", threshold_factor: "8", is_active: "1" },
    counters: [{ key: "admission-threshold-bytes", value: { kind: "integer", value: "9007199254740993" }, unit: { kind: "bytes" }, nature: "gauge", scope: "server",
      reset: { kind: "unknown" }, native_object: "sys.dm_exec_query_optimizer_memory_gateways", native_name: "threshold", native_instance: null, native_type: null }],
  } } };
}

export function compilerWaiter(): OperationalObservation {
  return partial<OperationalObservation>({ source: COMPILER_ADMISSION_SOURCE, key: "a".repeat(64), fact: { kind: "compiler-waiter", data: {
    observed_at: "2026-11-16T10:00:00Z", request: { key: { session: { id: "42", generation: "original-session" }, request: { id: "0", generation: "original-request" }, transaction: null },
      scope: "request-database", database: "app", wait: { class: "compilation-admission" }, client: { application: "native-client<script>canary</script>" },
      statement: { state: "available", text: "SELECT compiler_private_sql_body_canary", sha256: "c".repeat(64), redaction_version: 1 } },
  } } });
}

export function compilerAdmissionEvidence(observations = [compilerWaiter(), compilerGateway()]): CompilerAdmissionSnapshot {
  return { source: COMPILER_ADMISSION_SOURCE, context: null, observations };
}

export function compilerAdmissionReport(observations = [compilerWaiter(), compilerGateway()]): CompilerAdmissionReport {
  return { source_state: "partial", source_reason: "Native gateway visibility is partial.",
    title: english["view.compiler_admission.title"], summary: english["view.compiler_admission.summary.waiter"],
    limitations: [english["view.compiler_admission.limit.ownership"], english["view.compiler_admission.limit.memory"]],
    waiters: observations.flatMap((row) => row.fact.kind === "compiler-waiter" ? [{ observation_key: row.key, activity: row.fact.data.request.key,
      title: "Original request is waiting to compile", explanation: english["view.compiler_admission.waiter.explanation"],
      investigation: english["view.compiler_admission.waiter.investigate"], verify: english["view.compiler_admission.waiter.verify"] }] : []),
    gateways: observations.flatMap((row) => row.fact.kind === "counter-set" ? [{ observation_key: row.key, identity: row.fact.data.identity,
      title: row.fact.data.identity.id, explanation: "Active: 1; configured limit: 4; waiting: 2." }] : []),
  };
}
