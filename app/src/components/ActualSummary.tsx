import { Clock, Cpu, Database, Gauge, HardDrive, Scale, TriangleAlert, Users } from "lucide-react";
import { Fragment, type ReactNode } from "react";
import type { Plan } from "../gen/Plan";
import { actualFacts } from "../lib/actual";
import { count, duration, percent, rowCount, when } from "../lib/format";
import { t } from "../lib/i18n";
import { estRowsForRuns } from "../lib/plan-format";

function kb(value: number): string {
  if (value >= 1024 * 1024) return `${(value / 1024 / 1024).toFixed(1)} GB`;
  if (value >= 1024) return `${(value / 1024).toFixed(value >= 10 * 1024 ? 0 : 1)} MB`;
  return `${Math.round(value)} KB`;
}

function where(node: { op: string; object: string | null }): string {
  const table = node.object?.split(".").pop();
  return table ? t("ui.actual_summary.op_on", { op: node.op, table }) : node.op;
}

/** A message whose `{name}` arguments are elements: t() leaves them as `{name}`, and each is put in its place. */
function place(message: string, elements: Record<string, ReactNode>): ReactNode[] {
  return message.split(/\{(\w+)\}/).map((piece, i) => (i % 2 ? <Fragment key={i}>{elements[piece] ?? `{${piece}}`}</Fragment> : piece));
}

function Stat({ icon, value, label, title }: { icon: ReactNode; value: string; label: string; title?: string }) {
  return (
    <span className="as-stat" title={title}>
      {icon}
      <b>{value}</b>
      <span>{label}</span>
    </span>
  );
}

/**
 * What a measured run cost, in words: how long it took, what it read, how much memory it was given and used, and the
 * operators that explain the time (slowest step, worst estimate, a worker doing most of the work, a spill).
 * Each callout selects its operator in the diagram.
 */
