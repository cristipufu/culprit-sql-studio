import { test } from "bun:test";
import type { Finding } from "../gen/Finding";
import type { FileIo } from "../gen/FileIo";
import type { FileReference } from "../gen/FileReference";
import type { FindingFacts } from "../gen/FindingFacts";
import type { Replica } from "../gen/Replica";
import type { Report } from "../gen/Report";
import type { QueryStat } from "../gen/QueryStat";
import { findingStatus, querySignalStatus, replicaState, replicaStatus, storageStatus } from "./health";
import { partial } from "../test/fixtures";

const check = (condition: boolean, message: string) => { if (!condition) throw new Error(message); };
const finding = (extra: Partial<Finding>): Finding => {
  const severity = extra.severity ?? "low";
  return {
    id: "example", check: "example", severity, inactive: false, queries: [], metrics: [],
    subject: { kind: "database", name: "example" },
    // As the checks classify them: a low finding is optional, a higher one needs attention.
    actionability: severity === "low" ? "optional-improvement" : "needs-attention",
    ...extra,
  } as Finding;
};
const report = (findings: Finding[]) => partial<Report>({ findings });

function reference(file: FileIo): FileReference {
  return { name: file.name, kind: file.kind, database: file.database, scope: file.scope ?? null,
    stats_reset: file.stats_reset ?? null, write_operation: file.write_operation ?? null };
}

test("Health warnings follow measured evidence", () => {
  // A tiny query can spend nearly all its time planning or in JIT without costing a meaningful amount of time.
  const tiny = { id: "tiny", executions: 1000, total_time_ms: 1, costs: { jit_ms: 0.9, plan_ms: 10 } } as QueryStat;
  check(querySignalStatus(report([]), tiny, ["jit-overhead"]) === "info", "A large ratio alone cannot warn");
  check(querySignalStatus(report([finding({ check: "planning-time", severity: "high", inactive: true, queries: [{ id: "tiny", plan_node: null, note: null }] })]), tiny, ["planning-time"]) === "info", "Stopped findings cannot alarm current health");
  check(querySignalStatus(report([finding({ check: "jit-overhead", severity: "medium", queries: [{ id: "tiny", plan_node: null, note: null }] })]), tiny, ["jit-overhead"]) === "watch", "A supported warning must remain visible");

  // Bulk reads are a Low context finding; a file with the same name in another database is separate evidence.
  const file = { name: "data, archive", database: "shop", kind: "data", reads: 10000, read_ms: 1_000_000 } as FileIo;
  const storage = finding({ id: "opaque-storage", check: "slow-storage", facts: partial<FindingFacts>({ storage_files: [reference(file)] }) });
  // A log said as one (MySQL's redo log) covers each of its files, whatever the finding calls it.
  const redo = { name: "#ib_redo10", database: "shop", kind: "log", reads: 0, writes: 10000, write_ms: 300_000 } as FileIo;
  const log = finding({ id: "opaque-log", check: "slow-storage", severity: "medium", facts: partial<FindingFacts>({ storage_files: [reference({ ...redo, name: "#ib_redo9" }), reference(redo)] }) });
  check(storageStatus(report([log]), redo) === "watch", "Each file of a slow log shows it");
  check(storageStatus(report([log]), file) === "info", "A slow log says nothing of a data file");
  check(storageStatus(report([storage]), file) === "info", "Bulk or historical storage context cannot become a red disk warning");
  check(storageStatus(report([{ ...storage, id: "opaque-other", severity: "high", actionability: "needs-attention", facts: partial<FindingFacts>({ storage_files: [reference({ ...file, database: "other" })] }) }]), file) === "info", "Another database's matching filename cannot warn here");
  check(storageStatus(report([{ ...storage, severity: "medium", actionability: "needs-attention" }]), file) === "watch", "Assessed slow storage still warns");

  check(storageStatus(report([{ ...storage, severity: "high", actionability: "needs-attention" }]), { ...file, scope: "server" }) === "info", "Shared and database-local sources remain distinct");
  check(storageStatus(report([{ ...storage, severity: "high", actionability: "needs-attention" }]), { ...file, name: "archive" }) === "info", "Commas are part of a filename");

  // An inactive slot may be between syncs or bounded by a WAL cap; inactivity and byte count alone do not grade it.
  const slot = { name: "batch_consumer", role: "slot", condition: "slot_inactive", state: null, lag_bytes: 2 * 1024 ** 3 } as const;
  check(replicaStatus(report([]), slot as Replica) === "info", "An inactive slot alone cannot become an incident");
  check(replicaStatus(report([finding({ check: "replica-lag", subject: { kind: "database", name: slot.name } })]), slot as Replica) === "info", "A bounded slot's Low finding stays informational");
  check(findingStatus(finding({ severity: "high" })) === "bad", "Confirmed incidents retain their warning");
});

test("Information and history findings never become health warnings", () => {
  for (const actionability of ["information", "history"] as const) {
    const observation = finding({ severity: "high", actionability });
    check(findingStatus(observation) === "info", `${actionability} never creates a health warning`);
  }
});

test("A replica's state is its condition said from the catalog, or the engine's own word", () => {
  const replica = (extra: Partial<Replica>) => ({ name: "r", role: "this-server", condition: "reported", state: null, ...extra }) as Replica;
  check(replicaState(replica({ state: "streaming" })) === "streaming", "The engine's own word is shown as it is");
  check(replicaState(replica({ condition: "not_receiving" })) === "not receiving", "A condition Culprit established is said from the catalog");
  check(replicaState(replica({ condition: "receiving_stopped", state: "Connecting" })) === "receiving stopped (I/O thread: Connecting)", "The I/O thread's own value stays inside the words");
  check(replicaState(replica({ condition: "slot_inactive" })) === "inactive", "A slot says whether a consumer is attached");
  check(replicaState(replica({ condition: "stopped", state: "NOT SYNCHRONIZING" })) === "NOT SYNCHRONIZING", "A stop the engine words keeps its word");
});
