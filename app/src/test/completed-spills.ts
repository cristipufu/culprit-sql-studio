import english from "../../../locales/en.json" with { type: "json" };
import type { CachedRuntimeRead } from "../gen/CachedRuntimeRead";
import type { CompletedSpillReport } from "../gen/CompletedSpillReport";
import type { NativeReadContext } from "../gen/NativeReadContext";
import type { OperationalObservation } from "../gen/OperationalObservation";

export const COMPLETED_SPILL_SOURCE = "statement-runtime-cache";
export const EXACT_SPILL_COUNT = "9007199254740993";
export const SPILL_FROM = "2026-11-13T10:00:00.000000001Z";
export const SPILL_UNTIL = "2026-11-13T10:00:01.000000002Z";

const context = (): NativeReadContext => ({
  target: { scope: "database", identity: { engine: "mssql", database: "db", incarnation: "private-database-incarnation",
    location: { kind: "server", host: "private-native-host.invalid", port: 1433, instance: null } } },
  partition: { kind: "instance-local", member: { engine: "mssql", kind: "instance",
    namespace: { id: "private-native-machine", qualifiers: {} }, native: { id: "private-native-member", qualifiers: {} } }, epoch: "private-native-process" },
  observed_from: SPILL_FROM, observed_until: SPILL_UNTIL, database_writable: null,
});

function observation(index: number): OperationalObservation {
  return { source: COMPLETED_SPILL_SOURCE, key: String(index).padStart(64, "a"), fact: { kind: "mssql-cached-runtime", data: {
    context: { identity: { plan_handle: "0xaabbccdd", sql_handle: "0xeeff0011", statement_start_bytes: 0, statement_end_bytes: -1,
      created_without_offset: "2026-11-13T09:00:00.003", plan_generation: String(index + 1) },
      database_id: 7, query_hash: "0x0123456789ABCDEF", origin: "unknown", monitoring_template_id: null,
      source_text_bytes: null, source_text_complete: false },
    query_plan_hash: null, executions: "17", reset: { kind: "unknown" }, last_execution_without_offset: "2026-11-13T09:59:00.003",
    counters: Object.fromEntries(["worker-microseconds", "elapsed-microseconds", "clr-microseconds", "logical-reads", "physical-reads", "logical-writes", "rows", "spilled-pages"].map((metric) =>
      [metric, { total: EXACT_SPILL_COUNT, last: "3", minimum: "0", maximum: "4" }])),
  } } };
}

/** Synthetic transport data; native classification and receipt authentication stay in Rust tests. */
export function completedSpillFixture(count = 12): { read: CachedRuntimeRead; report: CompletedSpillReport } {
  const native = context();
  const rows = Array.from({ length: count }, (_, index) => observation(index));
  const read: CachedRuntimeRead = { read_id: "c".repeat(64), read_complete: true, observations: rows,
    description: { target: native.target, partition: native.partition, source: "private-logical-reader", continuity: "unverified",
      observation: { kind: "snapshot", data: { dataset: "cached-runtime", observed_from: SPILL_FROM, observed_until: SPILL_UNTIL,
        records: count, sources: [{ source: COMPLETED_SPILL_SOURCE, state: count === 0 ? "empty" : "collected", records: count,
          complete_scope: true, error_code: null, stopped: null }] } } } };
  const report: CompletedSpillReport = {
    context: native, sources: [{ source: COMPLETED_SPILL_SOURCE, state: count === 0 ? "empty" : "collected", reason: null }],
    read_complete: true, native_rows: count, observed_rows: count,
    definition_context: null, definition_sources: [], definition_read_complete: null,
    title: english["view.completed_spills.title"],
    summary: count === 0 ? english["view.completed_spills.summary.missing"] : "Recorded completed-statement entries.",
    limitations: [english["view.completed_spills.limit.cause"], english["view.completed_spills.limit.population"]],
    entries: rows.map((row, index) => {
      if (row.fact.kind !== "mssql-cached-runtime") throw new Error("Cache-runtime fixture required");
      return { observation: { kind: "native-read", read_key: "d".repeat(64), observation_key: row.key },
        identity: row.fact.data.context.identity, database_id: 7, origin: "unknown", reset: { kind: "unknown" },
        monitoring_template_id: null, state: "recorded-spills", statement_binding: "not-requested", definition: null,
        statement_preview: null, statement_preview_truncated: false, statement_unavailable: "not-collected",
        title: `Completed statement ${index + 1}`,
        explanation: `${english["view.completed_spills.entry.recorded_detail"]} ${english["view.completed_spills.binding.not_requested"]}`,
        metrics: [{ key: "view.completed_spills.metric.total", label: english["view.completed_spills.metric.total"], unit: "text", value: 0, text: EXACT_SPILL_COUNT },
          { key: "view.completed_spills.metric.last_start", label: english["view.completed_spills.metric.last_start"], unit: "text", value: 0, text: "2026-11-13T09:59:00.003" }],
        investigation: english["view.completed_spills.investigate"], verify: english["view.completed_spills.verify"],
      };
    }),
  };
  return { read, report };
}

export function partialCompletedSpillFixture() {
  const fixture = completedSpillFixture(1);
  fixture.read.read_complete = false;
  if (fixture.read.description.observation.kind !== "snapshot") throw new Error("Cache-runtime snapshot required");
  fixture.read.description.observation.data.records = 3;
  fixture.read.description.observation.data.sources[0]!.records = 3;
  fixture.report.read_complete = false;
  fixture.report.native_rows = 3;
  fixture.report.sources[0]!.state = "partial";
  fixture.report.sources[0]!.reason = english["view.completed_spills.limit.partial"];
  fixture.report.definition_context = { ...context(), observed_from: "2026-11-13T10:01:00Z", observed_until: "2026-11-13T10:01:01Z" };
  fixture.report.definition_read_complete = false;
  fixture.report.definition_sources = [{ source: "independent-definitions", state: "partial", reason: "Some definition bodies were not read." }];
  fixture.report.entries[0]!.statement_binding = "definition-not-observed";
  fixture.report.entries[0]!.explanation = english["view.completed_spills.binding.not_observed"];
  return fixture;
}
