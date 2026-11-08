import { ArrowDown, ArrowUp, Search } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { Plan } from "../gen/Plan";
import type { PlanNode } from "../gen/PlanNode";
import type { QueryStat } from "../gen/QueryStat";
import { planShares } from "../lib/actual";
import { can } from "../lib/engines";
import { duration, percent } from "../lib/format";
import { t } from "../lib/i18n";
import { allNodes, estRowsForRuns, findingNoShareReason, insightsByNode, misestimate, noShareReason, operatorRuns, rows } from "../lib/plan-format";
import { OpIcon } from "./OpIcon";
import { shortPredicate } from "./PlanView";

type SortKey = "cost" | "subtree" | "est" | "act" | "error" | "runs" | "read" | "time" | "issues";

type Row = {
  node: PlanNode;
  depth: number;
  /** Shares the plan can state; null where it can't (see `stepShare`). */
  self: number | null;
  subtree: number | null;
  error: number;
  issues: number;
  worst: "high" | "medium" | "low" | null;
};

const COLUMNS: { key: SortKey; label: () => string; title: () => string; actual?: boolean }[] = [
  { key: "cost", label: () => t("ui.plan_operations.column.self"), title: () => t("ui.plan_operations.column.self_title") },
  { key: "subtree", label: () => t("ui.plan_operations.column.subtree"), title: () => t("ui.plan_operations.column.subtree_title") },
  { key: "est", label: () => t("ui.plan_operations.column.est"), title: () => t("ui.plan_operations.column.est_title") },
  { key: "act", label: () => t("ui.plan_operations.column.act"), title: () => t("ui.plan_operations.column.act_title"), actual: true },
  { key: "error", label: () => t("ui.plan_operations.column.error"), title: () => t("ui.plan_operations.column.error_title"), actual: true },
  { key: "runs", label: () => t("ui.plan_operations.column.runs"), title: () => t("ui.plan_operations.column.runs_title") },
  { key: "read", label: () => t("ui.plan_operations.column.read"), title: () => t("ui.plan_operations.column.read_title") },
  { key: "time", label: () => t("ui.plan_operations.column.time"), title: () => t("ui.plan_operations.column.time_title"), actual: true },
  { key: "issues", label: () => t("ui.plan_operations.column.issues"), title: () => t("ui.plan_operations.column.issues_title") },
];

