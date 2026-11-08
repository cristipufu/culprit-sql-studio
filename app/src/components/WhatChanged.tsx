import { ArrowRight, ChevronDown, ChevronRight, GitCompareArrows, SquarePen, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { ChangeRequest } from "../gen/ChangeRequest";
import type { Changes } from "../gen/Changes";
import type { ChangeStatus } from "../gen/ChangeStatus";
import type { PeriodStats } from "../gen/PeriodStats";
import type { QueryChange } from "../gen/QueryChange";
import type { Report } from "../gen/Report";
import { api, errorMessage, type ApiError } from "../lib/api";
import { canOpenQueryInEditor, prepareQueryForEditor, prepareRawQueryTextForEditor } from "../lib/editor-sql";
import { count, duration, percent } from "../lib/format";
import { useHotkeys } from "../lib/hotkeys";
import { t } from "../lib/i18n";
import { querySummary } from "../lib/query-summary";
import { eventTime, useEventsVersion } from "../lib/events";
import { changeRequest, setState } from "../lib/store";
import { AroundThen, PeriodStrip } from "./ChangeTimeline";
import { EventIcon, eventTone } from "./EventIcon";
import { MarkButton } from "./MarkButton";
import { rangeLabel, when } from "./TimeChart";

type State = { status: "loading" } | { status: "ready"; changes: Changes } | { status: "error"; error: ApiError };

const GROUPS: { status: ChangeStatus; label: () => string; hint: () => string; tone: string }[] = [
  { status: "regressed", label: () => t("ui.what_changed.group.regressed"), hint: () => t("ui.what_changed.group.regressed_hint"), tone: "high" },
  { status: "new", label: () => t("ui.what_changed.group.new"), hint: () => t("ui.what_changed.group.new_hint"), tone: "medium" },
  { status: "improved", label: () => t("ui.what_changed.group.improved"), hint: () => t("ui.what_changed.group.improved_hint"), tone: "good" },
  { status: "stopped", label: () => t("ui.what_changed.group.stopped"), hint: () => t("ui.what_changed.group.stopped_hint"), tone: "low" },
  { status: "not-comparable", label: () => t("ui.what_changed.group.not_comparable"), hint: () => t("ui.what_changed.group.not_comparable_hint"), tone: "muted" },
  { status: "low-sample", label: () => t("ui.what_changed.group.low_sample"), hint: () => t("ui.what_changed.group.low_sample_hint"), tone: "muted" },
  { status: "unchanged", label: () => t("ui.what_changed.group.unchanged"), hint: () => t("ui.what_changed.group.unchanged_hint"), tone: "muted" },
];

const PRESETS: { label: () => string; hours: number }[] = [
  { label: () => t("ui.what_changed.last_hour"), hours: 1 },
  { label: () => t("ui.what_changed.last_6_h"), hours: 6 },
  { label: () => t("ui.what_changed.last_24_h"), hours: 24 },
];

function hoursOf(p: { start: string; end: string }): number {
  return (Date.parse(p.end) - Date.parse(p.start)) / 3_600_000;
}

function spanWords(hours: number): string {
  if (hours < 1 / 60) return t("ui.what_changed.span_seconds", { n: String(Math.max(1, Math.round(hours * 3600))) });
  if (hours < 1.5) return t("ui.what_changed.span_minutes", { n: String(Math.round(hours * 60)) });
  if (hours < 48) return t("ui.what_changed.span_hours", { n: String(Math.round(hours)) });
  return t("ui.what_changed.span_days", { n: Math.round(hours / 24) });
}

/** One run's time with enough precision to compare two: "7.0 ms", not "7 ms". */
function runTime(ms: number): string {
  return ms >= 1 && ms < 10 ? `${ms.toFixed(1)} ms` : duration(ms);
}

function ratioWords(c: QueryChange): string | null {
  if (c.ratio == null) return null;
  const r = c.status === "improved" ? 1 / c.ratio : c.ratio;
  return r >= 10 ? `${Math.round(r)}×` : `${r.toFixed(1).replace(/\.0$/, "")}×`;
}

/** Query Store names its plans; scan-based sources only know the shapes Culprit captured. */
function namedPlans(changes: Changes): boolean {
  return changes.source === "query-store" && changes.engine === "mssql";
}

/** The sentence for a row, in local time; the CLI and agents get the same facts in UTC. */
function sentence(c: QueryChange, changes: Changes): string {
  // `String`: a change without a ratio reads as it always has.
  const ratio = String(ratioWords(c));
  const s = c.plan_switch;
  const named = namedPlans(changes);
  const moved = (direction: "slower" | "faster"): string => {
    if (!s) return t("ui.what_changed.sentence.changed", { direction, ratio });
    const at = s.at ? when(Date.parse(s.at)) : null;
    if (named) {
      return at != null
        ? t("ui.what_changed.sentence.switched_named_at", { direction, ratio, at, from: s.from, to: s.to })
        : t("ui.what_changed.sentence.switched_named", { direction, ratio, from: s.from, to: s.to });
    }
    return at != null ? t("ui.what_changed.sentence.switched_at", { direction, ratio, at }) : t("ui.what_changed.sentence.switched", { direction, ratio });
  };
  switch (c.status) {
    case "regressed":
      if (!s && c.same_plan) return named ? t("ui.what_changed.sentence.same_plan", { ratio }) : t("ui.what_changed.sentence.same_estimated_plan", { ratio });
      return moved("slower");
    case "improved":
      return moved("faster");
    case "new":
      return t("ui.what_changed.sentence.new", { share: percent(c.recent_share) });
    case "stopped":
      return t("ui.what_changed.sentence.stopped");
    case "not-comparable":
      return c.reason ? t("ui.what_changed.sentence.not_comparable_reason", { reason: c.reason }) : t("ui.what_changed.sentence.not_comparable");
    case "low-sample":
      return t("ui.what_changed.sentence.low_sample", { before: count(c.baseline?.executions ?? 0), after: count(c.recent?.executions ?? 0) });
    default:
      return t("ui.what_changed.sentence.unchanged");
  }
}

/** Before → after on a shared log scale, so rows compare at a glance. */
function Dumbbell({ before, after, scale, tone }: { before?: PeriodStats | null; after?: PeriodStats | null; scale: [number, number]; tone: string }) {
  const [lo, hi] = scale;
  const pos = (ms: number) => {
    const v = Math.log10(Math.max(ms, lo));
    return Math.min(100, Math.max(0, ((v - Math.log10(lo)) / (Math.log10(hi) - Math.log10(lo) || 1)) * 100));
  };
  const a = before && before.executions > 0 ? pos(before.mean_ms) : null;
  const b = after && after.executions > 0 ? pos(after.mean_ms) : null;
  return (
    <div className={`dumbbell ${tone}`} aria-hidden="true">
      <div className="db-track" />
      {a != null && b != null && <div className="db-bar" style={{ left: `${Math.min(a, b)}%`, width: `${Math.abs(b - a)}%` }} />}
      {a != null && <i className="db-before" style={{ left: `${a}%` }} />}
      {b != null && <i className="db-after" style={{ left: `${b}%` }} />}
    </div>
  );
}

function toIsoLocal(value: string): string | null {
  const t = Date.parse(value);
  return Number.isFinite(t) ? new Date(t).toISOString() : null;
}

function toInputLocal(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export default function WhatChanged({ report, request, onClose }: { report: Report; request: ChangeRequest; onClose: () => void }) {
  const [state, setLocal] = useState<State>({ status: "loading" });
  const [open, setOpen] = useState<string | null>(null);
  const [cursor, setCursor] = useState(0);
  const [showUnchanged, setShowUnchanged] = useState(false);
  const list = useRef<HTMLDivElement>(null);
  const connection = report.snapshot.connection;
  const key = JSON.stringify(request);
  // The request as of its contents: a new object with the same fields doesn't reload.
  const asked = useMemo(() => JSON.parse(key) as ChangeRequest, [key]);
  // A mark added or removed anywhere changes what explains the queries.
  const marks = useEventsVersion();

  useEffect(() => {
    let cancelled = false;
    setLocal({ status: "loading" });
    setOpen(null);
    setCursor(0);
    api()
      .then((client) => client.changes(connection, asked))
      .then((changes) => !cancelled && setLocal({ status: "ready", changes }))
      .catch((e) => !cancelled && setLocal({ status: "error", error: errorMessage(e) }));
    return () => {
      cancelled = true;
    };
  }, [connection, asked, marks]);

  const changes = state.status === "ready" ? state.changes : null;
  const rows = useMemo(() => (changes ? changes.queries.filter((q) => q.status !== "unchanged" || showUnchanged) : []), [changes, showUnchanged]);
  const scale = useMemo<[number, number]>(() => {
    const values = rows.flatMap((q) => [q.baseline?.mean_ms, q.recent?.mean_ms]).filter((v): v is number => v != null && v > 0);
    if (!values.length) return [0.01, 1000];
    return [Math.max(Math.min(...values) / 1.6, 0.005), Math.max(...values) * 1.6];
  }, [rows]);
  const known = useMemo(() => new Set(report.snapshot.workload?.queries.map((q) => q.id) ?? []), [report]);

  useHotkeys({
    j: () => setCursor((c) => Math.min(rows.length - 1, c + 1)),
    k: () => setCursor((c) => Math.max(0, c - 1)),
    ArrowDown: () => setCursor((c) => Math.min(rows.length - 1, c + 1)),
    ArrowUp: () => setCursor((c) => Math.max(0, c - 1)),
    Enter: () => rows[cursor] && setOpen((o) => (o === rows[cursor]!.query_id ? null : rows[cursor]!.query_id)),
    Escape: onClose,
  });
  useEffect(() => {
    list.current?.querySelector(".wc-row.cursor")?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  const setRequest = (next: Partial<ChangeRequest>) =>
    setState({ changes: changeRequest({ ...next, min_executions: request.min_executions, threshold: request.threshold }) });

  return (
    <div className="wc">
      <div className="wc-head">
        <div className="wc-title">
          <GitCompareArrows size={15} />
          <h2>{t("ui.what_changed.title")}</h2>
          <span className="spacer" />
          <button type="button" className="icon-btn" title={t("ui.common.close_esc")} onClick={onClose}>
            <X size={15} />
          </button>
        </div>
        <div className="wc-periods">
          {changes ? (
            <>
              <span className="wc-period recent">
                <b>{t("ui.what_changed.recent")}</b> {rangeLabel(Date.parse(changes.recent.start), Date.parse(changes.recent.end))} <span className="muted">· {spanWords(hoursOf(changes.recent))}</span>
              </span>
              <span className="muted">{t("ui.what_changed.vs")}</span>
              <span className="wc-period">
                <b>{t("ui.what_changed.baseline")}</b> {rangeLabel(Date.parse(changes.baseline.start), Date.parse(changes.baseline.end))} <span className="muted">· {spanWords(hoursOf(changes.baseline))}</span>
              </span>
            </>
          ) : (
            <span className="muted">{t("ui.what_changed.comparing")}</span>
          )}
        </div>
        {changes && changes.events.length > 0 && (
          <PeriodStrip
            changes={changes}
            onFocusEvent={(id) => {
              const at = rows.findIndex((q) => q.context.some((l) => l.event === id));
              if (at >= 0) {
                setCursor(at);
                setOpen(rows[at]!.query_id);
              }
            }}
          />
        )}
        <div className="wc-controls">
          <span className="seg">
            {PRESETS.map((p) => (
              <button type="button" key={p.hours} className={request.recent_hours === p.hours && !request.since ? "on" : ""} onClick={() => setRequest({ recent_hours: p.hours })}>
                {p.label()}
              </button>
            ))}
          </span>
          <MarkButton connection={connection} />
          <label className="wc-since">
            {t("ui.what_changed.since")}
            <input
              type="datetime-local"
              value={toInputLocal(request.since ?? null)}
              onChange={(e) => {
                const iso = toIsoLocal(e.target.value);
                if (iso) setRequest({ since: iso });
              }}
            />
          </label>
        </div>
      </div>

      {state.status === "loading" && (
        <div className="wc-body">
          {[0, 1, 2, 3].map((i) => (
            <div key={i} className="wc-skeleton" style={{ animationDelay: `${i * 80}ms` }} />
          ))}
        </div>
      )}
      {state.status === "error" && (
        <div className="wc-body">
          <div className="notice">
            <div>
              <b>{state.error.message}</b>
              {state.error.hint && <div className="muted">{state.error.hint}</div>}
            </div>
          </div>
        </div>
      )}
      {changes && (
        <div className="wc-body" ref={list}>
          <Summary changes={changes} />
          <AroundThen
            changes={changes}
            onFocusQuery={(id) => {
              const at = rows.findIndex((q) => q.query_id === id);
              if (at >= 0) {
                setCursor(at);
                setOpen(id);
              }
            }}
          />
          {GROUPS.map((g) => {
            const group = rows.filter((q) => q.status === g.status);
            if (!group.length) return null;
            return (
              <section key={g.status} className="wc-group">
                <h3 title={g.hint()}>
                  <span className={`wc-dot ${g.tone}`} />
                  {g.label()} <span className="muted">{group.length}</span>
                </h3>
                {group.map((q) => {
                  const index = rows.indexOf(q);
                  const expanded = open === q.query_id;
                  const summary = querySummary(q.text, q.object_name);
                  const switchTitle =
                    q.plan_switch?.what ?? (q.plan_switch?.from_label && q.plan_switch?.to_label ? `${q.plan_switch.from_label} → ${q.plan_switch.to_label}` : undefined);
                  return (
                    <div key={q.query_id} className={`wc-row ${index === cursor ? "cursor" : ""} ${expanded ? "open" : ""}`}>
                      <button type="button"
                        className="wc-main"
                        onClick={() => {
                          setCursor(index);
                          setOpen(expanded ? null : q.query_id);
                        }}
                        aria-expanded={expanded}
                      >
                        <span className="wc-caret">{expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}</span>
                        <span className="wc-what">
                          <span className="qsum">
                            <b>{summary.verb}</b>
                            {summary.target && <span className="qsum-table">{summary.target}</span>}
                            {summary.predicate && <span className="qsum-pred">{summary.predicate}</span>}
                          </span>
                          <span className="wc-sentence">{sentence(q, changes)}</span>
                        </span>
                        <span className="wc-numbers">
                          <Dumbbell before={q.baseline} after={q.recent} scale={scale} tone={g.tone} />
                          <span className="wc-times">
                            {q.baseline ? runTime(q.baseline.mean_ms) : "—"}
                            <ArrowRight size={11} />
                            {q.recent ? runTime(q.recent.mean_ms) : "—"}
                          </span>
                        </span>
                        <span className="wc-side">
                          {ratioWords(q) && <span className={`wc-ratio ${g.tone}`}>{ratioWords(q)}</span>}
                          {q.plan_switch && (
                            <span className="wc-plan" title={switchTitle}>
                              {namedPlans(changes) ? t("ui.what_changed.plan_switch", { from: q.plan_switch.from, to: q.plan_switch.to }) : t("ui.what_changed.plan_changed")}
                            </span>
                          )}
                        </span>
                        <Context q={q} changes={changes} />
                      </button>
                      {expanded && <Detail q={q} changes={changes} report={report} inReport={known.has(q.query_id)} />}
                    </div>
                  );
                })}
              </section>
            );
          })}
          {changes.unchanged > 0 && (
            <button type="button" className="btn ghost wc-more" onClick={() => setShowUnchanged((v) => !v)}>
              {showUnchanged
                ? t("ui.what_changed.hide_unchanged", { n: changes.unchanged, count: String(changes.unchanged) })
                : t("ui.what_changed.show_unchanged", { n: changes.unchanged, count: String(changes.unchanged) })}
            </button>
          )}
          {rows.length === 0 && changes.unchanged === 0 && (
            <div className="empty" style={{ padding: 28 }}>
              <h3>{t("ui.what_changed.nothing")}</h3>
              <p>{t("ui.what_changed.nothing_why")}</p>
            </div>
          )}
          <footer className="wc-foot">
            <span>
              {t("ui.what_changed.method", { runs: count(changes.min_executions), threshold: String(changes.threshold) })}
            </span>
            {changes.notes
              // The period chips already say which scans the periods follow, in local time.
              .filter((n) => !n.startsWith("Periods follow Culprit's scans"))
              .map((n) => (
                <span key={n}>{n}</span>
              ))}
            <span className="muted">{t("ui.what_changed.keys")}</span>
          </footer>
        </div>
      )}
    </div>
  );
}

function Summary({ changes }: { changes: Changes }) {
  const rate = (s: PeriodStats, p: { start: string; end: string }) => s.total_ms / Math.max(hoursOf(p), 1 / 3600);
  const short = hoursOf(changes.baseline) < 1 || hoursOf(changes.recent) < 1;
  const before = rate(changes.baseline_total, changes.baseline);
  const after = rate(changes.recent_total, changes.recent);
  const ratio = before > 0 ? after / before : null;
  const tone = ratio == null ? "" : ratio >= 1.15 ? "high" : ratio <= 1 / 1.15 ? "good" : "";
  const counts = (status: ChangeStatus) => changes.queries.filter((q) => q.status === status).length;
  return (
    <div className="wc-summary">
      <div className="wc-stat">
        <span className="label">{short ? t("ui.what_changed.database_time") : t("ui.what_changed.database_time_per_hour")}</span>
        <span className="value">
          {short ? duration(changes.baseline_total.total_ms) : duration(before)}
          <ArrowRight size={13} />
          <b>{short ? duration(changes.recent_total.total_ms) : duration(after)}</b>
          {ratio != null && Number.isFinite(ratio) && <span className={`wc-ratio ${tone}`}>{t("ui.what_changed.rate", { ratio: String(ratio >= 10 ? Math.round(ratio) : ratio.toFixed(1)) })}</span>}
        </span>
        {short && (
          <span className="hint">
            {t("ui.what_changed.over_spans", { before: spanWords(hoursOf(changes.baseline)), since: spanWords(hoursOf(changes.recent)) })}
          </span>
        )}
      </div>
      <div className="wc-stat">
        <span className="label">{t("common.metric.runs")}</span>
        <span className="value">
          {count(changes.baseline_total.executions)}
          <ArrowRight size={13} />
          <b>{count(changes.recent_total.executions)}</b>
        </span>
      </div>
      <div className="wc-stat">
        <span className="label">{t("ui.what_changed.queries")}</span>
        <span className="value wc-counts">
          <span className="high">{t("ui.what_changed.count_slower", { count: String(counts("regressed")) })}</span>
          <span className="medium">{t("ui.what_changed.count_new", { count: String(counts("new")) })}</span>
          <span className="good">{t("ui.what_changed.count_faster", { count: String(counts("improved")) })}</span>
        </span>
      </div>
      <p className="wc-source">
        {changes.source === "query-store" ? t("ui.what_changed.source_query_store") : changes.source === "slow-log" ? t("ui.what_changed.source_slow_log") : t("ui.what_changed.source_scans")}
        {changes.baseline_coverage < 0.99 && <span className="medium"> · {t("ui.what_changed.coverage", { share: percent(changes.baseline_coverage) })}</span>}
      </p>
    </div>
  );
}

function Detail({ q, changes, report, inReport }: { q: QueryChange; changes: Changes; report: Report; inReport: boolean }) {
  const stat = report.snapshot.workload?.queries.find((x) => x.id === q.query_id);
  const editorSql = stat ? prepareQueryForEditor(stat, report.snapshot.server.engine) : prepareRawQueryTextForEditor(q.text);
  const row = (label: string, pick: (s: PeriodStats) => string) => (
    <tr>
      <th>{label}</th>
      <td>{q.baseline ? pick(q.baseline) : "—"}</td>
      <td>{q.recent ? pick(q.recent) : "—"}</td>
    </tr>
  );
  return (
    <div className="wc-detail">
      <table className="wc-table">
        <thead>
          <tr>
            <th />
            <th>{t("ui.what_changed.baseline")}</th>
            <th>{t("ui.what_changed.recent")}</th>
          </tr>
        </thead>
        <tbody>
          {row(t("common.metric.runs"), (s) => count(s.executions))}
          {row(t("ui.what_changed.average"), (s) => runTime(s.mean_ms))}
          {(q.baseline?.min_ms != null || q.recent?.min_ms != null) && row(t("ui.what_changed.fastest"), (s) => (s.min_ms != null ? runTime(s.min_ms) : "—"))}
          {(q.baseline?.max_ms != null || q.recent?.max_ms != null) && row(t("ui.what_changed.slowest"), (s) => (s.max_ms != null ? runTime(s.max_ms) : "—"))}
          {row(t("ui.what_changed.database_time"), (s) => duration(s.total_ms))}
        </tbody>
      </table>
      {q.plan_switch && (
        <div className="wc-switch">
          <b>{namedPlans(changes) ? t("ui.what_changed.plan_switch_detail", { from: q.plan_switch.from, to: q.plan_switch.to }) : t("ui.what_changed.captured_plan_changed")}</b>
          {q.plan_switch.what && <span>{q.plan_switch.what}</span>}
          {!q.plan_switch.what && (q.plan_switch.from_label || q.plan_switch.to_label) && (
            <span>
              {q.plan_switch.from_label ?? "?"} → {q.plan_switch.to_label ?? "?"}
            </span>
          )}
        </div>
      )}
      {q.context.length > 0 && (
        <div className="wc-related">
          <span className="label">{t("ui.what_changed.around_it")}</span>
          {q.context.map((l) => {
            const e = changes.events.find((x) => x.id === l.event);
            if (!e) return null;
            return (
              <span key={l.event} className="wc-context">
                <span className={`tc-ev-dot ${eventTone(e.kind)}`}>
                  <EventIcon kind={e.kind} size={10} />
                </span>
                <span>
                  <b>{e.title}</b> · {eventTime(e)} <span className="muted">· {l.why}</span>
                </span>
              </span>
            );
          })}
        </div>
      )}
      <pre className="wc-sql">{q.text}</pre>
      <div className="wc-actions">
        <span className="muted">{t("ui.what_changed.query_id", { id: q.query_id })}</span>
        <span className="spacer" />
        {inReport && (
          <button type="button" className="btn small" onClick={() => setState({ view: "queries", query: q.query_id })}>
            {t("ui.common.open_in_queries")}
          </button>
        )}
        {canOpenQueryInEditor(q.text, editorSql) && (
          <button type="button" className="btn small" onClick={() => setState({ view: "editor", editorSql })}>
            <SquarePen size={12} /> {t("ui.common.open_in_editor")}
          </button>
        )}
      </div>
    </div>
  );
}

/** The likeliest explanation from around the periods, on the row itself: "Index ix_orders_status was dropped …". */
function Context({ q, changes }: { q: QueryChange; changes: Changes }) {
  const top = q.context.find((l) => l.strong);
  const e = top ? changes.events.find((x) => x.id === top.event) : undefined;
  if (!top || !e) return null;
  return (
    <span className="wc-context row-context" title={`${e.title} · ${eventTime(e)} · ${top.why}`}>
      <span className={`tc-ev-dot ${eventTone(e.kind)}`}>
        <EventIcon kind={e.kind} size={10} />
      </span>
      <span className="row-context-text">
        <span className="muted">{t("ui.what_changed.around_then")}</span> <b>{e.title}</b> <span className="muted">· {eventTime(e)}</span>
      </span>
    </span>
  );
}
