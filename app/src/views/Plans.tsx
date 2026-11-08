import { Braces, ClipboardCopy, ClipboardPaste, Copy, FileCode, FileDown, FileUp, GitCompareArrows, Trash2, TriangleAlert, X } from "lucide-react";
import { useRef, useState, type DragEvent } from "react";
import { ShareMenu } from "../components/ShareMenu";
import { PlanCatalog } from "../components/PlanCatalog";
import { PlanCompare } from "./PlanCompare";
import { LookToggle as PlanLookToggle, PlanSurface, rememberLook, rememberedLook as rememberedPlanLook, type PlanLook } from "../components/PlanSurface";
import { Sql } from "../components/Sql";
import type { Plan } from "../gen/Plan";
import { api, type ApiError } from "../lib/api";
import { useHotkeys } from "../lib/hotkeys";
import { t } from "../lib/i18n";
import {
  dismissError,
  exportable,
  importFile,
  importText,
  removePlan,
  savePlan,
  selectPlan,
  setMode,
  setSide,
  swapSides,
  trySample,
  usePlans,
  type LoadedPlan,
} from "../lib/plans";
import { deliver, exportOptions } from "../lib/share";
import { toast } from "../lib/store";
import { planEngineName } from "../lib/engines";
import { keys } from "../lib/keys";
import { copyAndTell } from "../lib/clipboard";

function cost(value: number): string {
  if (value < 10) return value.toFixed(3);
  if (value < 100) return value.toFixed(1);
  if (value >= 1e6) return `${(value / 1e6).toFixed(1)}M`;
  return value >= 1e4 ? `${Math.round(value / 1e3)}K` : value.toFixed(0);
}

function nodeCount(plan: Plan): number {
  const walk = (n: Plan["root"]): number => 1 + n.children.reduce((s, c) => s + walk(c), 0);
  return walk(plan.root);
}

export function Plans() {
  const items = usePlans((s) => s.items);
  const mode = usePlans((s) => s.mode);
  const selected = usePlans((s) => s.selected);
  const busy = usePlans((s) => s.busy);
  const error = usePlans((s) => s.error);
  const fileInput = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);
  const current = items.find((p) => p.id === selected) ?? items[items.length - 1] ?? null;

  const openFile = () => fileInput.current?.click();
  const pasteFromClipboard = async () => {
    try {
      const text = await navigator.clipboard.readText();
      if (text.trim()) await importText(text, t("ui.plans.pasted_name", { number: String(items.length + 1) }), t("ui.plans.pasted_origin"));
      else toast(t("ui.plans.clipboard_empty"));
    } catch {
      toast(t("ui.plans.press_to_paste", { key: keys("⌘V") }));
    }
  };
  useHotkeys({ "mod+o": openFile });

  const onDrop = async (e: DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const files = [...e.dataTransfer.files];
    if (files.length) {
      for (const file of files.slice(0, 8)) await importFile(file);
      return;
    }
    const text = e.dataTransfer.getData("text/plain");
    if (text.trim()) await importText(text, t("ui.plans.dropped_name", { number: String(items.length + 1) }), t("ui.plans.dropped_origin"));
  };

  return (
    <div
      className={`plans ${dragging ? "dragging" : ""}`}
      onDragOver={(e) => {
        e.preventDefault();
        if (!dragging) setDragging(true);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target || !e.currentTarget.contains(e.relatedTarget as Node)) setDragging(false);
      }}
      onDrop={onDrop}
    >
      <input
        ref={fileInput}
        type="file"
        accept=".sqlplan,.json,.txt,.xml"
        multiple
        hidden
        onChange={async (e) => {
          const files = [...(e.target.files ?? [])];
          e.target.value = "";
          for (const file of files) await importFile(file);
        }}
      />
      {items.length === 0 ? (
        <Welcome busy={busy} error={error} onOpen={openFile} onPaste={pasteFromClipboard} />
      ) : (
        <>
          <aside className="plans-list">
            <div className="plans-actions">
              <button type="button" className="btn" onClick={openFile} title={t("ui.plans.open_title", { key: keys("⌘O") })}>
                <FileUp size={13} /> {t("ui.plans.open")}
              </button>
              <button type="button" className="btn" onClick={pasteFromClipboard} title={t("ui.plans.paste_title", { key: keys("⌘V") })}>
                <ClipboardPaste size={13} /> {t("ui.plans.paste")}
              </button>
            </div>
            <div className="plans-mode seg">
              <button type="button" className={mode === "single" ? "on" : ""} onClick={() => setMode("single")}>
                {t("ui.plans.one_plan")}
              </button>
              <button type="button" className={mode === "compare" ? "on" : ""} disabled={items.length < 2} title={items.length < 2 ? t("ui.plans.compare_needs_two") : t("ui.plans.compare_title")} onClick={() => setMode("compare")}>
                <GitCompareArrows size={12} /> {t("ui.common.compare")}
              </button>
            </div>
            {mode === "compare" && <div className="plans-tip">{t("ui.plans.baseline_tip")}</div>}
            {error && <ErrorNote error={error} onDismiss={dismissError} />}
            {busy && (
              <div className="plans-busy">
                <span className="spinner" style={{ width: 12, height: 12 }} /> {t("ui.plans.reading")}
              </div>
            )}
            <div className="plans-items">
              {items.map((item) => (
                <PlanRow key={item.id} item={item} active={mode === "single" && item.id === current?.id} />
              ))}
            </div>
            <div className="plans-hint">{t("ui.plans.drop_hint")}</div>
          </aside>
          <section className="plans-main">{mode === "compare" ? <PlanCompare /> : current && <Single item={current} />}</section>
        </>
      )}
      {dragging && <div className="drop-veil">{t("ui.plans.drop")}</div>}
    </div>
  );
}

