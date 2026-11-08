import { Check, Flame, Pause, Play, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { NodeProgress } from "../gen/NodeProgress";
import type { Plan } from "../gen/Plan";
import type { PlanNode } from "../gen/PlanNode";
import type { QueryProgress } from "../gen/QueryProgress";
import { api, errorMessage, type ApiError } from "../lib/api";
import { count, duration, percent, rowCount } from "../lib/format";
import { t } from "../lib/i18n";
import { PlanDiagram } from "./PlanDiagram";
import { slot, withElements } from "../views/message-parts";

const REFRESH_MS = 1000;

/** Plain-language reading of the waits a running statement most often shows. */
const WAIT_WORDS: [RegExp, () => string][] = [
  [/^CXPACKET|^CXCONSUMER|^CXSYNC/i, () => t("ui.query_progress.wait.parallel")],
  [/^PAGEIOLATCH|^IO:|DataFileRead/i, () => t("ui.query_progress.wait.disk")],
  [/^LCK_|^Lock:|lock/i, () => t("ui.query_progress.wait.lock")],
  [/^ASYNC_NETWORK_IO|^Client:/i, () => t("ui.query_progress.wait.client")],
  [/^RESOURCE_SEMAPHORE/i, () => t("ui.query_progress.wait.memory")],
  [/^SOS_SCHEDULER_YIELD/i, () => t("ui.query_progress.wait.cpu")],
  [/^WRITELOG|WALWrite/i, () => t("ui.query_progress.wait.log")],
];

function waitWords(wait: string): string | null {
  return WAIT_WORDS.find(([re]) => re.test(wait))?.[1]() ?? null;
}

function nodesOf(n: PlanNode, out: PlanNode[] = []): PlanNode[] {
  out.push(n);
  n.children.forEach((c) => nodesOf(c, out));
  return out;
}

function fraction(p: NodeProgress): number | null {
  if (p.state === "done") return 1;
  if (p.state === "pending") return 0;
  return p.est_rows >= 1 ? Math.min(p.rows / p.est_rows, 0.99) : null;
}

/** Drawn over a plan card: how far the operator is, done ticks, dimmed waiting steps, a ring on the busiest one. */
function ProgressOverlay({ p, hot, box }: { p: NodeProgress | undefined; hot: boolean; box: { x: number; y: number; w: number; h: number } }) {
  if (!p) return null;
  const f = fraction(p);
  const over = p.est_rows >= 1 && p.rows > p.est_rows * 1.5;
  return (
    <span className={`po po-${p.state} ${hot ? "po-hot" : ""} ${over ? "po-over" : ""}`} style={{ left: box.x, top: box.y, width: box.w, height: box.h }} aria-hidden>
      <span className="po-badge">
        {p.state === "done" ? (
          <>
            <Check size={11} /> {t("ui.query_progress.step_done")}
          </>
        ) : p.state === "pending" ? (
          t("ui.query_progress.step_waiting")
        ) : over ? (
          t("ui.query_progress.step_over", { times: count(p.rows / p.est_rows) })
        ) : p.rows === 0 ? (
          t("ui.query_progress.step_working")
        ) : f != null ? (
          percent(f)
        ) : (
          rowCount(p.rows)
        )}
      </span>
      <span className="po-track">
        <span className={`po-fill ${p.state === "running" && p.rows === 0 ? "po-busy" : ""}`} style={{ width: `${Math.round((over ? 1 : p.state === "running" && p.rows === 0 ? 0.3 : f ?? 0.15) * 100)}%` }} />
      </span>
    </span>
  );
}

type Load = { status: "loading" } | { status: "ready"; progress: QueryProgress } | { status: "error"; error: ApiError };

/**
 * A running statement, refreshed every second while it's on screen: how long it has run, how far along it is, the step
 * doing the work right now, and (SQL Server) every operator's progress drawn on the plan. PostgreSQL and MySQL show the
 * plan and the engine's own progress reports for the commands that have them, and say plainly what they can't show.
 */
export function QueryProgressPanel({ connection, session, onClose }: { connection: string; session: string; onClose: () => void }) {
  const [load, setLoad] = useState<Load>({ status: "loading" });
  const [paused, setPaused] = useState(false);
  const [visible, setVisible] = useState(() => typeof document === "undefined" || document.visibilityState === "visible");
  const [updatedAt, setUpdatedAt] = useState<number | null>(null);
  const [, tick] = useState(0);
  const plan = useRef<{ statement: string | null; plan: Plan | null }>({ statement: null, plan: null });

  useEffect(() => {
    const onVisibility = () => setVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", onVisibility);
    const clock = setInterval(() => tick((n) => n + 1), 250);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      clearInterval(clock);
    };
  }, []);

  useEffect(() => {
    plan.current = { statement: null, plan: null };
    setLoad({ status: "loading" });
  }, [connection, session]);

  useEffect(() => {
    if (paused || !visible) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        // PostgreSQL and MySQL send their (estimated) plan once per statement; ask again when the statement changes.
        const known = plan.current;
        const next = await (await api()).queryProgress(connection, session, known.plan == null);
        if (cancelled) return;
        if (next.plan) plan.current = { statement: next.statement, plan: next.plan };
        else if (next.statement && known.statement && next.statement !== known.statement) plan.current = { statement: null, plan: null };
        setLoad({ status: "ready", progress: { ...next, plan: next.plan ?? (next.running ? plan.current.plan : null) } });
        setUpdatedAt(Date.now());
        if (!next.running) return;
      } catch (e) {
        if (!cancelled) setLoad({ status: "error", error: errorMessage(e) });
      }
      if (!cancelled) timer = setTimeout(poll, REFRESH_MS);
    };
    poll();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [connection, session, paused, visible]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const progress = load.status === "ready" ? load.progress : null;
  // Mid-flight counters aren't a finished run: draw the plan by its estimates and lay progress over it.
  const livePlan = useMemo(() => (progress?.plan ? { ...progress.plan, kind: "estimated" as const } : null), [progress]);
  const byId = useMemo(() => new Map((progress?.nodes ?? []).map((n) => [n.node_id, n])), [progress]);
  const hotNode = useMemo(() => (progress?.plan && progress.hot_node != null ? nodesOf(progress.plan.root).find((n) => n.id === progress.hot_node) ?? null : null), [progress]);
  const age = updatedAt ? (Date.now() - updatedAt) / 1000 : null;
  const panel = useRef<HTMLElement>(null);

  // Bring the operator doing the work into view when it changes (the diagram can be taller than the panel).
  useEffect(() => {
    const root = panel.current;
    const scroller = root?.querySelector<HTMLElement>(".diagram-scroll");
    const hot = root?.querySelector<HTMLElement>(".po-hot");
    if (!scroller || !hot) return;
    const box = scroller.getBoundingClientRect();
    const card = hot.getBoundingClientRect();
    const calm = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    scroller.scrollTo({
      top: scroller.scrollTop + (card.top - box.top) - (box.height - card.height) / 2,
      left: scroller.scrollLeft + (card.left - box.left) - (box.width - card.width) / 2,
      behavior: calm ? "auto" : "smooth",
    });
  }, [progress?.hot_node, livePlan != null]);
  const live = !paused && visible && progress?.running !== false && load.status !== "error";

  return (
    <section className="qprog" ref={panel} aria-label={t("ui.query_progress.label", { session })}>
      <header className="qp-head">
        <span className="qp-title">
          {withElements(t("ui.query_progress.session", { session: slot(0) }), [<b key="session" className="mono">{session}</b>])}
          {progress && <span className={`qp-state ${progress.running ? "running" : "finished"}`}>{progress.running ? progress.state ?? t("ui.query_progress.running") : t("ui.query_progress.not_running")}</span>}
        </span>
        <span className="spacer" />
        <span className={`freshness ${live ? "live" : ""}`} title={t("ui.query_progress.refresh_title")}>
          <span className="dot" />
          {paused ? t("ui.query_progress.paused") : !visible ? t("ui.common.paused_while_hidden") : load.status === "error" ? t("ui.query_progress.cant_refresh") : progress && !progress.running ? t("ui.query_progress.stopped") : age != null ? age < 1 ? t("ui.query_progress.live_now") : t("ui.query_progress.live_ago", { seconds: age.toFixed(0) }) : t("ui.query_progress.connecting")}
        </span>
        <button type="button" className="btn ghost small" onClick={() => setPaused((p) => !p)} title={paused ? t("ui.query_progress.resume") : t("ui.query_progress.pause")}>
          {paused ? <Play size={12} /> : <Pause size={12} />}
        </button>
        <button type="button" className="btn ghost small" onClick={onClose} title={t("ui.common.close_esc")}>
          <X size={13} />
        </button>
      </header>

      {load.status === "loading" && (
        <div className="plan-skeleton" aria-busy="true">
          <div className="sk-line" style={{ width: "46%" }} />
          <div className="sk-line" style={{ width: "72%" }} />
          <div className="sk-cards">
            <span />
            <span />
            <span />
          </div>
        </div>
      )}
      {load.status === "error" && (
        <div className="qp-note">
          <b>{load.error.message}</b>
          {load.error.hint && <div className="muted">{load.error.hint}</div>}
        </div>
      )}
      {progress && !progress.running && (
        <div className="qp-note calm">
          <Check size={13} /> {t("ui.query_progress.idle", { session })}
        </div>
      )}
      {progress?.running && (
        <div className="qp-body">
          <div className="qp-side">
            {progress.statement && (
              <div className="qp-sql mono" title={progress.statement}>
                {progress.statement}
              </div>
            )}
            <div className="qp-stats">
              {progress.elapsed_ms != null && (
                <span className="qp-big">
                  {duration(progress.elapsed_ms)}
                  <small>{t("ui.query_progress.running")}</small>
                </span>
              )}
              {progress.cpu_ms != null && (
                <span>
                  {withElements(t("ui.query_progress.cpu", { time: slot(0) }), [<b key="time">{duration(progress.cpu_ms)}</b>])}
                </span>
              )}
              {progress.logical_reads != null && (
                <span>
                  {withElements(t("ui.query_progress.reads", { reads: slot(0) }), [<b key="reads">{count(progress.logical_reads)}</b>])}
                </span>
              )}
              {progress.rows_examined != null && (
                <span>
                  {withElements(t("ui.query_progress.rows_examined", { rows: slot(0) }), [<b key="rows">{count(progress.rows_examined)}</b>])}
                </span>
              )}
            </div>
            {progress.wait && (
              <div className="qp-wait" title={progress.wait}>
                {waitWords(progress.wait) ?? t("ui.query_progress.waiting_on", { wait: progress.wait })}
                {progress.wait_ms != null && progress.wait_ms >= 1000 ? ` · ${duration(progress.wait_ms)}` : ""}
              </div>
            )}
            {(progress.percent != null || progress.phase) && (
              <div className="qp-progress">
                {progress.phase && <div className="qp-phase">{progress.phase}</div>}
                {progress.percent != null && (
                  <div className="qp-bar-row">
                    <span className="qp-bar">
                      <span style={{ width: `${Math.round(progress.percent * 100)}%` }} />
                    </span>
                    <b>{percent(progress.percent)}</b>
                  </div>
                )}
                {progress.percent != null && (
                  <div className="qp-basis" title={progress.percent_basis ?? undefined}>
                    {progress.nodes.length > 0 ? t("ui.query_progress.basis_rows") : t("ui.query_progress.basis_engine")}
                  </div>
                )}
              </div>
            )}
            {hotNode && (
              <div className="qp-hot">
                <Flame size={13} />
                <span>
                  {withElements(t("ui.query_progress.working_on", { operator: slot(0) }), [<b key="operator">{hotNode.op}</b>])}
                  {hotNode.object && <span className="mono"> {hotNode.object.split(".").pop()}</span>}
                  {byId.get(hotNode.id) && (
                    <span className="muted">
                      {" · "}
                      {byId.get(hotNode.id)!.rows > 0 ? t("ui.query_progress.out_so_far", { rows: rowCount(byId.get(hotNode.id)!.rows) }) : t("ui.query_progress.still_reading")}
                    </span>
                  )}
                </span>
              </div>
            )}
            {progress.note && <div className="qp-note subtle">{progress.note}</div>}
          </div>
          <div className="qp-plan">
            {livePlan ? (
              <PlanDiagram
                plan={livePlan}
                kindLabel={byId.size > 0 ? t("ui.query_progress.running_now") : t("ui.query_progress.plan_running")}
                focus={progress.hot_node}
                initialZoom={0.8}
                overlay={byId.size > 0 ? (node, box) => <ProgressOverlay p={byId.get(node.id)} hot={node.id === progress.hot_node} box={box} /> : undefined}
              />
            ) : (
              <div className="qp-noplan">{t("ui.query_progress.no_plan")}</div>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
