import type { MemoryClerkField } from "../gen/MemoryClerkField";
import type { MemoryClerkReport } from "../gen/MemoryClerkReport";
import type { Report } from "../gen/Report";
import { t } from "../lib/i18n";
import { partial } from "./fixtures";
import { healthReportFixture } from "./health-report";

export const EXACT_CLERK_ALLOCATION = "9007199254740993";

/** Already interpreted presentation data; native validity and grouping are tested in Rust. */
export function memoryClerkReport(count = 12): MemoryClerkReport {
  return partial<MemoryClerkReport>({
    context: { kind: "collector", observed_from: "2020-01-01T12:00:00Z", observed_until: "2020-01-01T12:00:01Z" },
    native_source: null, source_state: "partial", source_reason: null, read_complete: true,
    native_rows: count, observed_rows: count, presented_rows: count, entries_limited: false,
    title: t("collected.memory_clerks.title"),
    summary: t("collected.memory_clerks.summary.partial"),
    limitations: [t("collected.memory_clerks.limit.scope"), t("collected.memory_clerks.limit.dimensions"),
      t("collected.memory_clerks.limit.owner"), t("collected.memory_clerks.limit.clock")],
    entries: Array.from({ length: count }, (_, index) => ({
      observation_key: `clerk-${index}`, scope: "server" as const,
      clerk_type: "MEMORYCLERK_SQLBUFFERPOOL", name: `native-clerk-${index}`, memory_node_id: String(index),
      explanation: t("collected.memory_clerks.entry.explanation"), fields: allocationFields(),
    })),
  });
}

function allocationFields(): MemoryClerkField[] {
  return [
    { key: "clerk_count", label: t("collected.memory_clerks.field.clerk_count"), value: "1", unit: { kind: "count" } },
    { key: "pages_kb", label: t("collected.memory_clerks.field.pages_kb"), value: EXACT_CLERK_ALLOCATION, unit: { kind: "kibibytes" } },
    { key: "virtual_memory_reserved_kb", label: t("collected.memory_clerks.field.virtual_memory_reserved_kb"), value: "1024", unit: { kind: "kibibytes" } },
    { key: "virtual_memory_committed_kb", label: t("collected.memory_clerks.field.virtual_memory_committed_kb"), value: "512", unit: { kind: "kibibytes" } },
    { key: "awe_allocated_kb", label: t("collected.memory_clerks.field.awe_allocated_kb"), value: "0", unit: { kind: "kibibytes" } },
    { key: "shared_memory_reserved_kb", label: t("collected.memory_clerks.field.shared_memory_reserved_kb"), value: null, unit: { kind: "kibibytes" } },
    { key: "shared_memory_committed_kb", label: t("collected.memory_clerks.field.shared_memory_committed_kb"), value: null, unit: { kind: "kibibytes" } },
    { key: "minimum_page_size_in_bytes", label: t("collected.memory_clerks.field.minimum_page_size_in_bytes"), value: "8192", unit: { kind: "bytes" } },
    { key: "maximum_page_size_in_bytes", label: t("collected.memory_clerks.field.maximum_page_size_in_bytes"), value: "8192", unit: { kind: "bytes" } },
  ];
}

export function memoryClerkHealthReport(view: MemoryClerkReport | null = memoryClerkReport()): Report {
  return healthReportFixture({
    snapshot: { id: "memory-clerk-presentation", server: { engine: "mssql" }, resources: [], pressure: null,
      sources: [{ source: "memory clerks", state: view ? "partial" : "unavailable",
        reason: view ? null : t("collected.memory_clerks.source.unavailable") }],
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } },
    memory_clerks: view ?? undefined,
  });
}