function Welcome({ busy, error, onOpen, onPaste }: { busy: boolean; error: ApiError | null; onOpen: () => void; onPaste: () => void }) {
  return (
    <div className="plans-welcome">
      <div className="welcome-card">
        <GitCompareArrows size={26} className="welcome-icon" />
        <h2>{t("ui.plans.welcome.heading")}</h2>
        <p className="muted">{t("ui.plans.welcome.detail")}</p>
        <div className="welcome-actions">
          <button type="button" className="btn primary" onClick={onOpen}>
            <FileUp size={14} /> {t("ui.plans.welcome.open")}
          </button>
          <button type="button" className="btn" onClick={onPaste}>
            <ClipboardPaste size={14} /> {t("ui.plans.paste")} <span className="kbd">{keys("⌘V")}</span>
          </button>
        </div>
        {busy && (
          <div className="plans-busy">
            <span className="spinner" style={{ width: 12, height: 12 }} /> {t("ui.plans.reading")}
          </div>
        )}
        {error && <ErrorNote error={error} onDismiss={dismissError} />}
        <PlanCatalog onSample={(product) => void trySample(product)} />
        <p className="muted small">{t("ui.plans.welcome.from_queries")}</p>
      </div>
    </div>
  );
}

function ErrorNote({ error, onDismiss }: { error: ApiError; onDismiss?: () => void }) {
  return (
    <div className="notice error plans-error">
      <TriangleAlert size={14} style={{ flex: "none", marginTop: 2 }} />
      <div style={{ flex: 1 }}>
        <b>{error.message}</b>
        {error.hint && <div className="muted">{error.hint}</div>}
      </div>
      {onDismiss && (
        <button type="button" className="btn icon ghost" title={t("ui.common.dismiss")} onClick={onDismiss} style={{ width: 20, height: 20 }}>
          <X size={12} />
        </button>
      )}
    </div>
  );
}

function PlanRow({ item, active }: { item: LoadedPlan; active: boolean }) {
  const mode = usePlans((s) => s.mode);
  const a = usePlans((s) => s.a);
  const b = usePlans((s) => s.b);
  const side = item.id === a ? "A" : item.id === b ? "B" : null;
  return (
    <div className={`plan-row ${active ? "on" : ""} ${mode === "compare" && side ? "in-compare" : ""}`}>
      <button type="button"
        className="plan-row-main"
        title={mode === "compare" ? (side === "A" ? t("ui.common.swap_sides") : t("ui.plans.compare_as_b")) : undefined}
        onClick={() => (mode !== "compare" ? selectPlan(item.id) : side === "A" ? swapSides() : setSide("b", item.id))}
      >
        <span className="plan-row-name">
          {mode === "compare" && side && <span className="side-badge">{side}</span>}
          {item.name}
        </span>
        <span className="plan-row-meta">
          {item.plan.facts.estimated_cost_unavailable
            ? t("ui.plans.row_meta_no_cost", { engine: planEngineName(item.plan), kind: item.plan.kind })
            : t("ui.plans.row_meta", { engine: planEngineName(item.plan), kind: item.plan.kind, cost: cost(item.plan.total_cost) })}
        </span>
        {item.origin !== item.name && <span className="plan-row-origin">{item.origin}</span>}
      </button>
      <button type="button" className="btn icon ghost plan-row-remove" title={t("ui.plans.close")} onClick={() => removePlan(item.id)}>
        <X size={13} />
      </button>
    </div>
  );
}

