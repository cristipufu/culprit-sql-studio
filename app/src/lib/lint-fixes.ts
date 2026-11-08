// The editor's DDL quick fixes as text changes: which statement a finding's safe SQL replaces, or, for an unindexed
// foreign key, where its supporting CREATE INDEX goes. Pure, so the editor only dispatches what this returns.
import type { Finding } from "../gen/Finding";
import { locateStatementSpan } from "./editor-sql";
import { t } from "./i18n";

/** One edit of the document: replace `from`..`to` with `insert` (an insertion when they are equal). */
export type TextChange = { from: number; to: number; insert: string };

type Fixable = Pick<Finding, "id" | "facts" | "fix">;

/** A foreign key's fix adds an index before the statement instead of rewriting it. */
function addsIndex(f: Fixable): boolean {
  return f.id.startsWith("sql-lint:unindexed-fk:");
}

/** The change one finding's fix makes, and what to tell the person; null when it has no fix or no place in `doc`. */
export function lintFix(doc: string, f: Fixable): { change: TextChange; message: string } | null {
  const fixSql = f.fix?.sql?.trim();
  if (!fixSql) return null;
  const span = locateStatementSpan(doc, f);
  if (!span) return null;
  if (addsIndex(f)) {
    const at = span[0];
    return { change: { from: at, to: at, insert: `${fixSql}\n` }, message: t("ui.lint_fixes.added_index") };
  }
  return { change: { from: span[0], to: span[1], insert: fixSql }, message: t("ui.lint_fixes.applied") };
}

/**
 * Every finding's fix as one set of non-overlapping changes, in document order. A statement is rewritten once (the
 * first fix for it wins) and keeps every supporting index its foreign keys need in front of it, each added once.
 */
export function allLintFixes(doc: string, findings: readonly Fixable[]): { changes: TextChange[]; applied: number } {
  const byStart = new Map<number, { from: number; to: number; prefixes: string[]; replacement: string | null }>();
  let applied = 0;
  for (const f of findings) {
    const fixSql = f.fix?.sql?.trim();
    if (!fixSql) continue;
    const span = locateStatementSpan(doc, f);
    if (!span) continue;
    if (addsIndex(f)) {
      const from = span[0];
      const to = span[1];
      const entry = byStart.get(from) ?? { from, to, prefixes: [], replacement: null };
      if (!entry.prefixes.includes(fixSql)) {
        entry.prefixes.push(fixSql);
        applied += 1;
      }
      byStart.set(from, entry);
    } else {
      const from = span[0];
      const to = span[1];
      const entry = byStart.get(from) ?? { from, to, prefixes: [], replacement: null };
      entry.to = Math.max(entry.to, to);
      if (entry.replacement === null) {
        entry.replacement = fixSql;
        applied += 1;
      }
      byStart.set(from, entry);
    }
  }
  const changes = [...byStart.values()]
    .sort((a, b) => a.from - b.from)
    .map((entry) => {
      const prefix = entry.prefixes.length > 0 ? `${entry.prefixes.join("\n")}\n` : "";
      const body = entry.replacement ?? doc.slice(entry.from, entry.to);
      return { from: entry.from, to: entry.to, insert: `${prefix}${body}` };
    });
  return { changes: applied === 0 ? [] : changes, applied };
}
