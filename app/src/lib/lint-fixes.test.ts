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
  fix: { sql: "CREATE INDEX CONCURRENTLY idx_lines_order ON public.lines (order_id);" },
});
const unindexedFk = partial<Finding>({
  id: "sql-lint:unindexed-fk:public.orders.customer_id",
  summary: "The foreign key on orders has no index on its columns.",
  subject: { kind: "table", name: "public.orders" },
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
});