function Single({ item }: { item: LoadedPlan }) {
  const [look, setPlanLook] = useState<PlanLook>(rememberedPlanLook);
  const setLook = (next: PlanLook) => {
    setPlanLook(next);
    rememberLook(next);
  };
  const [showSql, setShowSql] = useState(false);
  const file = exportable(item);
  const plan = item.plan;
  return (
    <div className="plans-single">
      <div className="plans-head">
        <div style={{ minWidth: 0 }}>
          <h2 title={item.name}>{item.name}</h2>
          <div className="muted plans-sub">
            {t("ui.plans.sub", { engine: planEngineName(plan), kind: plan.kind, n: nodeCount(plan), count: String(nodeCount(plan)) })}
            {item.origin !== item.name && ` · ${item.origin}`}
          </div>
        </div>
        <span className="spacer" />
        <PlanLookToggle look={look} onChange={setLook} />
        <ShareMenu
          actions={[
            {
              id: "md",
              icon: <ClipboardCopy size={15} />,
              label: t("ui.common.share.copy_pr"),
              hint: t("ui.plans.share.copy_pr_hint"),
              redacts: true,
              run: (redact) => deliver(api().then((c) => c.exportPlan(plan, item.name, "markdown", exportOptions(redact))), "copy", redact),
            },
            {
              id: "html",
              icon: <FileCode size={15} />,
              label: t("ui.common.share.save_html"),
              hint: t("ui.plans.share.save_html_hint"),
              redacts: true,
              run: (redact) => deliver(api().then((c) => c.exportPlan(plan, item.name, "html", exportOptions(redact))), "save", redact),
            },
            {
              id: "native",
              icon: <FileDown size={15} />,
              label: plan.engine === "mssql" ? t("ui.plans.share.save_sqlplan") : t("ui.plans.share.save_native"),
              hint: plan.engine === "mssql" ? t("ui.plans.share.save_sqlplan_hint") : t("ui.plans.share.save_native_hint"),
              redacts: true,
              unavailable: file.native || item.source ? undefined : t("ui.plans.share.no_native"),
              run: (redact) =>
                deliver(
                  api().then((c) =>
                    file.native
                      ? c.exportPlan(plan, item.name, "native", exportOptions(redact))
                      : c.exportQueryPlan(item.source!.connection, item.source!.queryId, item.source!.planId, "native", exportOptions(redact)),
                  ),
                  "save",
                  redact,
                ),
            },
            {
              id: "json",
              icon: <Braces size={15} />,
              label: t("ui.plans.share.save_json"),
              hint: t("ui.plans.share.save_json_hint"),
              run: () => savePlan(item, true),
            },
          ]}
        />
        <button type="button" className="btn icon ghost" title={t("ui.plans.copy_text")} aria-label={t("ui.plans.copy_text")} onClick={() => copyAndTell(file.contents, t("ui.plans.copied"))}>
          <Copy size={13} />
        </button>
        <button type="button" className="btn icon ghost" title={t("ui.plans.close")} onClick={() => removePlan(item.id)}>
          <Trash2 size={13} />
        </button>
      </div>
      {plan.statement && (
        <div
          className={`plans-sql ${showSql ? "open" : ""}`}
          role="button"
          tabIndex={0}
          aria-expanded={showSql}
          onClick={() => setShowSql((v) => !v)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              setShowSql((v) => !v);
            }
          }}
          title={showSql ? t("ui.plans.collapse") : t("ui.plans.expand")}
        >
          <Sql text={plan.statement} />
        </div>
      )}
      <PlanSurface key={item.id} plan={plan} query={item.query} look={look} />
    </div>
  );
}
