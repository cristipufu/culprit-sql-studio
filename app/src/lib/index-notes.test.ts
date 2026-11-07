import { expect, test } from "bun:test";
import type { Engine } from "../gen/Engine";
import type { Finding } from "../gen/Finding";
import type { IndexStat } from "../gen/IndexStat";
import type { Report } from "../gen/Report";
import type { TableStat } from "../gen/TableStat";
import { partial } from "../test/fixtures";
import { countingSince, indexNotes, type Relation } from "./schema-model";

const table = partial<TableStat>({ schema: "app", name: "orders" });
const takenAt = "2026-10-20T12:00:00Z";

function report(engine: Engine, findings: Finding[] = []): Report {
  return partial<Report>({ snapshot: { taken_at: takenAt, server: { engine, started_at: null } }, findings });
}

function index(name: string, extra: Partial<IndexStat> = {}): IndexStat {
  return partial<IndexStat>({
    schema: "app", table: "orders", name, kind: "btree", columns: ["status"], include: [], filter: null, reads: 0, writes: 500,
    is_primary_key: false, is_unique: false, is_clustered: false, is_disabled: false, created_at_latest: null, ...extra,
  });
}

const fk: Relation = partial<Relation>({ fk: { name: "orders_customer_fk", columns: ["customer_id"], ref_table: "customers" }, indexed: true });
const unusedFinding = (name: string) => partial<Finding>({ id: `unused-index:app.orders.${name}`, check: "unused-index", subject: { kind: "index", name: `app.orders.${name}` } });

test("an index nothing reads is called unused, and links the finding that says so", () => {
  const i = index("ix_orders_status");
  const notes = indexNotes(report("postgres", [unusedFinding(i.name)]), table, i, [i], []);
  expect(notes).toEqual([{ tone: "high", text: "Never read since the last restart, yet every write to orders maintains it", finding: "unused-index:app.orders.ix_orders_status" }]);
  expect(indexNotes(report("postgres"), table, { ...i, reads: 12 }, [i], [])).toEqual([]);
  expect(indexNotes(report("postgres"), table, { ...i, reads: null }, [i], [])).toEqual([]);
});

test("an unread index is not called unused when something explains it", () => {
  const r = report("postgres");
  const young = index("ix_new", { created_at_latest: "2026-10-18T09:00:00Z" });
  expect(indexNotes(r, table, young, [young], [])[0]).toMatchObject({ tone: "low", text: expect.stringContaining("too soon to call it unused") });

  const unique = index("ux_orders_number", { is_unique: true });
  expect(indexNotes(r, table, unique, [unique], [])).toEqual([{ tone: "low", text: "Never read since the last restart; it still enforces uniqueness" }]);

  const pk = index("orders_pkey", { is_primary_key: true, is_unique: true });
  expect(indexNotes(r, table, pk, [pk], [])).toEqual([]);

  const backing = index("ix_orders_customer", { columns: ["customer_id"] });
  expect(indexNotes(r, table, backing, [backing], [fk])[0]!.text).toBe(
    "Never read since the last restart, but it backs foreign key orders_customer_fk: without it, deleting a customers row scans orders",
  );
  expect(indexNotes(report("mysql"), table, backing, [backing], [fk])[0]!.text).toEndWith("InnoDB requires it");
  const filtered = { ...backing, filter: "customer_id IS NOT NULL" };
  expect(indexNotes(r, table, filtered, [filtered], [fk])[0]!.tone).toBe("high");
});

test("YugabyteDB's counters only call an index unused when a finding vouches for them", () => {
  const i = index("ix_orders_status", { kind: "lsm", docdb: { hash_columns: [], tablets: 3 } });
  const unproven = indexNotes(report("yugabyte"), table, i, [i], []);
  expect(unproven).toHaveLength(1);
  expect(unproven[0]!.tone).toBe("low");
  expect(unproven[0]!.text).toStartWith("No reads recorded;");
  expect(countingSince(report("yugabyte"))).toBe("across the recorded cluster nodes, with independent reset dates");
  expect(indexNotes(report("yugabyte", [unusedFinding(i.name)]), table, i, [i], [])[0]!.tone).toBe("high");
});

test("disabled and covered indexes say why", () => {
  const r = report("mysql");
  const invisible = index("ix_hidden", { reads: 4, is_disabled: true, state: "invisible" });
  expect(indexNotes(r, table, invisible, [invisible], [])).toEqual([{ tone: "medium", text: "Invisible to the optimizer; still maintained by writes" }]);
  const invalid = index("ix_broken", { reads: 4, is_disabled: true, state: "invalid", maintained: null });
  expect(indexNotes(r, table, invalid, [invalid], [])[0]!.text).toBe("Invalid: unavailable to the optimizer; maintenance state was not recorded");

  const narrow = index("ix_status", { reads: 4 });
  const wide = index("ix_status_created", { reads: 90, columns: ["status", "created_at"] });
  expect(indexNotes(r, table, narrow, [narrow, wide], [])).toEqual([{ tone: "medium", text: "Covered by ix_status_created: it leads with the same keys", finding: undefined }]);
});
