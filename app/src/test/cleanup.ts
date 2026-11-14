import english from "../../../locales/en.json" with { type: "json" };
import type { CleanupExplanation } from "../gen/CleanupExplanation";
import type { CleanupReport } from "../gen/CleanupReport";
import type { CleanupSnapshot } from "../gen/CleanupSnapshot";
import type { OperationalObservation } from "../gen/OperationalObservation";
import type { Pressure } from "../gen/Pressure";
import type { RetentionObservation } from "../gen/RetentionObservation";
import type { RetentionSnapshot } from "../gen/RetentionSnapshot";
import type { SourceOutcome } from "../gen/SourceOutcome";

export const PVS_SOURCE = "persistent version store";
export const RETENTION_SOURCE = "pg_stat_activity";
export const PREPARED_SOURCE = "pg_prepared_xacts";
export const EXACT_REASON_COUNT = "9007199254740993";
export const NATIVE_COLUMN_CANARY = `<script>native-column-canary</script>${"x".repeat(384)}`;
export const PREPARED_AT = "2026-11-13T09:45:00.000001Z";

export function persistentVersionObservation(): OperationalObservation {
  return { source: PVS_SOURCE, key: "native-pvs-point", fact: { kind: "mssql-persistent-version-store", data: {
    available_columns: ["persistent_version_store_size_kb", "pvs_off_row_page_skipped_oldest_active_xdesid", NATIVE_COLUMN_CANARY],
    filegroup_id: null, off_row_kibibytes: "4096", online_index_kibibytes: null, aborted_transactions: null,
    oldest_active_transaction_id: null, oldest_aborted_transaction_id: null,
    minimum_transaction_timestamp: null, online_index_minimum_timestamp: null, secondary_low_water_mark: null,
    off_row_cleanup: { started_without_offset: null, ended_without_offset: null },
    aborted_cleanup: { started_without_offset: null, ended_without_offset: null },
    pages_skipped_for_secondaries: null, pages_skipped_for_aborted_transactions: null,
    pages_skipped_for_oldest_active_transaction: EXACT_REASON_COUNT, pages_skipped_for_long_snapshot_scan: null,
    pages_skipped_for_online_index_rebuild: null, pages_skipped_for_oldest_aborted_transaction: null,
  } } };
}

export function persistentVersionRead(observations = [persistentVersionObservation()]): CleanupSnapshot {
  return { source: PVS_SOURCE, observed_from: "2026-11-13T10:00:00Z", observed_until: "2026-11-13T10:00:01Z", observations };
}

export function persistentVersionReport(row = persistentVersionObservation()): CleanupReport {
  if (row.fact.kind !== "mssql-persistent-version-store") throw new Error("PVS fixture required");
  return report([{ source: PVS_SOURCE, state: "partial", reason: "Only this bounded native point was read." }], [{
    reference: { source: row.source, observation_key: row.key, identity: null },
    mechanisms: ["persistent-version-store", "oldest-active-transaction"], title: "Native persistent version store",
    explanation: "A native counter records skipped pages; the responsible application code is unresolved.",
    metrics: [{ key: "pvs_off_row_page_skipped_oldest_active_xdesid", label: "Native pages skipped for the oldest active transaction", value: 0, unit: "text", text: row.fact.data.pages_skipped_for_oldest_active_transaction }],
    investigation: "Compare independently qualified native transaction context.",
    verify: "Compare native points with matching scope and reset evidence.",
  }]);
}

export function retainer(index: number): RetentionObservation {
  const generation = `2026-11-13T09:00:${String(index).padStart(2, "0")}.000001Z`;
  const startedAt = `2026-11-13T09:30:${String(index).padStart(2, "0")}.000001Z`;
  return {
    source: RETENTION_SOURCE,
    identity: { id: RETENTION_SOURCE, qualifiers: { native_id: "42", database_oid: "7", backend_start: generation, transaction_start: startedAt } },
    observed_at: "2026-11-13T10:00:00.000001Z", scope: "database", holder: "session", name: `Native retainer ${index}`,
    horizons: [{ protects: ["data", "catalog", "shared-catalog"], native_name: "backend_xmin", native_value: "100000000", transactions_behind: "200000000" }],
    session: { id: "42", generation }, unresolved_session_id: null,
    transaction: { native_id: `native-xid-${index}`, started_at: startedAt, prepared_at: null },
    query: null, client: null, activity: "idle_in_transaction", native_state: "idle in transaction",
    age_seconds: null, database: "fixture", code_ownership: "not-collected",
  };
}

export function preparedRetainer(): RetentionObservation {
  return {
    source: PREPARED_SOURCE,
    identity: { id: PREPARED_SOURCE, qualifiers: { native_id: "5000", database_oid: "7", prepared_at: PREPARED_AT } },
    observed_at: "2026-11-13T10:00:00.000001Z", scope: "database", holder: "prepared-transaction", name: "Prepared native transaction",
    horizons: [{ protects: ["data", "catalog", "shared-catalog"], native_name: "transaction", native_value: "5000", transactions_behind: "200000000" }],
    session: null, unresolved_session_id: null, transaction: { native_id: "5000", started_at: null, prepared_at: PREPARED_AT },
    query: null, client: null, activity: "prepared", native_state: null, age_seconds: null,
    database: "fixture", code_ownership: "not-collected",
  };
}

export function retentionEvidence(holders = Array.from({ length: 12 }, (_, index) => retainer(index))): RetentionSnapshot {
  const nativeSources = [...new Set(holders.map((holder) => holder.source))];
  return {
    holders, sources: nativeSources.map((source) => ({ source, state: "collected" })),
    coverage: { native_sources: nativeSources, qualified_horizons: ["data", "catalog"] },
  };
}

export function retentionReport(holders = Array.from({ length: 12 }, (_, index) => retainer(index))): CleanupReport {
  const sources: SourceOutcome[] = [...new Set(holders.map((holder) => holder.source))].map((source) => ({ source, state: "collected", reason: null }));
  const result = report(sources, holders.map<CleanupExplanation>((holder, index) => ({
    reference: { source: holder.source, observation_key: `native-retainer-${index}`, identity: structuredClone(holder.identity) },
    mechanisms: [holder.holder === "prepared-transaction" ? "prepared-transaction-horizon" : "session-horizon"],
    title: holder.name, explanation: "This observed native context retains a cleanup horizon; application ownership is unresolved.",
    metrics: [{ key: "native-distance", label: "Observed transaction distance", value: 0, unit: "text", text: holder.horizons[0]!.transactions_behind }],
    investigation: "Inspect this exact native session generation and transaction context.",
    verify: "Compare scope-qualified native horizons; an empty partial read does not prove recovery.",
  })));
  result.boundaries = (["data", "catalog", "shared-catalog"] as const).map((horizon) => ({
    horizon, complete: false, oldest_observed: [],
  }));
  return result;
}

export function cleanupPressure(retention: RetentionSnapshot): Pressure {
  return { uptime_hours: null, signals: [], grants: [], oldest_transaction: null, retention, files: [], replicas: [], cpus: null, numa_nodes: null };
}

function report(sources: SourceOutcome[], entries: CleanupExplanation[]): CleanupReport {
  return {
    sources, entries, title: english["view.cleanup.title"], summary: "Observed native cleanup evidence.",
    limitations: ["Independent native points do not establish a resource owner or continuous retention."], boundaries: [],
  };
}
