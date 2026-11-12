import { Activity, Check, Copy, History, Play, SquarePen } from "lucide-react";
import { useEffect, useState } from "react";
import type { Engine } from "../gen/Engine";
import type { Plan } from "../gen/Plan";
import type { QueryStat } from "../gen/QueryStat";
import { api, errorMessage, type ApiError } from "../lib/api";
import { canOpenQueryInEditor, prepareQueryForEditor } from "../lib/editor-sql";
import { evidencePresentation, serverName } from "../lib/engines";
import { t } from "../lib/i18n";
import { tx } from "../lib/message-parts";
import { currentReport, setState, useStore } from "../lib/store";
import { PlanSurface, type PlanLook } from "./PlanSurface";
import { copyAndTell } from "../lib/clipboard";
import { measurementOrigin } from "../lib/measurement";
import { tabListKeys } from "../lib/keys";

export type PlanSource = "plan" | "actual";

const TURN_ON = "ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS = ON;";


/** "Estimated" (what the optimizer planned) or "Last run" (what actually happened). */
export function PlanSourceToggle({ value, onChange }: { value: PlanSource; onChange: (v: PlanSource) => void }) {
  return (
    <span className="seg" role="tablist" aria-label={t("ui.actual_run.which_plan")} onKeyDown={tabListKeys}>
      <button type="button" role="tab" aria-selected={value === "plan"} tabIndex={value === "plan" ? 0 : -1} className={value === "plan" ? "on" : ""} onClick={() => onChange("plan")} title={t("ui.actual_run.estimated_hint")}>
        {t("ui.actual_run.estimated")}
      </button>
      <button type="button" role="tab" aria-selected={value === "actual"} tabIndex={value === "actual" ? 0 : -1} className={value === "actual" ? "on" : ""} onClick={() => onChange("actual")} title={t("ui.actual_run.last_run_hint")}>
        <History size={12} /> {t("ui.common.last_run")}
      </button>
    </span>
  );
}

/** Placeholders a recorded statement keeps instead of values; such text can't run as it is. */
function runnable(text: string): boolean {
  return !/(\$\d+|@P\d+|@p\d+|(^|[\s(,=<>])\?($|[\s),]))/.test(text);
}

type State =
  | { status: "loading" }
  | { status: "ready"; plan: Plan; measured: boolean }
  | { status: "missing"; error: ApiError }
  | { status: "measuring" };

/**
 * The actual run of a query: SQL Server's kept last actual plan when it has one, otherwise a calm explanation of why not
 * and the safe ways to get one (turn the setting on, measure once, or open it in the editor with real values).
 */
export function LastActualRun(props: { connection: string; engine: Engine; query: QueryStat; planId: string | null; reportId?: string; look: PlanLook }) {
  const { connection, engine, query, planId, reportId, look } = props;
  const product = useStore((s) => currentReport(s)?.snapshot.server.product);
  const [state, setRun] = useState<State>({ status: "loading" });
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setRun({ status: "loading" });
    api()
      .then((client) => client.lastActualPlan(connection, query.id, planId, reportId))
      .then((plan) => !cancelled && setRun({ status: "ready", plan, measured: false }))
      .catch((e) => !cancelled && setRun({ status: "missing", error: errorMessage(e) }));
    return () => {
      cancelled = true;
    };
  }, [connection, query.id, planId, reportId]);

  const measure = async () => {
    setRun({ status: "measuring" });
    try {
      const plan = await (await api()).explain(connection, query.text, "analyze", true);
      setRun({ status: "ready", plan, measured: true });
    } catch (e) {
      setRun({ status: "missing", error: errorMessage(e) });
    }
  };

  if (state.status === "loading" || state.status === "measuring") {
    return (
      <div className="plan-skeleton" aria-busy="true">
        <div className="sk-line" style={{ width: "38%" }} />
        <div className="sk-cards">
          <span />
          <span />
          <span />
        </div>
        <div className="muted">{state.status === "measuring" ? t("ui.actual_run.measuring") : t("ui.actual_run.loading")}</div>
      </div>
    );
  }
  if (state.status === "ready") {
    return (
      <>
        {state.measured && (
          <div className="actual-origin">
            <Activity size={12} /> {measurementOrigin(state.plan.facts.execution_protection)}
          </div>
        )}
        <PlanSurface key={`actual-${query.id}-${planId}`} plan={state.plan} look={look} />
      </>
    );
  }

  const settingOff = state.error.detail === "actual-plan-statistics-unavailable";
  const canRun = runnable(query.text);
  return (
    <div className="actual-missing">
      <div className="am-title">{settingOff ? t("ui.actual_run.setting_off") : state.error.message}</div>
      <p className="am-body">
        {settingOff ? (
          tx("ui.actual_run.setting_explain", {}, { setting: <span className="mono">LAST_QUERY_PLAN_STATS</span> })
        ) : evidencePresentation(engine).retained_actual_plans ? (
          state.error.hint
        ) : (
          <>
            {t("ui.actual_run.only_while_measured", { engine: serverName(engine, product) })} {canRun ? t("ui.actual_run.can_run") : t("ui.actual_run.placeholders")}
          </>
        )}
      </p>
      {settingOff && (
        <div className="am-code">
          <code>{TURN_ON}</code>
          <button type="button"
            className="btn small"
            onClick={() =>
              copyAndTell(TURN_ON).then((ok) => {
                if (!ok) return;
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              })
            }
          >
            {copied ? <Check size={12} /> : <Copy size={12} />} {copied ? t("ui.common.copied") : t("ui.common.copy")}
          </button>
        </div>
      )}
      <div className="am-actions">
        {canRun && (
          <button type="button" className="btn primary" onClick={measure} title={t("ui.actual_run.measure_hint")}>
            <Play size={13} /> {t("ui.actual_run.measure")}
          </button>
        )}
        {(() => {
          const editorSql = prepareQueryForEditor(query, engine);
          return (
            canOpenQueryInEditor(query.text, editorSql) && (
              <button type="button" className="btn" onClick={() => setState({ view: "editor", editorSql })}>
                <SquarePen size={13} /> {t("ui.common.open_in_editor")}
              </button>
            )
          );
        })()}
      </div>
    </div>
  );
}
