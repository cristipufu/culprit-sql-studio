import { useMemo, useState } from "react";
import type { Deadlock } from "../gen/Deadlock";
import type { DeadlockProcess } from "../gen/DeadlockProcess";
import type { DeadlockResource } from "../gen/DeadlockResource";
import { t, useLanguage } from "../lib/i18n";

const W = 640;
const PROC_W = 172;
const PROC_H = 70;
const RES_W = 168;
const RES_H = 48;
const ROW = 100;
const TOP = 26;
/** An edge label's line height, with room for its halo. */
const LABEL_H = 16;

type Box = { x: number; y: number; w: number; h: number };
type Edge = { from: Box; to: Box; kind: "holds" | "wants"; mode: string | null; proc: string; res: string; leftward: boolean };

function clip(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

/** A lock mode short enough for an edge label (X, S, U, IX, SIX); longer ones only in the tooltip. */
function shortMode(mode: string | null): string | null {
  const m = mode?.split(" (")[0]?.trim();
  return m && m.length <= 3 ? m : null;
}

function path(e: Edge): { d: string; mx: number; my: number } {
  // Connect the facing sides; holds and wants between the same pair bow apart so both stay readable.
  const fromRight = e.from.x + e.from.w <= e.to.x;
  const x1 = fromRight ? e.from.x + e.from.w : e.from.x;
  const x2 = fromRight ? e.to.x : e.to.x + e.to.w;
  const bow = e.kind === "holds" ? -9 : 9;
  const y1 = e.from.y + e.from.h / 2 + bow;
  const y2 = e.to.y + e.to.h / 2 + bow;
  const dx = (x2 - x1) * 0.45;
  return { d: `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`, mx: (x1 + x2) / 2, my: (y1 + y2) / 2 };
}

function who(p: DeadlockProcess): string {
  return p.session ? t("ui.deadlock_graph.session", { session: p.session }) : t("ui.deadlock_graph.process", { process: p.id });
}

function resourceLabel(r: DeadlockResource): { title: string; sub: string } {
  const kind = r.kind.toLowerCase();
  const scope = kind.includes("key") || kind === "record" || kind === "rid" || kind === "ridlock" || kind === "tuple" ? t("ui.deadlock_graph.scope.row") : kind.includes("page") ? t("ui.deadlock_graph.scope.page") : kind.includes("object") || kind === "table" ? t("ui.deadlock_graph.scope.table") : kind === "transaction" ? t("ui.deadlock_graph.scope.transaction") : r.kind;
  return { title: r.object ?? (kind === "transaction" ? r.id : r.kind), sub: [r.index, scope].filter(Boolean).join(" · ") };
}

/** A lock's tooltip: what it is, then the modes it is held and wanted in. */
function lockTitle(r: DeadlockResource, label: { title: string; sub: string }): string {
  const held = r.holders.map((h) => h.mode ?? "?").join(", ") || "?";
  const wanted = r.waiters.map((h) => h.mode ?? "?").join(", ") || "?";
  return label.sub
    ? t("ui.deadlock_graph.lock_title_scope", { object: label.title, scope: label.sub, held, wanted })
    : t("ui.deadlock_graph.lock_title", { object: label.title, held, wanted });
}

/** A process box's accessible name: who, whether it was rolled back, and its statement. */
function processLabel(p: DeadlockProcess): string {
  const statement = p.statement ?? t("ui.deadlock_graph.statement_unknown");
  return p.victim ? t("ui.deadlock_graph.process_label_victim", { process: who(p), statement }) : t("ui.deadlock_graph.process_label", { process: who(p), statement });
}

/** An edge's label: "holds" or "wants", with the lock mode when it is short enough to fit. */
function edgeLabel(e: Edge): string {
  const mode = shortMode(e.mode);
  return mode ? t("ui.deadlock_graph.edge_mode", { kind: e.kind, mode }) : t("ui.deadlock_graph.edge", { kind: e.kind });
}

/**
 * The cycle as a picture: processes on the sides, the locks they fight over in the middle. A solid line means "holds",
 * a dashed arrow means "wants"; follow them and the circle closes. The rolled-back process is marked.
 */
export function DeadlockGraph({ deadlock, onPick }: { deadlock: Deadlock; onPick?: (process: DeadlockProcess) => void }) {
  const [focus, setFocus] = useState<string | null>(null);
  const language = useLanguage();

  const layout = useMemo(() => {
    const resources = deadlock.resources.length ? deadlock.resources : [];
    const rows = Math.max(resources.length, Math.ceil(deadlock.processes.length / 2), 1);
    const height = TOP * 2 + (rows - 1) * ROW + Math.max(PROC_H, RES_H);
    const resBox = new Map<string, Box>();
    resources.forEach((r, i) => resBox.set(r.id, { x: (W - RES_W) / 2, y: TOP + i * ROW + (PROC_H - RES_H) / 2, w: RES_W, h: RES_H }));
    // Each process sits level with the locks it touches, alternating sides so the cycle reads left → right → left.
    const procBox = new Map<string, Box>();
    deadlock.processes.forEach((p, i) => {
      const touched = resources.map((r, j) => (r.holders.some((h) => h.process === p.id) || r.waiters.some((h) => h.process === p.id) ? j : -1)).filter((j) => j >= 0);
      const centre = touched.length ? touched.reduce((a, b) => a + b, 0) / touched.length : Math.floor(i / 2);
      const left = i % 2 === 0;
      procBox.set(p.id, { x: left ? 0 : W - PROC_W, y: TOP + centre * ROW + (deadlock.processes.length > 2 ? Math.floor(i / 2) * 8 : 0), w: PROC_W, h: PROC_H });
    });
    const edges: Edge[] = [];
    for (const r of resources) {
      const rb = resBox.get(r.id)!;
      for (const h of r.holders) {
        const pb = procBox.get(h.process);
        if (pb) edges.push({ from: rb, to: pb, kind: "holds", mode: h.mode, proc: h.process, res: r.id, leftward: pb.x < rb.x });
      }
      for (const h of r.waiters) {
        const pb = procBox.get(h.process);
        if (pb) edges.push({ from: pb, to: rb, kind: "wants", mode: h.mode, proc: h.process, res: r.id, leftward: rb.x < pb.x });
      }
    }
    return { height, resBox, procBox, edges };
  }, [deadlock]);

  const lit = (e: Edge) => !focus || focus === e.proc || focus === e.res;

  // Labels sit on their curve (the halo hides the line beneath); labels that would touch are pushed apart vertically.
  const labelY = useMemo(() => {
    const boxes = layout.edges.map((e, i) => {
      const p = path(e);
      const text = edgeLabel(e);
      return { i, x: p.mx, half: text.length * 3.3, y: p.my + 4 };
    });
    const sorted = [...boxes].sort((a, b) => a.y - b.y);
    for (let a = 0; a < sorted.length; a++) {
      for (let b = a + 1; b < sorted.length; b++) {
        const A = sorted[a]!;
        const B = sorted[b]!;
        const gap = B.y - A.y;
        if (Math.abs(A.x - B.x) < A.half + B.half + 4 && gap < LABEL_H) {
          A.y -= (LABEL_H - gap) / 2;
          B.y += (LABEL_H - gap) / 2;
        }
      }
    }
    return new Map(boxes.map((b) => [b.i, b.y]));
  }, [layout, language]);

  return (
    <figure className="dl-graph">
      <svg viewBox={`0 0 ${W} ${layout.height}`} role="group" aria-label={t("ui.deadlock_graph.label", { headline: deadlock.headline })} preserveAspectRatio="xMidYMin meet">
        <defs>
          <marker id="dl-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" className="dl-arrowhead" />
          </marker>
        </defs>
        {layout.edges.map((e, i) => {
          const p = path(e);
          return (
            <g key={i} className={`dl-edge ${e.kind} ${lit(e) ? "" : "dim"}`}>
              <title>{e.mode ? t("ui.deadlock_graph.edge_title_mode", { kind: e.kind, mode: e.mode }) : t("ui.deadlock_graph.edge_title", { kind: e.kind })}</title>
              <path d={p.d} markerEnd={e.kind === "wants" ? "url(#dl-arrow)" : undefined} />
              {/* The label must fit the gap between boxes: a long lock mode (RangeS-U, X,REC_NOT_GAP) goes to the tooltip. */}
              <text x={p.mx} y={labelY.get(i)} textAnchor="middle" className="dl-edge-label">
                {edgeLabel(e)}
              </text>
            </g>
          );
        })}
        {deadlock.resources.map((r) => {
          const b = layout.resBox.get(r.id)!;
          const label = resourceLabel(r);
          return (
            <g
              key={r.id}
              className={`dl-res ${focus && focus !== r.id && !layout.edges.some((e) => e.res === r.id && e.proc === focus) ? "dim" : ""}`}
              tabIndex={0}
              role="button"
              aria-label={t("ui.deadlock_graph.lock_label", { object: label.title, scope: label.sub })}
              onMouseEnter={() => setFocus(r.id)}
              onMouseLeave={() => setFocus(null)}
              onFocus={() => setFocus(r.id)}
              onBlur={() => setFocus(null)}
            >
              <title>{lockTitle(r, label)}</title>
              <rect x={b.x} y={b.y} width={b.w} height={b.h} rx={b.h / 2} />
              <text x={b.x + b.w / 2} y={b.y + 20} textAnchor="middle" className="dl-res-title">
                {clip(label.title, 22)}
              </text>
              <text x={b.x + b.w / 2} y={b.y + 36} textAnchor="middle" className="dl-res-sub">
                {clip(label.sub, 26)}
              </text>
            </g>
          );
        })}
        {deadlock.processes.map((p) => {
          const b = layout.procBox.get(p.id)!;
          const meta = [p.application, p.login].filter(Boolean).join(" · ");
          return (
            <g
              key={p.id}
              className={`dl-proc ${p.victim ? "victim" : ""} ${focus && focus !== p.id && !layout.edges.some((e) => e.proc === p.id && e.res === focus) ? "dim" : ""}`}
              tabIndex={0}
              role="button"
              aria-label={processLabel(p)}
              onMouseEnter={() => setFocus(p.id)}
              onMouseLeave={() => setFocus(null)}
              onFocus={() => setFocus(p.id)}
              onBlur={() => setFocus(null)}
              onClick={() => onPick?.(p)}
              onKeyDown={(e) => e.key === "Enter" && onPick?.(p)}
            >
              <title>{p.statement ?? who(p)}</title>
              <rect x={b.x} y={b.y} width={b.w} height={b.h} rx={9} />
              <text x={b.x + 12} y={b.y + 21} className="dl-proc-title">
                {who(p)}
              </text>
              {p.victim && (
                <g className="dl-victim-tag">
                  <rect x={b.x + b.w - 84} y={b.y + 9} width={74} height={17} rx={8.5} />
                  <text x={b.x + b.w - 47} y={b.y + 21} textAnchor="middle">
                    {t("ui.deadlock_graph.rolled_back")}
                  </text>
                </g>
              )}
              <text x={b.x + 12} y={b.y + 38} className="dl-proc-meta">
                {clip(meta || p.host || "", 26)}
              </text>
              <text x={b.x + 12} y={b.y + 56} className="dl-proc-sql">
                {clip(p.statement ?? "", 25)}
              </text>
            </g>
          );
        })}
      </svg>
      <figcaption className="dl-legend">
        <span>
          <i className="dl-key holds" /> {t("ui.deadlock_graph.edge", { kind: "holds" })}
        </span>
        <span>
          <i className="dl-key wants" /> {t("ui.deadlock_graph.edge", { kind: "wants" })}
        </span>
        <span className="muted">{t("ui.deadlock_graph.hint")}</span>
      </figcaption>
    </figure>
  );
}
