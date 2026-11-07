import type { ColumnInfo } from "../gen/ColumnInfo";
import type { Engine } from "../gen/Engine";
import type { Finding } from "../gen/Finding";
import type { ForeignKey } from "../gen/ForeignKey";
import type { IndexStat } from "../gen/IndexStat";
import type { Report } from "../gen/Report";
import type { TableStat } from "../gen/TableStat";
import type { TablePreviewOptions } from "../gen/TablePreviewOptions";
import type { ColumnSort } from "../gen/ColumnSort";
import { sqlSyntax } from "./engines";
import { when } from "./format";

export const tableKey = (t: { schema: string; name: string }) => `${t.schema}.${t.name}`;

const bare = (column: string) =>
  column
    .replace(/\s+(ASC|DESC)$/i, "")
    .replace(/[[\]"`]/g, "")
    .toLowerCase();

/** Findings about a table: its own, its indexes', and table-level ones named without the schema (`customers.email`). */
export function tableFindings(report: Report, table: TableStat): Finding[] {
  const qualified = tableKey(table).toLowerCase();
  const short = table.name.toLowerCase();
  return report.findings.filter((f) => {
    if (f.subject.kind !== "table" && f.subject.kind !== "index") return false;
    const name = f.subject.name.toLowerCase();
    return name === qualified || name.startsWith(`${qualified}.`) || name === short || name.startsWith(`${short}.`);
  });
}

export type Match = { score: number; hits: number[] };

/** Fuzzy subsequence match of `query` in `text`; contiguous runs, word starts and name prefixes score higher. */
export function fuzzy(text: string, query: string): Match | null {
  const q = query.trim().toLowerCase();
  if (!q) return { score: 0, hits: [] };
  const t = text.toLowerCase();
  const direct = t.indexOf(q);
  if (direct >= 0) {
    const atWord = direct === 0 || /[._\s-]/.test(t[direct - 1] ?? "");
    return {
      score: 1000 - direct + (atWord ? 200 : 0) + q.length * 10,
      hits: [...Array(q.length).keys()].map((i) => direct + i),
    };
  }
  const hits: number[] = [];
  let score = 0;
  let from = 0;
  for (const ch of q) {
    const at = t.indexOf(ch, from);
    if (at < 0) return null;
    const previous = hits.at(-1);
    score += previous === at - 1 ? 8 : 1;
    if (at === 0 || /[._\s-]/.test(t[at - 1] ?? "")) score += 5;
    hits.push(at);
    from = at + 1;
  }
  return { score, hits };
}

export type Membership = {
  index: IndexStat;
  position: number | null;
  included: boolean;
};

/** The indexes a column takes part in: as a key (with its position) or as an included column. */
export function columnMembership(indexes: IndexStat[], column: string): Membership[] {
  const name = column.toLowerCase();
  return indexes.flatMap((index): Membership[] => {
    const position = index.columns.map(bare).indexOf(name);
    if (position >= 0) return [{ index, position, included: false }];
    if (index.include.map(bare).includes(name)) return [{ index, position: null, included: true }];
    return [];
  });
}

/** An index whose keys are a leading prefix of another's on the same table (and isn't unique): the wider one can serve it. */
export function coveredBy(index: IndexStat, siblings: IndexStat[]): IndexStat | null {
  if (index.is_primary_key || index.is_unique || index.filter) return null;
  const keys = index.columns.map(bare);
  return (
    siblings.find((other) => {
      if (other === index || other.filter || other.columns.length < keys.length) return false;
      if (other.is_disabled || other.is_clustered || other.kind !== index.kind) return false;
      const native = index.docdb;
      const otherNative = other.docdb;
      if (native || otherNative || index.kind.toLowerCase() === "lsm") {
        if (!native || !otherNative || native.hash_columns.length !== otherNative.hash_columns.length || !native.hash_columns.every((c, n) => bare(c) === bare(otherNative.hash_columns[n] ?? ""))) return false;
      }
      if (!index.include.every((c) => [...other.columns, ...other.include].some((held) => bare(held) === bare(c)))) return false;
      if (!index.columns.every((c, n) => c.toLowerCase() === other.columns[n]?.toLowerCase() && (index.opclasses?.[n] ?? "").toLowerCase() === (other.opclasses?.[n] ?? "").toLowerCase())) return false;
      const theirs = other.columns.map(bare);
      return keys.every((k, i) => theirs[i] === k) && (other.columns.length > keys.length || other.name < index.name);
    }) ?? null
  );
}

export function canSeek(index: IndexStat, equality: string[]): boolean {
  return index.docdb ? index.docdb.hash_columns.every((hash) => equality.some((c) => bare(c) === bare(hash))) : index.kind.toLowerCase() !== "lsm";
}

export type Relation = { fk: ForeignKey; indexed: boolean | null };

/** Foreign keys from this table (with whether an index leads with their columns) and to it. */
export function relations(report: Report, table: TableStat): { outgoing: Relation[]; incoming: ForeignKey[] } {
  const { foreign_keys, indexes } = report.snapshot;
  const same = (schema: string, name: string) => schema.toLowerCase() === table.schema.toLowerCase() && name.toLowerCase() === table.name.toLowerCase();
  const own = indexes.filter((i) => same(i.schema, i.table));
  const outgoing = foreign_keys
    .filter((fk) => same(fk.schema, fk.table))
    .map((fk) => {
      const cols = fk.columns.map(bare);
      const indexed = own.some((i) => {
        const keys = i.columns.map(bare);
        return !i.is_disabled && !i.filter && canSeek(i, fk.columns) && cols.every((c) => keys.slice(0, cols.length).includes(c));
      });
      const unknown = report.snapshot.server.engine === "yugabyte" && (
        own.some((i) => !i.docdb) || (report.snapshot.sources ?? []).some((source) =>
          ["indexes", "tables", "table storage"].includes(source.source) && ["partial", "unavailable"].includes(source.state)) ||
        !(report.snapshot.sources ?? []).some((source) => source.source === "indexes" && ["collected", "empty"].includes(source.state))
      );
      return { fk, indexed: indexed ? true : unknown ? null : false };
    });
  const incoming = foreign_keys.filter((fk) => same(fk.ref_schema, fk.ref_table));
  return { outgoing, incoming };
}

/** Column name → the table it references through a single-column foreign key. */
/** The foreign keys from a table (with whether an index backs each) and to it. */
export type TableRelations = ReturnType<typeof relations>;

export function referenceOf(outgoing: Relation[], column: string): ForeignKey | null {
  const name = column.toLowerCase();
  return outgoing.find((r) => r.fk.columns.length === 1 && bare(r.fk.columns[0] ?? "") === name)?.fk ?? null;
}

/** How long the index counters have been counting, as words that follow "Never read …". */
export function countingSince(report: Report): string {
  if (report.snapshot.server.engine === "yugabyte") return "across the recorded cluster nodes, with independent reset dates";
  const since = report.snapshot.server.started_at;
  return since ? `since ${when(since).short}` : "since the last restart";
}

/** The foreign key an index serves: it is usable and unfiltered, can seek on the key, and leads with its columns. */
export function backedForeignKey(index: IndexStat, outgoing: Relation[]): ForeignKey | null {
  const keys = index.columns.map((c) => c.replace(/\s+(ASC|DESC)$/i, "").toLowerCase());
  return outgoing.find(({ fk }) => !index.is_disabled && !index.filter && canSeek(index, fk.columns) && fk.columns.every((c, n) => keys[n] === c.toLowerCase()))?.fk ?? null;
}

/** A line under an index in the Indexes tab; `finding` is the id of the finding it repeats, when there is one. */
export type IndexNote = { tone: "high" | "medium" | "low"; text: string; finding?: string };

const WEEK_MS = 7 * 24 * 3600 * 1000;

/**
 * What the Indexes tab says about one index, by the same evidence the checks use: an index that isn't read is called
 * unused only when nothing explains it. It may be under a week old, enforce uniqueness, back a foreign key, or (on
 * YugabyteDB) have counters no unused-index finding vouched for; each of those says so instead.
 */
export function indexNotes(report: Report, table: TableStat, index: IndexStat, siblings: IndexStat[], outgoing: Relation[]): IndexNote[] {
  const engine = report.snapshot.server.engine;
  const since = countingSince(report);
  const subject = `${tableKey(table)}.${index.name}`.toLowerCase();
  const finding = (check: string) => report.findings.find((f) => f.check === check && f.subject.kind === "index" && f.subject.name.toLowerCase() === subject);
  const unused = finding("unused-index");
  const unprovenNativeAbsence = engine === "yugabyte" && index.reads === 0 && !index.is_primary_key && !unused;
  const neverRead = index.reads === 0 && !index.is_primary_key && !unprovenNativeAbsence;
  const tooNew = index.created_at_latest != null && Date.parse(report.snapshot.taken_at) - Date.parse(index.created_at_latest) < WEEK_MS;
  const serves = backedForeignKey(index, outgoing);
  const wider = coveredBy(index, siblings);
  const notes: IndexNote[] = [];
  if (index.is_disabled) {
    const maintained = index.maintained == null ? "maintenance state was not recorded" : index.maintained ? "still maintained by writes" : "not ready for write maintenance";
    const text =
      index.state === "invisible" ? "Invisible to the optimizer; still maintained by writes"
      : index.state === "ignored" ? "Ignored by the optimizer; still maintained by writes"
      : index.state === "invalid" ? `Invalid: unavailable to the optimizer; ${maintained}`
      : "Disabled: unavailable to the optimizer and not maintained";
    notes.push({ tone: "medium", text });
  }
  if (unprovenNativeAbsence) {
    notes.push({ tone: "low", text: `No reads recorded; complete cluster usage with at least a week of known reset history is needed before calling this index unused.${serves ? ` It also supports foreign key ${serves.name}.` : ""}` });
  }
  if (neverRead && serves) {
    const why = engine === "mysql" ? "InnoDB requires it" : `without it, deleting a ${serves.ref_table} row scans ${table.name}`;
    notes.push({ tone: "low", text: `Never read ${since}, but it backs foreign key ${serves.name}: ${why}` });
  } else if (neverRead && !index.is_unique && tooNew) {
    notes.push({ tone: "low", text: `Not read yet, but it may be new (it existed by ${when(index.created_at_latest).short}): too soon to call it unused` });
  } else if (neverRead && !index.is_unique) {
    notes.push({ tone: "high", text: `Never read ${since}, yet every write to ${table.name} maintains it`, finding: unused?.id });
  } else if (neverRead) {
    notes.push({ tone: "low", text: `Never read ${since}; it still enforces uniqueness` });
  }
  if (wider) notes.push({ tone: "medium", text: `Covered by ${wider.name}: it leads with the same keys`, finding: finding("duplicate-index")?.id });
  return notes;
}

export function quoteIdent(engine: Engine, name: string): string {
  switch (sqlSyntax(engine)) {
    case "mssql":
      return `[${name.replaceAll("]", "]]")}]`;
    case "mysql":
      return `\`${name.replaceAll("`", "``")}\``;
    case "postgres":
    case "sqlite":
      return `"${name.replaceAll('"', '""')}"`;
  }
}

/** A read-only statement for the first `limit` rows of a table, with every name quoted for the engine. */
export function previewSql(engine: Engine, table: Pick<TableStat, "schema" | "name">, limit: number, options?: Pick<TablePreviewOptions, "filter" | "order">): string {
  const name = `${quoteIdent(engine, table.schema)}.${quoteIdent(engine, table.name)}`;
  let sql = sqlSyntax(engine) === "mssql" ? `SELECT TOP (${limit}) * FROM ${name}` : `SELECT * FROM ${name}`;
  if (options?.filter?.trim()) sql += ` WHERE (\n${options.filter.trim()}\n)`;
  if (options?.order) sql += ` ORDER BY ${quoteIdent(engine, options.order.column)} ${options.order.direction.toUpperCase()}`;
  if (sqlSyntax(engine) !== "mssql") sql += ` LIMIT ${limit}`;
  return sql;
}

export function nextColumnSort(order: ColumnSort | null, column: string): ColumnSort | null {
  return order?.column !== column ? { column, direction: "asc" } : order.direction === "asc" ? { column, direction: "desc" } : null;
}

/** Approximate DDL rebuilt from catalog metadata (columns, primary key, indexes, foreign keys). */
export function ddl(engine: Engine, table: TableStat, columns: ColumnInfo[], indexes: IndexStat[], outgoing: Relation[]): string {
  const original = table.definition ? `${table.definition.trimEnd().replace(/;$/, "")};` : null;
  if (table.kind === "hypertable" || table.kind === "timescale-unresolved") {
    return "-- TimescaleDB manages this table's partitioning, chunks, compression and policies.\n-- The collected column/index metadata is not a complete recreation script.\n-- Export its schema with the TimescaleDB tools before recreating it.";
  }
  if (table.kind === "virtual") return original ?? "-- The virtual table's original definition was not collected. Read it from the database before recreating it.";
  if (engine === "yugabyte" && !table.storage?.docdb)
    return original ?? "-- Native DocDB sharding metadata was unavailable. Export this table's YSQL schema before recreating it.";
  if (engine === "yugabyte" && columns.some((c) => c.generated))
    return original ?? "-- Complete native generated/identity column definitions were not collected. Export this table's YSQL schema before recreating it.";
  const q = (n: string) => quoteIdent(engine, n);
  const name = `${q(table.schema)}.${q(table.name)}`;
  const lines = columns.map((c) => {
    const parts = [`  ${q(c.name)} ${c.data_type}`];
    if (c.generated) parts.push(engine === "mssql" ? "IDENTITY" : engine === "mysql" ? "AUTO_INCREMENT" : "GENERATED");
    parts.push(c.nullable ? "NULL" : "NOT NULL");
    if (c.default && !c.generated) parts.push(`DEFAULT ${c.default}`);
    return parts.join(" ");
  });
  const pk = indexes.find((i) => i.is_primary_key);
  if (pk) {
    if (engine === "yugabyte" && !pk.docdb) return "-- Native primary-key sharding was unavailable. Export this table's YSQL schema before recreating it.";
    const keys = engine === "yugabyte" ? nativeKeys(pk, q) : pk.columns.map((c) => q(c.replace(/\s+(ASC|DESC)$/i, ""))).join(", ");
    lines.push(`  CONSTRAINT ${q(pk.name)} PRIMARY KEY (${keys})`);
  }
  outgoing.forEach(({ fk }) =>
    lines.push(
      `  CONSTRAINT ${q(fk.name)} FOREIGN KEY (${fk.columns.map(q).join(", ")}) REFERENCES ${q(fk.ref_schema)}.${q(fk.ref_table)} (${fk.ref_columns.map(q).join(", ")})`,
    ),
  );
  const native = engine === "yugabyte" ? table.storage?.docdb : null;
  const distribution = native ? ` WITH (colocation = ${native.colocated ? "true" : "false"})${!native.colocated && (native.hash_key_columns ?? 0) > 0 && (native.tablets ?? 0) > 1 ? ` SPLIT INTO ${native.tablets} TABLETS` : ""}` : "";
  const out = [original ?? `CREATE TABLE ${name} (\n${lines.join(",\n")}\n)${distribution};`];
  if (native) out.unshift("-- Logical YSQL schema with captured sharding. Review tablespaces, placement, tablegroups and range split points against a native schema export.");
  indexes
    .filter((i) => !i.is_primary_key && (engine !== "sqlite" || i.definition != null))
    .forEach((i) => {
      if (i.definition && /^CREATE /i.test(i.definition)) {
        out.push(`${i.definition.replace(/;?\s*$/, "")};`);
        return;
      }
      if (engine === "yugabyte") {
        out.push(`-- The native definition of ${q(i.name)} was unavailable; export it before recreating this index.`);
        return;
      }
      const keys = i.columns.map((c) => {
        const desc = /\s+DESC$/i.test(c);
        return `${q(c.replace(/\s+(ASC|DESC)$/i, ""))}${desc ? " DESC" : ""}`;
      });
      const unique = i.is_unique ? "UNIQUE " : "";
      const kind = engine === "mssql" ? (i.is_clustered ? "CLUSTERED " : "NONCLUSTERED ") : "";
      const include = i.include.length ? ` INCLUDE (${i.include.map(q).join(", ")})` : "";
      const where = i.filter ? ` WHERE ${i.filter}` : "";
      out.push(`CREATE ${unique}${kind}INDEX ${q(i.name)} ON ${name} (${keys.join(", ")})${include}${where};`);
    });
  return out.join("\n\n");
}

function nativeKeys(index: IndexStat, quote: (name: string) => string): string {
  const hashes = index.docdb?.hash_columns ?? [];
  const group = hashes.length === 0 ? [] : [hashes.length === 1 ? `${quote(hashes[0]!)} HASH` : `(${hashes.map(quote).join(", ")}) HASH`];
  return [...group, ...index.columns.filter((c) => !hashes.some((hash) => bare(c) === bare(hash))).map((c) => `${quote(c.replace(/\s+(ASC|DESC)$/i, ""))} ${/\s+DESC$/i.test(c) ? "DESC" : "ASC"}`)].join(", ");
}
