import type { NativeReadContext } from "../gen/NativeReadContext";
import type { Report } from "../gen/Report";
import type { TempdbMetadataMemoryReport } from "../gen/TempdbMetadataMemoryReport";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";
import { healthReportFixture } from "./health-report";

export const EXACT_TEMPDB_RESERVATION = "9007199254740993";
export type MetadataMode = "usage" | "disabled" | "unknown" | "partial";

/** Already interpreted presentation data; native source qualification is tested in Rust. */
export function tempdbMetadataReport(mode: MetadataMode = "usage", count = 12): TempdbMetadataMemoryReport {
  const hasConsumers = mode === "usage" || mode === "partial";
  const state = mode === "partial" || mode === "unknown" ? "partial" : "collected";
  const owner = { engine: "mssql" as const, kind: "instance" as const,
    namespace: { id: "private-metadata-machine", qualifiers: {} }, native: { id: "private-metadata-instance", qualifiers: {} } };
  return {
    context: { kind: "native", context: partial<NativeReadContext>({
      target: { scope: "shared", identity: owner }, partition: { kind: "instance-local", member: owner, epoch: "private-process-epoch" },
      observed_from: "2020-01-01T12:00:00Z", observed_until: "2020-01-01T12:00:01Z", database_writable: null,
    }) },
    native_source: { source: "tempdb metadata memory", state, records: hasConsumers ? count+1 : 1,
      complete_scope: state === "collected", error_code: null, stopped: null },
    source_state: state, read_complete: true, metadata_enabled: mode === "unknown" ? null : mode !== "disabled",
    feature_observed_at: "2020-01-01T12:00:00.2Z", tempdb_created_without_offset: "2020-01-01T14:00:00.003",
    native_rows: hasConsumers ? count+1 : 1, observed_rows: hasConsumers ? count+1 : 1,
    title: t("view.tempdb_metadata_memory.title"),
    summary: t(mode === "disabled" ? "view.tempdb_metadata_memory.summary.disabled" : hasConsumers ? "view.tempdb_metadata_memory.summary.recorded" : "view.tempdb_metadata_memory.summary.unavailable"),
    limitations: [t("view.tempdb_metadata_memory.limit.scope"), t("view.tempdb_metadata_memory.limit.point"),
      t("view.tempdb_metadata_memory.limit.owner"), ...(state === "partial" ? [t("view.tempdb_metadata_memory.limit.partial")] : [])],
    entries: hasConsumers ? Array.from({ length: count }, (_, index) => ({
      observation_key: `private-metadata-row-${index}`, observed_at: "2020-01-01T12:00:00.5Z",
      native_consumer_id: String(42+index), native_type_code: "2", native_type_name: "VARHEAP",
      description: `consumer-${index}`, allocated_bytes: EXACT_TEMPDB_RESERVATION, used_bytes: index === 0 ? "0" : null,
      explanation: t(index === 0 ? "view.tempdb_metadata_memory.usage.reservation" : "view.tempdb_metadata_memory.usage.unknown"), investigation: t("view.tempdb_metadata_memory.investigate"),
      verify: t("view.tempdb_metadata_memory.verify"),
    })) : [],
  };
}

export function tempdbMetadataHealthReport(view: TempdbMetadataMemoryReport | null = tempdbMetadataReport()): Report {
  return healthReportFixture({
    snapshot: { id: "metadata-presentation", server: { engine: "mssql" }, pressure: null, resources: [],
      sources: [{ source: "tempdb metadata memory", state: view ? view.source_state : "unavailable",
        reason: view ? null : t("view.tempdb_metadata_memory.source.denied") }],
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    tempdb_metadata_memory: view ?? undefined,
  });
}
