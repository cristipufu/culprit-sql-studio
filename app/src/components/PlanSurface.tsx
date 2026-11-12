import { GitBranch, ListTree, Network, TableProperties } from "lucide-react";
import { useMemo, useState } from "react";
import type { Plan } from "../gen/Plan";
import type { QueryStat } from "../gen/QueryStat";
import { planShares } from "../lib/actual";
import { evidencePresentation, can } from "../lib/engines";
import { allNodes, cost, findingNoShareReason, insightsByNode, noShareReason } from "../lib/plan-format";
import { NodeDetails } from "./NodeDetails";
import { DocdbWork } from "./DocdbWork";
import { PlanDiagram } from "./PlanDiagram";
import { PlanInsights } from "./PlanInsights";
import { PlanAssessmentNotes } from "./PlanAssessmentNotes";
import { PlanOperations } from "./PlanOperations";
import { PlanView, planKindLabel } from "./PlanView";
import { remember, rememberedChoice } from "../lib/prefs";
import { tabListKeys } from "../lib/keys";
import { t } from "../lib/i18n";

export type PlanLook = "diagram" | "tree" | "operations";

const LOOK_KEY = "culprit.plan.look";

export function rememberedLook(): PlanLook {
  return rememberedChoice<PlanLook>(LOOK_KEY, "diagram", ["diagram", "tree", "operations"]);
}

export function rememberLook(look: PlanLook): void {
  remember(LOOK_KEY, look);
}

const LOOKS: { look: PlanLook; label: () => string; icon: typeof Network; hint: () => string }[] = [
  { look: "diagram", label: () => t("ui.plan_surface.diagram"), icon: Network, hint: () => t("ui.plan_surface.diagram_hint") },
  { look: "tree", label: () => t("ui.plan_surface.tree"), icon: ListTree, hint: () => t("ui.plan_surface.tree_hint") },
  { look: "operations", label: () => t("ui.plan_surface.operations"), icon: TableProperties, hint: () => t("ui.plan_surface.operations_hint") },
];

/** Diagram · Tree · Operations. */
export function LookToggle({ look, onChange }: { look: PlanLook; onChange: (look: PlanLook) => void }) {
  return (
    <span className="seg look-toggle" role="tablist" aria-label={t("ui.plan_surface.label")} onKeyDown={tabListKeys}>
      {LOOKS.map((l) => {
        const Icon = l.icon;
        return (
          <button type="button" key={l.look} role="tab" aria-selected={look === l.look} tabIndex={look === l.look ? 0 : -1} className={look === l.look ? "on" : ""} title={l.hint()} onClick={() => onChange(l.look)}>
            <Icon size={12} />
            {l.label()}
          </button>
        );
      })}
    </span>
  );
}

/**
 * A plan with everything Culprit knows about it: the insights first (what's wrong, how to fix it), then the plan in the
 * chosen view. Picking an insight selects its operator in whichever view is showing, and back.
 */
export function PlanSurface({ plan, query, focus, look, badge }: { plan: Plan; query?: QueryStat | null; focus?: number | null; look: PlanLook; badge?: string }) {
  const [selected, setSelected] = useState<number | null>(null);
  return (
    <div className="plan-surface">
      {plan.facts.constants_redacted && <p className="notice">{t("ui.plan_surface.redacted")}</p>}
      {!plan.insights?.note && plan.kind !== "actual" && plan.kind === "generic" && (
        <PlanAssessmentNotes note={[
          t("ui.plan_surface.generic"),
          t(evidencePresentation(plan.engine).generic_plan_context),
          t("ui.plan_surface.generic_history"),
        ].join(" ")} />
      )}
      <PlanInsights plan={plan} selectedNode={selected} onSelectNode={setSelected} />
      <DocdbWork metrics={plan.facts.docdb} basis="execution" />
      {look === "diagram" && <PlanDiagram plan={plan} query={query} focus={focus} selectedId={selected} onSelect={setSelected} badge={badge} />}
      {look === "tree" && <PlanView plan={plan} query={query} focus={focus} selectedId={selected} onSelect={setSelected} />}
      {look === "operations" && <OperationsPanel plan={plan} query={query} focus={focus} selected={selected ?? focus ?? null} onSelect={setSelected} />}
    </div>
  );
}

function OperationsPanel({ plan, query, focus, selected, onSelect }: { plan: Plan; query?: QueryStat | null; focus?: number | null; selected: number | null; onSelect: (id: number) => void }) {
  // The same shares as the diagram and the tree: own time in a timed run, the estimated cost otherwise.
  const shares = useMemo(() => planShares(plan, query), [plan, query]);
  const nodes = useMemo(() => allNodes(plan.root), [plan]);
  const flagged = useMemo(() => insightsByNode(plan), [plan]);
  // The share a person reads: none where the plan can't state one.
  const shareOf = (id: number) => shares.stated.get(id) ?? null;
  // Open on the step where the work is (the core's pick, never an exchange), in a plan that does real work and where its
  // share can be stated; otherwise on the plan's root.
  const start = shares.hottest ?? plan.root;
  const current = nodes.find((n) => n.id === (selected ?? start.id)) ?? null;
  return (
    <div className="plan">
      <div className="plan-head">
        <span className="tag">{planKindLabel(plan)}</span>
        <span>
          {t("ui.common.plan_operators", { count: nodes.length })}
          {can(plan.engine, "plan_estimates") && ` · ${plan.facts.estimated_cost_unavailable ? t("ui.common.plan_cost_unavailable") : t("ui.common.plan_cost", { cost: cost(plan.total_cost || plan.root.subtree_cost) })}`}
        </span>
        <span className="spacer" />
        <GitBranch size={12} className="muted" />
        <span className="muted">{t("ui.plan_surface.click_row")}</span>
      </div>
      <PlanOperations plan={plan} query={query} focus={focus} selectedId={current?.id ?? null} onSelect={onSelect} />
      {current && (
        <NodeDetails
          plan={plan}
          estimates={can(plan.engine, "plan_estimates")}
          actual={plan.kind === "actual"}
          node={current}
          share={shareOf(current.id)}
          subtreeShare={shares.statedSubtree.get(current.id) ?? null}
          noShare={shares.basis === "cost" ? (current.id === focus ? findingNoShareReason(plan, current, query) : noShareReason(plan, current, query)) : null}
          insights={flagged.get(current.id)}
        />
      )}
    </div>
  );
}
