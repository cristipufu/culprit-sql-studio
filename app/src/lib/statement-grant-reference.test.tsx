import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { CachedGrantStatement } from "../components/CachedGrantStatement";
import type { PayloadUnavailable } from "../gen/PayloadUnavailable";
import type { Snapshot } from "../gen/Snapshot";
import type { StatementGrantReference } from "../gen/StatementGrantReference";
import { ORIGINAL_CACHED_SQL, OTHER_CACHED_SQL, statementGrant, statementGrantRead, statementGrantReference, statementGrantSnapshot } from "../test/statement-grants";
import { t } from "./i18n";
import { originalStatementGrant } from "./statement-grant-reference";

function show(snapshot: Snapshot, reference: StatementGrantReference): string {
  return renderToStaticMarkup(<CachedGrantStatement snapshot={snapshot} reference={reference} />);
}

test("the full native tuple selects exact SQL despite equal hashes and reordered rows, without creating a workload query", () => {
  const original = statementGrant();
  const other = statementGrant("0x0506", OTHER_CACHED_SQL);
  expect(other.context.query_hash).toBe(original.context.query_hash);
  const read = statementGrantRead([other, original]);
  const reference = statementGrantReference(read, original);
  const snapshot = statementGrantSnapshot([read]);
  expect(originalStatementGrant(snapshot, reference)?.row).toBe(original);
  const html = show(snapshot, reference);
  expect(html).toContain("cached-grant-canary");
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("unrelated_same_hash");
  expect(html).not.toContain("Open in Queries");
  expect(html).not.toContain("<button");
  expect(html).toContain("2026-11-01T06:00:00Z");
  expect(html).toContain("member-a");
});

test("a different read or outbox revision cannot supply the original statement", () => {
  const read = statementGrantRead();
  const reference = statementGrantReference(read);
  for (const field of ["observation_id", "outbox_id", "owner_epoch", "sequence"] as const) {
    const changed = structuredClone(read);
    if (field === "observation_id") changed.capture.observation_id += "-other";
    else changed.capture.revision![field] = field === "sequence" ? "13" : `${changed.capture.revision![field]}-other`;
    const snapshot = statementGrantSnapshot([changed]);
    expect(originalStatementGrant(snapshot, reference)).toBeNull();
    const html = show(snapshot, reference);
    expect(html).toContain(t("ui.statement_grants.missing"));
    expect(html).not.toContain("cached-grant-canary");
  }
  const unowned = structuredClone(read);
  unowned.capture.revision = null;
  expect(originalStatementGrant(statementGrantSnapshot([unowned]), reference)).toBeNull();
});

test("duplicate captures or cache identities stay ambiguous rather than choosing a representative", () => {
  const read = statementGrantRead();
  const reference = statementGrantReference(read);
  const duplicate = structuredClone(read);
  duplicate.observations[0]!.statement = { state: "unavailable", reason: "restricted" };
  expect(originalStatementGrant(statementGrantSnapshot([read, duplicate]), reference)).toBeNull();
  const rows = statementGrantRead([read.observations[0]!, structuredClone(read.observations[0]!)]);
  expect(originalStatementGrant(statementGrantSnapshot([rows]), reference)).toBeNull();
});

test("same-hash recycled handles offsets generations and creation times cannot borrow another cached entry", () => {
  const read = statementGrantRead();
  const reference = statementGrantReference(read);
  const replacements = [
    { plan_handle: "0x0506" }, { sql_handle: "0x0708" }, { statement_start_bytes: 2 }, { statement_end_bytes: 10 },
    { plan_generation: "2" }, { created_without_offset: "2026-11-01T04:00:00.004" },
  ];
  for (const replacement of replacements) {
    const changed = structuredClone(read);
    Object.assign(changed.observations[0]!.context.identity, replacement);
    expect(originalStatementGrant(statementGrantSnapshot([changed]), reference)).toBeNull();
  }
});

