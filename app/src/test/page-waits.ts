import type { PageWaitObservation } from "../gen/PageWaitObservation";
import type { PageWaitReport } from "../gen/PageWaitReport";
import type { PageWaitRecordedIncident } from "../gen/PageWaitRecordedIncident";
import type { PageWaitSnapshot } from "../gen/PageWaitSnapshot";

export const PAGE_SOURCE = "page wait targets";
export const PAGE_FROM = "2026-11-13T10:00:00.200000Z";
export const PAGE_UNTIL = "2026-11-13T10:00:00.800000Z";
export const PAGE_BEFORE = "2026-11-13T10:00:00.300000Z";
export const PAGE_AFTER = "2026-11-13T10:00:00.700000Z";

/** A bounded original waiter and independently resolved page, shared by rendering and browser fixtures. */
export function pageWait(session = "42", page = 42): PageWaitObservation {
  const resource = {
    kind: "memory-page" as const,
    native: { id: `5:1:${page}`, qualifiers: { source: "sys.dm_exec_requests.wait_resource", namespace: "native" } },
  };
  return {
    source: PAGE_SOURCE, observed_from: PAGE_FROM, observed_until: PAGE_UNTIL,
    request: {
      blocking_status: "known", waiting_on_lock: null, key: { session: { id: session, generation: "native-login-1" }, request: { id: "0", generation: "native-request-1" }, transaction: null },
      idle: false, scope: "request-database", state: "suspended", database: "app", command: "SELECT",
      last_request_age_ms: null, open_transactions: 0, elapsed_ms: "100000", transaction_age_ms: null,
      session_counters: { cpu_ms: null, logical_reads: null, physical_reads: null, writes: null },
      request_counters: { cpu_ms: "0", logical_reads: "18446744073709551615", physical_reads: null, writes: "0" },
      wait: { class: null, native_type: "PAGELATCH_EX", elapsed_ms: "9007199254740993", resources: null, identified_resources: [structuredClone(resource)], owners: [{ kind: "unknown", native_id: "-5", reason: "untracked-latch" }] },
      query_hash: null, client: { application: "observed-label", user: null, host: null },
      statement: { state: "available", text: "SELECT * FROM [orders] WHERE name=N'<script>canary</script>'", sha256: "a".repeat(64), redaction_version: 1 },
    },
    observed_address: { database_id: 5, file_id: 1, page_id: page },
    before_check: { observed_at: PAGE_BEFORE, request: "same-statement", wait: "original-observed" },
    after_check: { observed_at: PAGE_AFTER, request: "same-statement", wait: "original-observed" },
    statement_identity: { id: "native-statement-handle", qualifiers: { statement_start_offset: "0" } },
    plan_identity: { id: "native-plan-handle", qualifiers: {} }, resource, resolution: "observed-at-both-bounds",
    target: {
      scope: { kind: "request-database" },
      identity_basis: "local-database-file-epoch", database: { id: "5", qualifiers: { epoch: "native-database-epoch" } }, file: { id: "1", qualifiers: { epoch: "native-file-epoch" } },
      header: { database_id: 5, file_id: 1, page_id: page, header_version: 1, page_type: 2, level: 0, object_id: "128", index_id: "1", partition_id: null, allocation_unit_id: "9223372036854775807", lsn: "000001:000002:0003", next_file_id: null, next_page_id: null },
      kind: "index-leaf", object: { native: { id: "128", qualifiers: {} }, schema: "dbo", name: "orders", index_name: "orders_id" },
    },
  };
}

export function pageEvidence(observations = [pageWait()]): PageWaitSnapshot {
  return { source: PAGE_SOURCE, observed_from: "2026-11-13T10:00:00.100000Z", observed_until: "2026-11-13T10:00:00.900000Z", observations };
}

/** Presentation receives the backend's authenticated original, independently of any current rows. */
export function recordedPageIncident(row = pageWait()): PageWaitRecordedIncident {
  return {
    original: {
      capture: { observation_id: "private-original-capture", revision: null },
      manifest_sha256: "a".repeat(64), source_scope_id: "private-source-scope", stream: "activity-observations",
      native_key: { kind: "activity", data: structuredClone(row.request.key) }, batch_id: "private-original-batch",
      record_id: "private-original-record", payload_sha256: "b".repeat(64), representation_version: 2, row_ordinal: 0,
    },
    observed_at: PAGE_FROM, request: structuredClone(row.request.key), resource: structuredClone(row.resource),
    sql: structuredClone(row.request.statement), client: structuredClone(row.request.client),
  };
}

/** Parallel tasks retain their own wait even when the coordinator reports an unrelated native wait. */
export function pageTaskWait(context = 1): PageWaitObservation {
  const row = pageWait();
  row.request.state = "running";
  row.request.wait = { class: null, native_type: "CXCONSUMER", elapsed_ms: "777", resources: null, identified_resources: [], owners: [] };
  row.resource.native.qualifiers.source = "sys.dm_os_waiting_tasks.resource_description";
  row.task_wait = {
    observed_at: PAGE_FROM, session_id: 42, request_id: 0, execution_context_id: context,
    task_address: context.toString(16).padStart(16, "0"),
    wait_type: "PAGELATCH_EX", wait_duration_ms: "9007199254740993", resource: structuredClone(row.resource),
    resource_address: "000000000000abcd", blocking_task_address: null,
    blocking_session_id: -5, blocking_execution_context_id: null,
  };
  return row;
}

export function pageReport(observations = [pageWait()]): PageWaitReport {
  return {
    source_state: "partial", source_reason: "Only bounded coordinator waits were retained.",
    title: "Observed page waits", summary: `${observations.length} affected requests; no transient holder identified.`,
    recorded_incident: null,
    limitations: ["Matching metadata at two bounds does not establish continuous page identity or a latch owner."],
    entries: observations.map((row, index) => ({
      observation_key: `opaque-backend-page-reference-${index}`, request_key: structuredClone(row.request.key), resource: structuredClone(row.resource),
      task: row.task_wait ? { task_address: row.task_wait.task_address, execution_context_id: row.task_wait.execution_context_id } : undefined,
      blocking: { basis: null, reported_owners: [], contexts: [], context_count: 0, issues: ["frame-unavailable"] },
      title: `Affected request ${index}`, explanation: "The request waited on the reported resource; an exclusive cause or owner is not established.",
      investigation: "Correlate the original request and native metadata before a measured code change.",
      verify: "Compare the same native evidence after the tested change.",
    })),
  };
}