export function ActualSummary({ plan, onPick }: { plan: Plan; onPick?: (id: number) => void }) {
  const f = actualFacts(plan);
  if (!f) return null;
  const memory = f.grantedKb != null && f.grantedKb > 0 && f.usedKb != null ? { granted: f.grantedKb, used: f.usedKb } : null;
  const idle = memory ? 1 - memory.used / memory.granted : 0;
  const callouts: { key: string; tone: "hot" | "warn" | "info"; icon: ReactNode; text: ReactNode; node?: number; title: string }[] = [];
  if (f.slowest && f.slowest.share >= 0.2) {
    callouts.push({
      key: "slowest",
      tone: "hot",
      icon: <Gauge size={13} />,
      text: (
        <>{place(t("ui.actual_summary.slowest", { time: duration(f.slowest.ms), share: percent(f.slowest.share) }), { operator: <b>{where(f.slowest.node)}</b> })}</>
      ),
      node: f.slowest.node.id,
      title: t("ui.actual_summary.slowest_hint"),
    });
  }
  if (f.worstEstimate) {
    const n = f.worstEstimate.node;
    // The estimate for the runs that happened: what the factor compares the actual rows with.
    const expected = estRowsForRuns(plan, n);
    callouts.push({
      key: "estimate",
      tone: "warn",
      icon: <Scale size={13} />,
      text: (
        <>{place(t("ui.actual_summary.estimate", { expected: rowCount(expected), factor: count(Math.abs(f.worstEstimate.factor)) }), { actual: <b>{count(n.act_rows ?? 0)}</b>, operator: <b>{where(n)}</b> })}</>
      ),
      node: f.worstEstimate.node.id,
      title: t("ui.actual_summary.estimate_hint"),
    });
  }
  if (f.skew) {
    callouts.push({
      key: "skew",
      tone: "warn",
      icon: <Users size={13} />,
      text: (
        <>{place(t("ui.actual_summary.skew", { threads: String(f.skew.threads), share: percent(f.skew.busiest) }), { operator: <b>{where(f.skew.node)}</b> })}</>
      ),
      node: f.skew.node.id,
      title: t("ui.actual_summary.skew_hint"),
    });
  }
  if (f.spilled) {
    callouts.push({
      key: "spill",
      tone: "warn",
      icon: <HardDrive size={13} />,
      text: f.spillPages > 0 ? t("ui.actual_summary.spill_pages", { pages: count(f.spillPages) }) : t("ui.actual_summary.spill"),
      title: t("ui.actual_summary.spill_hint"),
    });
  }
  return (
    <div className="actual-summary">
      <div className="as-head">
        <span className="as-kicker">{f.lastRunAt ? t("ui.common.last_run") : t("ui.actual_summary.measured_run")}</span>
        {f.elapsedMs != null && (
          <span className="as-elapsed" title={t("ui.actual_summary.elapsed")}>
            <Clock size={14} />
            {duration(f.elapsedMs)}
          </span>
        )}
        <span className="as-stats">
          {f.cpuMs != null && <Stat icon={<Cpu size={12} />} value={duration(f.cpuMs)} label={t("ui.actual_summary.cpu")} title={f.workers ? t("ui.actual_summary.across_workers", { workers: String(f.workers) }) : undefined} />}
          {f.logicalReads != null && (
            <Stat
              icon={<Database size={12} />}
              value={count(f.logicalReads)}
              label={f.physicalReads ? t("ui.actual_summary.reads_from_disk", { disk: count(f.physicalReads) }) : t("ui.actual_summary.reads")}
              title={t("ui.actual_summary.reads_hint")}
            />
          )}
          {f.rows != null && <Stat icon={null} value={count(f.rows)} label={t("ui.actual_summary.rows", { n: f.rows })} />}
          {f.workers != null && f.workers > 1 && <Stat icon={<Users size={12} />} value={String(f.workers)} label={t("ui.actual_summary.workers", { n: f.workers })} />}
        </span>
        {f.lastRunAt && (
          <span className="as-when" title={when(f.lastRunAt.endsWith("Z") ? f.lastRunAt : `${f.lastRunAt}Z`).full}>
            {when(f.lastRunAt.endsWith("Z") ? f.lastRunAt : `${f.lastRunAt}Z`).short}
          </span>
        )}
      </div>
      {memory && (
        <div className="as-memory" title={t("ui.actual_summary.memory_hint")}>
          <span className="as-label">{t("ui.actual_summary.memory")}</span>
          <span className="as-bar" aria-hidden>
            <span style={{ width: `${Math.min(100, (memory.used / memory.granted) * 100)}%` }} />
          </span>
          <span>
            {place(t("ui.actual_summary.memory_used", { granted: kb(memory.granted) }), { used: <b>{kb(memory.used)}</b> })}
            {idle >= 0.75 && memory.granted >= 10 * 1024 && <span className="muted"> · {t("ui.actual_summary.memory_idle", { share: percent(idle) })}</span>}
          </span>
        </div>
      )}
      {f.grantedKb != null && f.grantedKb > 0 && f.usedKb == null && <p className="as-note">{t("ui.actual_summary.memory_unrecorded", { granted: kb(f.grantedKb) })}</p>}
      {f.triggers.length > 0 && <section className="actual-triggers" aria-label={t("ui.actual_summary.triggers_label")}>
        <h4>{t("ui.actual_summary.triggers")}</h4>
        <p className="muted">{t("ui.actual_summary.triggers_note")}</p>
        <ul>{f.triggers.map((trigger, i) => <li key={`${trigger.name}-${i}`}><span>{trigger.relation ? place(t("ui.actual_summary.trigger_on", { relation: trigger.relation }), { trigger: <b>{trigger.name}</b> }) : <b>{trigger.name}</b>}</span><span>{duration(trigger.ms)} · {trigger.calls == null ? t("ui.actual_summary.calls_unrecorded") : t("ui.actual_summary.calls", { calls: count(trigger.calls) })}</span></li>)}</ul>
      </section>}
      {!f.timed && (
        <div className="as-note">
          <TriangleAlert size={12} /> {f.lastRunAt ? t("ui.actual_summary.untimed_past") : t("ui.actual_summary.untimed_run")}
        </div>
      )}
      {callouts.length > 0 && (
        <div className="as-callouts">
          {callouts.map((c) =>
            c.node != null && onPick ? (
              <button type="button" key={c.key} className={`as-callout ${c.tone}`} title={c.title} onClick={() => onPick(c.node!)}>
                {c.icon}
                <span>{c.text}</span>
              </button>
            ) : (
              <span key={c.key} className={`as-callout ${c.tone}`} title={c.title}>
                {c.icon}
                <span>{c.text}</span>
              </span>
            ),
          )}
        </div>
      )}
    </div>
  );
}
