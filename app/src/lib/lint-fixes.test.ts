import { expect, test } from "bun:test";
import type { Finding } from "../gen/Finding";
import { partial } from "../test/fixtures";
import { allLintFixes, lintFix, type TextChange } from "./lint-fixes";

const doc = `ALTER TABLE public.orders
  ADD CONSTRAINT fk_customer FOREIGN KEY (customer_id) REFERENCES customers(id);
CREATE INDEX idx_lines_order ON public.lines (order_id);`;

const blockingIndex = partial<Finding>({
  id: "sql-lint:index-without-concurrently:public.lines",
  summary: "The statement `CREATE INDEX idx_lines_order ON public.lines (order_id)` builds an index without `CONCURRENTLY`.",
  subject: { kind: "table", name: "public.lines" },
  facts: { statement: { sql: doc.slice(doc.indexOf("CREATE INDEX")), start: doc.indexOf("CREATE INDEX"), end: doc.length, line: 3 } },
  fix: { sql: "CREATE INDEX CONCURRENTLY idx_lines_order ON public.lines (order_id);" },
});
const unindexedFk = partial<Finding>({
  id: "sql-lint:unindexed-fk:public.orders.customer_id",
  summary: "The foreign key on orders has no index on its columns.",
  subject: { kind: "table", name: "public.orders" },
  facts: { statement: { sql: doc.slice(0, doc.indexOf(";") + 1), start: 0, end: doc.indexOf(";") + 1, line: 1 } },
  fix: { sql: "CREATE INDEX CONCURRENTLY idx_orders_customer ON public.orders (customer_id);" },
});

function apply(text: string, changes: TextChange[]): string {
  return [...changes].sort((a, b) => b.from - a.from).reduce((t, c) => t.slice(0, c.from) + c.insert + t.slice(c.to), text);
}

test("a quick fix rewrites its whole statement, or adds the supporting index before it", () => {
  const rewrite = lintFix(doc, blockingIndex)!;
  expect(rewrite.message).toBe("Applied safe SQL");
  expect(apply(doc, [rewrite.change]).endsWith("CREATE INDEX CONCURRENTLY idx_lines_order ON public.lines (order_id);")).toBe(true);

  const index = lintFix(doc, unindexedFk)!;
  expect(index.message).toBe("Added supporting CREATE INDEX");
  expect(index.change).toEqual({ from: 0, to: 0, insert: `${unindexedFk.fix!.sql}\n` });

  expect(lintFix(doc, { ...blockingIndex, fix: null })).toBeNull();
  expect(lintFix(doc, { ...blockingIndex, fix: { ...blockingIndex.fix!, sql: "  " } })).toBeNull();
  expect(lintFix("-- changed\n" + doc, unindexedFk)).toBeNull();
  expect(lintFix(doc, partial<Finding>({ ...unindexedFk, facts: {} }))).toBeNull();
});

test("applying every fix edits each statement once, adds each index once, and keeps the rest", () => {
  const { changes, applied } = allLintFixes(doc, [unindexedFk, blockingIndex, unindexedFk, { ...blockingIndex, id: "other", fix: null }]);
  expect(applied).toBe(2);
  expect(apply(doc, changes)).toBe(`${unindexedFk.fix!.sql}
ALTER TABLE public.orders
  ADD CONSTRAINT fk_customer FOREIGN KEY (customer_id) REFERENCES customers(id);
${blockingIndex.fix!.sql}`);
  for (let i = 1; i < changes.length; i++) expect(changes[i]!.from).toBeGreaterThanOrEqual(changes[i - 1]!.to);

  expect(allLintFixes(doc, [{ ...blockingIndex, fix: null }])).toEqual({ changes: [], applied: 0 });
  expect(allLintFixes("-- changed\n" + doc, [blockingIndex, unindexedFk])).toEqual({ changes: [], applied: 0 });
});

test("an old finding cannot replace SQL extended after the lint check", () => {
  const original = "CREATE INDEX idx_customer ON public.orders (customer_id)";
  const finding = partial<Finding>({
    id: "sql-lint:index-without-concurrently:public.orders",
    facts: { statement: { sql: original, start: 0, end: original.length, line: 1 } },
    fix: { sql: "CREATE INDEX CONCURRENTLY idx_customer ON public.orders (customer_id);" },
  });
  for (const extension of [" WHERE archived = false;", " INCLUDE (status);", "\nWHERE archived = false"]) {
    const edited = original + extension;
    expect(lintFix(edited, finding)).toBeNull();
    expect(allLintFixes(edited, [finding])).toEqual({ changes: [], applied: 0 });
  }
  for (const suffix of ["", ";", " \n; "]) {
    const current = original + suffix;
    const fix = lintFix(current, finding);
    expect(fix).not.toBeNull();
    expect(apply(current, [fix!.change]).trim()).toBe(finding.fix!.sql!);
  }
});

test("quick fixes retain prior SQL and inline comments on the same line", () => {
  for (const prefix of ["SET LOCAL lock_timeout = '1s'; ", "/* retain this comment */ "]) {
    const current = prefix + doc;
    const findings = [unindexedFk, blockingIndex].map((finding) => {
      const facts = finding.facts!;
      const statement = facts.statement!;
      return { ...finding, facts: { ...facts, statement: { ...statement, start: statement.start + prefix.length, end: statement.end + prefix.length } } };
    });
    const insertion = lintFix(current, findings[0]!)!;
    expect(insertion.change.from).toBe(prefix.length);
    const result = allLintFixes(current, findings);
    expect(result.applied).toBe(2);
    expect(apply(current, result.changes)).toBe(`${prefix}${unindexedFk.fix!.sql}\n${doc.slice(0, doc.indexOf(";") + 1)}\n${blockingIndex.fix!.sql}`);
  }
});