test("wrong dataset source target engine member scope or read coverage never renders cached SQL", () => {
  const original = statementGrantRead();
  const reference = statementGrantReference(original);
  const invalid = Array.from({ length: 10 }, () => structuredClone(original));
  const frame = (index: number) => {
    const observation = invalid[index]!.description.observation;
    if (observation.kind !== "snapshot") throw new Error("snapshot fixture");
    return observation.data;
  };
  frame(0).dataset = "statement-counters";
  invalid[1]!.description.source = "unknown native source";
  frame(2).sources[0]!.source = "unrelated native source";
  invalid[3]!.description.partition = { kind: "target" };
  const foreign = invalid[4]!.description.target;
  if (foreign.scope !== "database") throw new Error("database fixture");
  foreign.identity.database = "other-database";
  const otherEngine = invalid[5]!.description.target;
  if (otherEngine.scope !== "database") throw new Error("database fixture");
  otherEngine.identity.engine = "postgres";
  const member = invalid[6]!.description.partition;
  if (member.kind !== "instance-local" || !member.member) throw new Error("native member fixture");
  member.member.kind = "pool";
  frame(7).sources[0]!.state = "unavailable";
  frame(8).records += 1;
  const otherHost = invalid[9]!.description.target;
  if (otherHost.scope !== "database" || otherHost.identity.location.kind !== "server") throw new Error("server fixture");
  otherHost.identity.location.host = "other-instance.invalid";
  const scope = statementGrantSnapshot([original]);
  for (const read of invalid) {
    const snapshot = { ...scope, pressure: { ...scope.pressure!, grants: [read] } };
    expect(originalStatementGrant(snapshot, reference)).toBeNull();
    expect(show(snapshot, reference)).not.toContain("cached-grant-canary");
  }
});

test("an endpoint-only report can display its exact native-incarnation read without borrowing that proof", () => {
  for (const readKnowsIncarnation of [true, false]) {
    const read = statementGrantRead();
    const snapshot = statementGrantSnapshot([read]);
    const target = read.description.target;
    if (target.scope !== "database" || !snapshot.identity) throw new Error("database fixture");
    if (readKnowsIncarnation) target.identity.incarnation = "native-database-guid";
    else snapshot.identity.incarnation = "native-database-guid";
    const original = structuredClone(snapshot.identity);
    const reference = statementGrantReference(read);
    expect(originalStatementGrant(snapshot, reference)?.row.statement.state).toBe("available");
    expect(show(snapshot, reference)).toContain("cached-grant-canary");
    expect(snapshot.identity).toEqual(original);
    if (target.identity.location.kind !== "server") throw new Error("server fixture");
    target.identity.location.host = "unrelated-endpoint.invalid";
    expect(originalStatementGrant(snapshot, reference)).toBeNull();
  }
});

test("different known incarnations remain contradictory while a shared native incarnation permits server aliases", () => {
  const read = statementGrantRead();
  const target = read.description.target;
  if (target.scope !== "database" || target.identity.location.kind !== "server") throw new Error("server fixture");
  target.identity.incarnation = "native-database-guid";
  const snapshot = statementGrantSnapshot([read]);
  if (!snapshot.identity) throw new Error("database fixture");
  const reference = statementGrantReference(read);
  snapshot.identity.incarnation = "different-native-database-guid";
  expect(originalStatementGrant(snapshot, reference)).toBeNull();
  snapshot.identity.incarnation = "native-database-guid";
  target.identity.location.host = "original-server-alias.invalid";
  target.identity.database = "earlier-database-name";
  expect(originalStatementGrant(snapshot, reference)?.row.statement.state).toBe("available");
  expect(show(snapshot, reference)).toContain("cached-grant-canary");
});

test("unknown member and epoch preserve exact capture-local SQL with explicit qualification", () => {
  const read = statementGrantRead();
  read.description.partition = { kind: "instance-local", member: null, epoch: null };
  const snapshot = statementGrantSnapshot([read]);
  const reference = statementGrantReference(read);
  expect(originalStatementGrant(snapshot, reference)?.row.statement.state).toBe("available");
  const html = show(snapshot, reference);
  expect(html).toContain("cached-grant-canary");
  expect(html).toContain(t("view.statement_grants.limit.owner"));
  expect(html).not.toContain("Native cache member:");
});

test("unavailable native statement reasons stay explicit without fallback SQL or a new source action", () => {
  const reasons: PayloadUnavailable[] = ["missing", "restricted", "encrypted", "oversized", "redaction-failed", "not-collected", "source-unavailable"];
  for (const reason of reasons) {
    const read = statementGrantRead();
    read.observations[0]!.statement = { state: "unavailable", reason };
    const html = show(statementGrantSnapshot([read]), statementGrantReference(read));
    expect(html).toContain(t("ui.statement_grants.unavailable", { reason }));
    expect(html).not.toContain("<pre");
    expect(html).not.toContain("<button");
    expect(html).not.toContain(ORIGINAL_CACHED_SQL);
    expect(html).not.toContain(t("ui.statement_grants.missing"));
  }
});
