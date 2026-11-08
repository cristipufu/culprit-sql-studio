import { ChartColumn, ChevronDown, CircleCheck, Copy, Cpu, Crosshair, Info, KeyRound, ListChecks, MemoryStick, Server, SquareCode, type LucideIcon } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { InsightArea } from "../gen/InsightArea";
import type { Plan } from "../gen/Plan";
import type { PlanInsight } from "../gen/PlanInsight";
import type { Severity } from "../gen/Severity";

import { percent } from "../lib/format";
import { bySeverity, statedInsightShare } from "../lib/plan-format";

import { Prose } from "./Prose";
import { Sql } from "./Sql";
import { copyAndTell } from "../lib/clipboard";
import { can } from "../lib/engines";
import { t } from "../lib/i18n";

const AREA: Record<InsightArea, { icon: LucideIcon; label: () => string }> = {
  index: { icon: KeyRound, label: () => t("ui.plan_insights.area.index") },
  query: { icon: SquareCode, label: () => t("ui.plan_insights.area.query") },
  statistics: { icon: ChartColumn, label: () => t("ui.plan_insights.area.statistics") },
  memory: { icon: MemoryStick, label: () => t("ui.plan_insights.area.memory") },
  parallelism: { icon: Cpu, label: () => t("ui.plan_insights.area.parallelism") },
  server: { icon: Server, label: () => t("ui.plan_insights.area.server") },
};

const SEVERITY_LABEL: Record<Severity, () => string> = {
  high: () => t("ui.plan_insights.severity.high"),
  medium: () => t("ui.plan_insights.severity.medium"),
  low: () => t("ui.plan_insights.severity.low"),
};

/** "2 high", in the panel's head. */
const SEVERITY_COUNT: Record<Severity, (count: number) => string> = {
  high: (count) => t("ui.plan_insights.count.high", { n: count, count: String(count) }),
  medium: (count) => t("ui.plan_insights.count.medium", { n: count, count: String(count) }),
  low: (count) => t("ui.plan_insights.count.low", { n: count, count: String(count) }),
};

/** The share of the plan's work an insight states: none on a plan without costs (SQLite's has the access path only), a
 * cheap plan, a step whose cost rests on a guess, or for a rule's own weight (`statedInsightShare`). */
const shareOf = (plan: Plan, i: PlanInsight): number | null =>
  can(plan.engine, "plan_estimates") || plan.kind === "actual" ? statedInsightShare(plan, i) : null;
const SHOWN = 4;

/** One insight as Markdown, the way it reads in the panel: for a pull request, an issue or a coding agent. */
const insightMarkdown = (i: PlanInsight, share: number | null) => {
  const severity = SEVERITY_LABEL[i.severity]();
  const area = AREA[i.area].label();
  return [
    share != null
      ? t("ui.plan_insights.md.heading_share", { severity, area, title: i.title, share: percent(share) })
      : t("ui.plan_insights.md.heading", { severity, area, title: i.title }),
    "",
    i.detail,
    "",
    t("ui.plan_insights.md.fix", { fix: i.fix }),
    ...(i.fix_sql ? ["", "```sql", i.fix_sql, "```"] : []),
    "",
    t("ui.plan_insights.md.verify", { verify: i.verify }),
  ].join("\n");
};

/** Every insight, most important first, numbered as the panel lists them, after the statement they're about. */
const insightsMarkdown = (plan: Plan, items: PlanInsight[]) =>
  [
    t("ui.plan_insights.md.title"),
    ...(plan.statement ? [t("ui.plan_insights.md.statement"), ["```sql", plan.statement.trim(), "```"].join("\n")] : []),
    ...items.map((i, n) => `### ${n + 1}. ${insightMarkdown(i, shareOf(plan, i))}`),
  ].join("\n\n");

/**
 * What's wrong with this plan, most important first: one sentence each, the share of the plan's work it touches, and
 * (expanded) why, the fix with SQL to copy, and how to check it worked. Selecting one selects its operator.
 */
