import type { PageWaitObservation } from "../gen/PageWaitObservation";
import type { PageWaitReport } from "../gen/PageWaitReport";
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
      blocking_status: "known", key: { session: { id: session, generation: "native-login-1" }, request: { id: "0", generation: "native-request-1" }, transaction: null },
      idle: false, scope: "request-database", state: "suspended", database: "app", command: "SELECT",
      last_request_age_ms: null, open_transactions: 0, elapsed_ms: "100000", transaction_age_ms: null,
      session_counters: { cpu_ms: null, logical_reads: null, physical_reads: null, writes: null },
      request_counters: { cpu_ms: "0", logical_reads: "18446744073709551615", physical_reads: null, writes: "0" },
      wait: { native_type: "PAGELATCH_EX", elapsed_ms: "9007199254740993", resources: null, identified_resources: [structuredClone(resource)], owners: [{ kind: "unknown", native_id: "-5", reason: "untracked-latch" }] },
      query_hash: null, client: { application: "observed-label", user: null, host: null },
      statement: { state: "available", text: "SELECT * FROM [orders] WHERE name=N'<script>canary</script>'", sha256: "a".repeat(64), redaction_version: 1 },
    },
    observed_address: { database_id: 5, file_id: 1, page_id: page },
    before_check: { observed_at: PAGE_BEFORE, request: "same-statement", wait: "original-observed" },
    after_check: { observed_at: PAGE_AFTER, request: "same-statement", wait: "original-observed" },
    statement_identity: { id: "native-statement-handle", qualifiers: { statement_start_offset: "0" } },
    plan_identity: { id: "native-plan-handle", qualifiers: {} }, resource, resolution: "observed-at-both-bounds",
    target: {
      identity_basis: "local-database-file-epoch", database: { id: "5", qualifiers: { epoch: "native-database-epoch" } }, file: { id: "1", qualifiers: { epoch: "native-file-epoch" } },
      header: { database_id: 5, file_id: 1, page_id: page, header_version: 1, page_type: 2, level: 0, object_id: "128", index_id: "1", partition_id: null, allocation_unit_id: "9223372036854775807", lsn: "000001:000002:0003", next_file_id: null, next_page_id: null },
      kind: "index-leaf", object: { native: { id: "128", qualifiers: {} }, schema: "dbo", name: "orders", index_name: "orders_id" },
    },
  };
}

export function pageEvidence(observations = [pageWait()]): PageWaitSnapshot {
  return { source: PAGE_SOURCE, observed_from: "2026-11-13T10:00:00.100000Z", observed_until: "2026-11-13T10:00:00.900000Z", observations };
}

export function pageReport(observations = [pageWait()]): PageWaitReport {
  return {
    source_state: "partial", source_reason: "Only bounded coordinator waits were retained.",
    title: "Observed page waits", summary: `${observations.length} affected requests; no transient holder identified.`,
    limitations: ["Matching metadata at two bounds does not establish continuous page identity or a latch owner."],
    entries: observations.map((row, index) => ({
      observation_key: `opaque-backend-page-reference-${index}`, request_key: structuredClone(row.request.key), resource: structuredClone(row.resource),
      title: `Affected request ${index}`, explanation: "The request waited on the reported resource; an exclusive cause or owner is not established.",
      investigation: "Correlate the original request and native metadata before a measured code change.",
      verify: "Compare the same native evidence after the tested change.",
    })),
  };
}
