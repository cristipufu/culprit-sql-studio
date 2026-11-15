import type { CachedStatementIdentity } from "../gen/CachedStatementIdentity";
import type { CaptureIdentity } from "../gen/CaptureIdentity";
import type { DatabaseIdentity } from "../gen/DatabaseIdentity";
import type { Snapshot } from "../gen/Snapshot";
import type { StatementGrantReference } from "../gen/StatementGrantReference";
import type { StatementGrantSnapshot } from "../gen/StatementGrantSnapshot";
import type { StatementGrantStatistics } from "../gen/StatementGrantStatistics";

// The stable source required by MemoryGrantCheck's evidence contract, not its translated label.
const GRANT_SOURCE = "memory grants per query";

export type OriginalStatementGrant = { read: StatementGrantSnapshot; row: StatementGrantStatistics };

/** Locate one original row in this immutable report. Opaque backend keys are never recreated in the frontend. */
export function originalStatementGrant(snapshot: Snapshot, reference: StatementGrantReference): OriginalStatementGrant | null {
  if (!reference.snapshot_key || !reference.observation_key) return null;
  const reads = (snapshot.pressure?.grants ?? []).filter((read) => sameCapture(read.capture, reference.capture));
  if (reads.length !== 1) return null;
  const read = reads[0]!;
  if (!eligibleRead(snapshot, read)) return null;
  const rows = read.observations.filter((row) => sameEntry(row.context.identity, reference.identity));
  return rows.length === 1 ? { read, row: rows[0]! } : null;
}

function sameCapture(left: CaptureIdentity, right: CaptureIdentity): boolean {
  if (left.observation_id !== right.observation_id) return false;
  const a = left.revision;
  const b = right.revision;
  if (a === null || b === null) return a === b;
  return a.outbox_id === b.outbox_id && a.owner_epoch === b.owner_epoch && a.sequence === b.sequence;
}

function sameEntry(left: CachedStatementIdentity, right: CachedStatementIdentity): boolean {
  return left.plan_handle === right.plan_handle && left.sql_handle === right.sql_handle
    && left.statement_start_bytes === right.statement_start_bytes && left.statement_end_bytes === right.statement_end_bytes
    && left.created_without_offset === right.created_without_offset && left.plan_generation === right.plan_generation;
}

function eligibleRead(snapshot: Snapshot, read: StatementGrantSnapshot): boolean {
  const source = read.description;
  if (source.source !== GRANT_SOURCE || source.target.scope !== "database" || source.partition.kind !== "instance-local"
    || source.observation.kind !== "snapshot" || source.observation.data.dataset !== "statement-grants") return false;
  const database = source.target.identity;
  if (database.engine !== snapshot.server.engine) return false;
  if (snapshot.identity != null) {
    if (snapshot.identity.engine !== snapshot.server.engine || snapshot.identity.database !== snapshot.server.database
      || !sameDatabase(database, snapshot.identity)) return false;
  } else if (database.database !== snapshot.server.database) return false;
  const frame = source.observation.data;
  if (frame.sources.length !== 1 || frame.records !== read.observations.length) return false;
  const coverage = frame.sources[0]!;
  if (coverage.source !== source.source || coverage.records !== frame.records) return false;
  const readable = (coverage.state === "collected" && coverage.complete_scope) || (coverage.state === "partial" && !coverage.complete_scope);
  if (!readable) return false;
  const { member, epoch } = source.partition;
  if (member && (member.engine !== database.engine || (member.kind !== "instance" && member.kind !== "node"))) return false;
  return (member !== null && epoch !== null) || coverage.state === "partial";
}

function sameDatabase(left: DatabaseIdentity, right: DatabaseIdentity): boolean {
  if (left.engine !== right.engine) return false;
  // This locates original display evidence; an absent native fact is not a contradictory incarnation.
  if (left.incarnation?.trim() && right.incarnation?.trim()) {
    if (left.incarnation !== right.incarnation) return false;
    // Native server identity survives endpoint/name aliases. Local file IDs remain scoped to their path.
    if (left.location.kind !== "file" && right.location.kind !== "file") return true;
  }
  if (left.database !== right.database) return false;
  const a = left.location;
  const b = right.location;
  switch (a.kind) {
    case "server": return b.kind === "server" && a.host === b.host && a.port === b.port && a.instance === b.instance;
    case "socket": return b.kind === "socket" && a.directory === b.directory && a.port === b.port;
    case "file": return b.kind === "file" && a.path === b.path;
    case "unknown": return b.kind === "unknown";
  }
}