export function PlanInsights({ plan, selectedNode, onSelectNode }: { plan: Plan; selectedNode?: number | null; onSelectNode: (id: number) => void }) {
  const items = useMemo(() => [...(plan.insights?.items ?? [])].sort(bySeverity), [plan]);
  const note = plan.insights?.note ?? null;
  const [open, setOpen] = useState<number | null>(null);
  const [all, setAll] = useState(false);
  const list = useRef<HTMLDivElement>(null);
  const counts = useMemo(() => {
    const c: Record<Severity, number> = { high: 0, medium: 0, low: 0 };
    items.forEach((i) => (c[i.severity] += 1));
    return c;
  }, [items]);

  // Selecting an operator in the diagram highlights the insights about it.
  const related = (i: PlanInsight) => selectedNode != null && i.nodes.includes(selectedNode);
  useEffect(() => {
    if (selectedNode == null) return;
    const index = items.findIndex((i) => i.nodes.includes(selectedNode));
    if (index >= SHOWN) setAll(true);
  }, [selectedNode, items]);

  if (!plan.insights) return null;

  if (items.length === 0) {
    return (
      <div className="insights empty-insights">
        <CircleCheck size={16} className="ok" />
        <div>
          <div className="ins-empty-title">{t("ui.plan_insights.none")}</div>
          {note && <div className="ins-note">{note}</div>}
        </div>
      </div>
    );
  }

  const shown = all ? items : items.slice(0, SHOWN);
  const focusRow = (index: number) => list.current?.querySelectorAll<HTMLButtonElement>(".ins-row")[index]?.focus();

  return (
    <section className="insights" aria-label={t("ui.plan_insights.label")}>
      <header className="ins-head">
        <ListChecks size={14} />
        <span className="ins-label">{t("ui.plan_insights.title")}</span>
        <span className="ins-counts">
          {(["high", "medium", "low"] as Severity[])
            .filter((s) => counts[s] > 0)
            .map((s) => (
              <span key={s} className={`ins-count ${s}`}>
                <i />
                {SEVERITY_COUNT[s](counts[s])}
              </span>
            ))}
        </span>
        <span className="spacer" />
        <button type="button"
          className="btn ghost small ins-copy-all"
          onClick={() => copyAndTell(insightsMarkdown(plan, items), t("ui.plan_insights.copied_all", { count: items.length }))}
          title={t("ui.plan_insights.copy_all_title")}
        >
          <Copy size={12} /> {t("ui.common.copy")}
        </button>
      </header>
      {note && <p className="ins-note"><Info size={13} /> {note}</p>}
      <div className="ins-list" ref={list} role="list" aria-label={t("ui.plan_insights.list_label")}>
        {shown.map((i, index) => {
          const area = AREA[i.area];
          const Icon = area.icon;
          const expanded = open === index;
          return (
            <div key={`${i.rule}-${i.nodes[0] ?? index}`} role="listitem" className={`ins ${i.severity} ${expanded ? "open" : ""} ${related(i) ? "related" : ""}`}>
              <button type="button"
                className="ins-row"
                aria-current={related(i) || undefined}
                aria-expanded={expanded}
                onClick={() => {
                  setOpen(expanded ? null : index);
                  if (i.nodes[0] != null) onSelectNode(i.nodes[0]);
                }}
                onKeyDown={(e) => {
                  if (e.key === "ArrowDown") {
                    e.preventDefault();
                    focusRow(Math.min(shown.length - 1, index + 1));
                  } else if (e.key === "ArrowUp") {
                    e.preventDefault();
                    focusRow(Math.max(0, index - 1));
                  } else if (e.key === "Escape" && expanded) {
                    e.preventDefault();
                    setOpen(null);
                  }
                }}
              >
                <span className="ins-sev" title={t("ui.plan_insights.severity_title", { severity: i.severity })} />
                <span className="ins-area" title={area.label()}>
                  <Icon size={13} />
                </span>
                <span className="ins-title">{i.title}</span>
                {shareOf(plan, i) != null && (
                  <span className="ins-impact" title={t("ui.plan_insights.share_title")}>
                    <span className="ins-bar">
                      <span style={{ width: `${Math.max(4, Math.min(100, (shareOf(plan, i) ?? 0) * 100))}%` }} />
                    </span>
                    {percent(shareOf(plan, i))}
                  </span>
                )}
                <ChevronDown size={14} className="ins-chev" />
              </button>
              {expanded && (
                <div className="ins-body">
                  <p className="ins-detail">
                    <Prose text={i.detail} />
                  </p>
                  <div className="ins-block">
                    <div className="ins-block-label">{t("ui.plan_insights.fix")}</div>
                    <p>
                      <Prose text={i.fix} />
                    </p>
                    {i.fix_sql && (
                      <div className="ins-sql">
                        <Sql text={i.fix_sql} />
                        <button type="button" className="btn ghost small ins-copy" onClick={() => copyAndTell(i.fix_sql!, t("ui.plan_insights.fix_copied"))} title={t("ui.plan_insights.copy_fix_title")}>
                          <Copy size={12} /> {t("ui.common.copy")}
                        </button>
                      </div>
                    )}
                  </div>
                  <div className="ins-block">
                    <div className="ins-block-label">{t("ui.plan_insights.verify")}</div>
                    <p>{i.verify}</p>
                  </div>
                  <div className="ins-actions">
                    {i.nodes[0] != null && (
                      <button type="button" className="btn ghost small" onClick={() => onSelectNode(i.nodes[0]!)}>
                        <Crosshair size={12} /> {t("ui.plan_insights.show_operator")}
                      </button>
                    )}
                    <button type="button" className="btn ghost small" onClick={() => copyAndTell(insightMarkdown(i, shareOf(plan, i)), t("ui.plan_insights.copied_one"))} title={t("ui.plan_insights.copy_one_title")}>
                      <Copy size={12} /> {t("ui.plan_insights.copy_one")}
                    </button>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
      {items.length > SHOWN && (
        <button type="button" className="ins-more" onClick={() => setAll((v) => !v)}>
          {all ? t("ui.plan_insights.show_fewer") : t("ui.plan_insights.show_more", { count: String(items.length - SHOWN) })}
        </button>
      )}
    </section>
  );
}