/** Every operator in one sortable table: where the work goes, where estimates broke, which ones have problems. */
export function PlanOperations({ plan, query: statement, focus, selectedId, onSelect }: { plan: Plan; query?: QueryStat | null; focus?: number | null; selectedId: number | null; onSelect: (id: number) => void }) {
  const [sort, setSort] = useState<{ key: SortKey; desc: boolean }>({ key: "cost", desc: true });
  const [query, setQuery] = useState("");
  const [issuesOnly, setIssuesOnly] = useState(false);
  const body = useRef<HTMLTableSectionElement>(null);
  // The same shares as the diagram and the tree: own time in a timed run, the estimated cost otherwise.
  const shares = useMemo(() => planShares(plan, statement), [plan, statement]);
  const actual = allNodes(plan.root).some((n) => n.act_rows != null);
  const timed = allNodes(plan.root).some((n) => n.act_time_ms != null);
  const flagged = useMemo(() => insightsByNode(plan), [plan]);

  const all = useMemo(() => {
    const out: Row[] = [];
    const visit = (n: PlanNode, depth: number) => {
      const list = flagged.get(n.id) ?? [];
      const err = misestimate(plan, n);
      out.push({
        node: n,
        depth,
        self: shares.stated.get(n.id) ?? null,
        subtree: shares.statedSubtree.get(n.id) ?? null,
        error: err != null ? Math.abs(err) : 0,
        issues: list.length + n.warnings.length,
        worst: list[0]?.severity ?? (n.warnings.length ? "low" : null),
      });
      n.children.forEach((c) => visit(c, depth + 1));
    };
    visit(plan.root, 0);
    return out;
  }, [plan, shares, flagged]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    const value = (r: Row): number => {
      switch (sort.key) {
        case "cost":
          return r.self ?? -1;
        case "subtree":
          return r.subtree ?? -1;
        case "est":
          return estRowsForRuns(plan, r.node) ?? -1;
        case "act":
          return r.node.act_rows ?? -1;
        case "error":
          return r.error;
        case "runs":
          return operatorRuns(plan, r.node) ?? -1;
        case "read":
          return r.node.rows_read ?? -1;
        case "time":
          return r.node.act_time_ms ?? -1;
        case "issues":
          return r.issues * 10 + (r.worst === "high" ? 3 : r.worst === "medium" ? 2 : r.worst ? 1 : 0);
      }
    };
    return all
      .filter((r) => !issuesOnly || r.issues > 0)
      .filter((r) => !q || `${r.node.op} ${r.node.object ?? ""} ${r.node.index ?? ""} ${r.node.predicate ?? ""}`.toLowerCase().includes(q))
      .sort((a, b) => (sort.desc ? value(b) - value(a) : value(a) - value(b)));
  }, [all, plan, sort, query, issuesOnly]);

  useEffect(() => {
    body.current?.querySelector("tr.sel")?.scrollIntoView({ block: "nearest" });
  }, [selectedId]);

  // Columns that would be empty for every operator of this plan are left out, so the useful ones fit.
  const hasRead = all.some((r) => r.node.rows_read != null);
  const hasRuns = all.some((r) => (r.node.act_executions ?? r.node.est_executions) > 1);
  const hasIssues = all.some((r) => r.issues > 0);
  // A plan with no estimates (SQLite's) has no shares or estimated rows to show.
  const weighed = can(plan.engine, "plan_estimates") || actual;
  const columns = COLUMNS.filter(
    (c) =>
      (!c.actual || actual) &&
      (weighed || (c.key !== "cost" && c.key !== "subtree" && c.key !== "est")) &&
      (c.key !== "time" || timed) &&
      (c.key !== "read" || hasRead) &&
      (c.key !== "runs" || hasRuns) &&
      (c.key !== "issues" || hasIssues),
  );
  const move = (delta: number) => {
    const index = visible.findIndex((r) => r.node.id === selectedId);
    const next = visible[Math.min(visible.length - 1, Math.max(0, index + delta))];
    if (next) onSelect(next.node.id);
  };

  return (
    <div className="ops">
      <div className="ops-tools">
        <label className="search ops-search">
          <Search size={12} />
          <input value={query} placeholder={t("ui.plan_operations.filter")} onChange={(e) => setQuery(e.target.value)} />
        </label>
        <label className="ops-toggle">
          <input type="checkbox" checked={issuesOnly} onChange={(e) => setIssuesOnly(e.target.checked)} />
          {t("ui.plan_operations.issues_only")}
        </label>
        <span className="spacer" />
        <span className="muted">
          {t("ui.plan_operations.shown", { shown: visible.length, count: all.length })}
        </span>
      </div>
      <div
        className="ops-scroll"
        // biome-ignore lint/a11y/noNoninteractiveTabindex: arrow keys and j/k move the selected operation from here
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" || e.key === "j") {
            e.preventDefault();
            move(1);
          } else if (e.key === "ArrowUp" || e.key === "k") {
            e.preventDefault();
            move(-1);
          }
        }}
      >
        <table className="ops-table">
          <thead>
            <tr>
              <th className="ops-op">{t("ui.plan_operations.operator")}</th>
              {columns.map((c) => (
                <th
                  key={c.key}
                  className={`r sortable col-${c.key}`}
                  title={c.title()}
                  aria-sort={sort.key === c.key ? (sort.desc ? "descending" : "ascending") : "none"}
                  onClick={() => setSort((s) => (s.key === c.key ? { key: c.key, desc: !s.desc } : { key: c.key, desc: true }))}
                >
                  {c.key === "read" && plan.kind !== "actual" ? t("ui.plan_operations.column.est_read") : c.label()}
                  {sort.key === c.key && (sort.desc ? <ArrowDown size={11} /> : <ArrowUp size={11} />)}
                </th>
              ))}
            </tr>
          </thead>
          <tbody ref={body}>
            {visible.map((r) => {
              const n = r.node;
              const err = misestimate(plan, n);
              const runs = operatorRuns(plan, n);
              return (
                <tr key={n.id} className={`${n.id === selectedId ? "sel" : ""}`} onClick={() => onSelect(n.id)}>
                  <td className="ops-op">
                    <span className="ops-name">
                      <span className="ops-icon">
                        <OpIcon node={n} size={13} />
                      </span>
                      <span className="ops-text">
                        <b>{n.op}</b>
                        {(n.object || n.index) && (
                          <span className="mono muted">
                            {" "}
                            {n.object?.split(".").pop()}
                            {n.index ? ` · ${n.index}` : ""}
                          </span>
                        )}
                        {(n.seek_predicate || n.predicate) && <span className="ops-pred mono">{shortPredicate(n.seek_predicate ?? n.predicate ?? "")}</span>}
                      </span>
                    </span>
                  </td>
                  {columns.map((c) => (
                    <td key={c.key} className={`r num col-${c.key}`}>
                      {c.key === "cost" &&
                        (r.self != null ? (
                          <span className="ops-cost">
                            <span className="ops-cost-bar">
                              <span style={{ width: `${Math.min(100, r.self * 100)}%` }} />
                            </span>
                            {percent(r.self)}
                          </span>
                        ) : (
                          <span className="muted" title={shares.basis === "cost" ? (focus === n.id ? findingNoShareReason(plan, n, statement) : noShareReason(plan, n, statement)) ?? undefined : undefined}>
                            —
                          </span>
                        ))}
                      {c.key === "subtree" && (r.subtree != null ? percent(r.subtree) : <span className="muted">—</span>)}
                      {c.key === "est" && rows(estRowsForRuns(plan, n))}
                      {c.key === "act" && (n.act_rows != null ? rows(n.act_rows) : "—")}
                      {c.key === "error" && (err != null && Math.abs(err) >= 2 ? <span className={`ops-err ${n.warnings.includes("misestimate") ? "bad" : ""}`}>{err > 0 ? t("ui.plan_operations.more", { times: rows(Math.abs(err)) }) : t("ui.plan_operations.fewer", { times: rows(Math.abs(err)) })}</span> : <span className="muted">—</span>)}
                      {c.key === "runs" && (runs == null ? "—" : runs > 1 ? rows(runs) : <span className="muted">{rows(runs)}</span>)}
                      {c.key === "read" && (n.rows_read != null ? rows(n.rows_read) : <span className="muted">—</span>)}
                      {c.key === "time" && (n.act_time_ms != null ? duration(n.act_time_ms) : <span className="muted">—</span>)}
                      {c.key === "issues" && (r.issues > 0 ? <span className={`ops-issues ${r.worst ?? ""}`}>{r.issues}</span> : <span className="muted">—</span>)}
                    </td>
                  ))}
                </tr>
              );
            })}
          </tbody>
        </table>
        {visible.length === 0 && <div className="empty" style={{ padding: 24 }}>{t("ui.plan_operations.none")}</div>}
      </div>
    </div>
  );
}
