import { GitCompareArrows, Search } from "lucide-react";
import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import type { Category } from "../gen/Category";
import type { ChangeRequest } from "../gen/ChangeRequest";
import type { Finding } from "../gen/Finding";
import type { Report } from "../gen/Report";
import { ago, byPeriod, bytes, count, duration, percent } from "../lib/format";
import { windowPhrase, windowUnknown } from "../lib/rates";
import { useHotkeys } from "../lib/hotkeys";
import { key, t } from "../lib/i18n";
import { changeRequest, setState, useStore } from "../lib/store";
import { planFlags, trendNote, WorkloadTrend } from "../components/Trends";
import { chartEvents } from "../components/ChangeTimeline";
import { MarkButton } from "../components/MarkButton";
import { useEvents } from "../lib/events";
import type { Flag, Range } from "../components/TimeChart";
import { CheckedFine } from "../components/CheckedFine";
import { WorkloadUnlock } from "../components/WorkloadUnlock";
import { HealthLine, WaitsVital, type WaitsTab } from "../components/Waits";
import { FindingDetail } from "./FindingDetail";
import { actionability, actionLabel, isActionable } from "../lib/findings";
import { can } from "../lib/engines";

const CATEGORIES: Category[] = ["indexes", "queries", "maintenance", "configuration"];

function categoryLabel(category: Category): string {
  switch (category) {
    case "indexes":
      return t("ui.overview.category.indexes");
    case "queries":
      return t("ui.overview.category.queries");
    case "maintenance":
      return t("ui.overview.category.maintenance");
    case "configuration":
      return t("ui.overview.category.configuration");
  }
}

/** One chip per category that has findings, counted from the list itself so a chip's number is what it shows. */
function categoryCounts(findings: Finding[]): { category: Category; count: number }[] {
  return CATEGORIES.map((category) => ({ category, count: findings.filter((f) => f.category === category).length })).filter((c) => c.count > 0);
}

// The waits and "What changed" panels load on first use, so Overview's first paint stays as light as before.
const WaitsPanel = lazy(() => import("../components/WaitsPanel"));
const WhatChanged = lazy(() => import("../components/WhatChanged"));

/** Compare everything since `at` (a plan change, a deploy) with the same length of time before it. */
export function compareSince(at: number): void {
  setState({ view: "overview", changes: changeRequest({ since: new Date(at).toISOString() }) });
}

/** The recent period a request asks for, to show on the chart. */
function requestedRange(request: ChangeRequest | null, end: number): Range | null {
  if (!request) return null;
  if (request.recent_start && request.recent_end) return { from: Date.parse(request.recent_start), to: Date.parse(request.recent_end) };
  if (request.since) return { from: Date.parse(request.since), to: end };
  if (request.recent_hours) return { from: end - request.recent_hours * 3_600_000, to: end };
  return null;
}

export function Overview({ report }: { report: Report }) {
  const selected = useStore((s) => s.finding);
  const dismissed = useStore((s) => s.dismissed);
  const changes = useStore((s) => s.changes);
  const [category, setCategory] = useState<Category | "all">("all");
  const [filter, setFilter] = useState("");
  const [showDismissed, setShowDismissed] = useState(false);
  const [showContext, setShowContext] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const [waits, setWaits] = useState<{ tab: WaitsTab; category?: string } | null>(null);
  const openWaits = (tab: WaitsTab, category?: string) => setWaits({ tab, category });

  const visible = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return report.findings.filter((f) => {
      if (!showContext && !q && !isActionable(f) && f.id !== selected) return false;
      if (category !== "all" && f.category !== category) return false;
      if (!showDismissed && dismissed[f.id]) return false;
      if (!q) return true;
      return `${f.title} ${f.subject.name} ${f.id}`.toLowerCase().includes(q);
    });
  }, [report.findings, category, filter, dismissed, showDismissed, showContext, selected]);

  const dismissedCount = report.findings.filter((f) => dismissed[f.id]).length;
  // Chips count what their filter would list: dismissed findings only while they're shown.
  const listed = showDismissed ? report.findings : report.findings.filter((f) => !dismissed[f.id]);
  const noOpportunities = listed.length > 0 && !listed.some(isActionable);
  const contextCount = listed.filter((f) => !isActionable(f)).length;
  const counted = showContext ? listed : listed.filter((f) => isActionable(f) || f.id === selected);
  const current = report.findings.find((f) => f.id === selected) ?? visible[0] ?? null;
  // A source that keeps running totals (pg_stat_statements) is dated by Culprit's scans: the first one bounds when a
  // statement that hasn't run since last ran.
  const firstScan = report.snapshot.workload && !report.snapshot.workload.windowed ? (report.snapshot.workload.timeline?.starts[0] ?? null) : null;

  const move = (delta: number) => {
    if (visible.length === 0) return;
    const index = Math.max(0, visible.findIndex((f) => f.id === current?.id));
    const next = visible[Math.min(visible.length - 1, Math.max(0, index + delta))];
    if (next) setState({ finding: next.id });
  };

  useHotkeys({
    j: () => move(1),
    k: () => move(-1),
    ArrowDown: () => move(1),
    ArrowUp: () => move(-1),
    "/": () => searchRef.current?.focus(),
    // `c` copies the selected finding's fix; `v` reads "recent vs baseline".
    ...(can(report.snapshot.server.engine, "periods") ? { v: () => setState({ changes: changeRequest() }) } : {}),
  }, !waits && !changes);

  useEffect(() => {
    listRef.current?.querySelector(".frow.selected")?.scrollIntoView({ block: "nearest" });
  }, [current?.id]);

  return (
    <div className="overview">
      <div className="ov-list">
        <Header report={report} onOpenWaits={openWaits} changes={changes} />
        <div className="filters">
          <button type="button" className={`chip ${category === "all" ? "on" : ""}`} onClick={() => setCategory("all")}>
            {t("ui.overview.all")} <span className="n">{counted.length}</span>
          </button>
          {categoryCounts(counted).map((c) => (
            <button type="button" key={c.category} className={`chip ${category === c.category ? "on" : ""}`} onClick={() => setCategory(c.category)}>
              {categoryLabel(c.category)} <span className="n">{c.count}</span>
            </button>
          ))}
          <label className="search" style={{ minWidth: 120, maxWidth: 180 }}>
            <Search size={13} />
            <input
              ref={searchRef}
              value={filter}
              placeholder={t("ui.overview.filter")}
              onChange={(e) => setFilter(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  setFilter("");
                  e.currentTarget.blur();
                }
              }}
            />
          </label>
        </div>
        <div className="list" ref={listRef}>
          <WorkloadUnlock report={report} />
          {visible.map((f, i) => (
            <div key={f.id}>
              {(!visible[i - 1] || actionLabel(f) !== actionLabel(visible[i - 1]!)) && (
                <div className="group-label eyebrow" >
                  {actionability(f) === "optional-improvement" ? t("ui.overview.optional_improvements") : actionLabel(f)}
                </div>
              )}
              <FindingRow finding={f} selected={f.id === current?.id} dismissed={!!dismissed[f.id]} firstScan={firstScan} />
            </div>
          ))}
          {visible.length === 0 && (
            <div className="empty" style={{ padding: 32 }}>
              <h3>{noOpportunities ? t("ui.overview.empty.no_opportunities") : report.findings.length === 0 ? t("ui.overview.empty.nothing") : t("ui.overview.empty.no_match")}</h3>
              <p>{noOpportunities ? t("ui.overview.empty.no_opportunities_detail") : report.findings.length === 0 ? t("ui.overview.empty.nothing_detail") : t("ui.overview.empty.no_match_detail")}</p>
            </div>
          )}
          {contextCount > 0 && (
            <button type="button" className="btn ghost" style={{ margin: "8px 4px" }} onClick={() => setShowContext((v) => !v)}>
              {showContext ? t("ui.overview.hide_context", { count: String(contextCount) }) : t("ui.overview.show_context", { count: String(contextCount) })}
            </button>
          )}
          {dismissedCount > 0 && (
            <button type="button" className="btn ghost" style={{ margin: "8px 4px" }} onClick={() => setShowDismissed((v) => !v)}>
              {showDismissed ? t("ui.overview.hide_dismissed", { count: String(dismissedCount) }) : t("ui.overview.show_dismissed", { count: String(dismissedCount) })}
            </button>
          )}
          {report.locked.length > 0 && <Locked report={report} />}
          <CheckedFine report={report} />
        </div>
      </div>
      {waits && (
        <Suspense fallback={null}>
          <WaitsPanel report={report} initialTab={waits.tab} initialCategory={waits.category} onClose={() => setWaits(null)} />
        </Suspense>
      )}
      {changes ? (
        <Suspense fallback={<div className="wc" />}>
          <WhatChanged report={report} request={changes} onClose={() => setState({ changes: null })} />
        </Suspense>
      ) : current ? (
        <FindingDetail key={current.id} report={report} finding={current} />
      ) : (
        <div className="empty">
          <h3>{t("ui.overview.select")}</h3>
        </div>
      )}
    </div>
  );
}

function Header({
  report,
  onOpenWaits,
  changes,
}: {
  report: Report;
  onOpenWaits: (tab: WaitsTab, category?: string) => void;
  changes: ChangeRequest | null;
}) {
  const { snapshot, summary } = report;
  const w = snapshot.workload;
  const compare = () => setState({ changes: changeRequest() });
  const end = w?.timeline ? Date.parse(w.timeline.end) : Date.now();
  const start = w?.timeline?.starts[0] ? Date.parse(w.timeline.starts[0]) : NaN;
  // Marks, schema and settings changes, restarts and cloud throttling on a lane under the chart.
  const log = useEvents(w?.timeline ? snapshot.connection : null, start, end);
  const events = useMemo(
    () =>
      chartEvents(log?.events ?? [], (r) =>
        setState({
          changes: r.to
            ? changeRequest({ recent_start: new Date(r.from).toISOString(), recent_end: new Date(r.to).toISOString() })
            : changeRequest({ since: new Date(r.from).toISOString() }),
        }),
      ),
    [log],
  );
  const flags: Flag[] = w ? planFlags(w, compareSince) : [];
  if (changes?.since) flags.push({ at: Date.parse(changes.since), label: t("ui.overview.since_flag"), tone: "anchor" });
  // Under 20 hours of history a "per day" figure would be an extrapolation: show what was measured, and over how long.
  const short = w != null && w.window_hours < 20;
  const span = !w
    ? ""
    : w.log != null
      ? windowUnknown(w)
        ? t("ui.overview.span.log", { log: w.log })
        : t("ui.overview.span.log_over", { log: w.log, span: compactSpan(w.window_hours) })
      : w.windowed
        ? byPeriod(w.window_hours, { day: key("ui.overview.span.last_days"), hour: key("ui.overview.span.last_hours"), minute: key("ui.overview.span.last_minutes") })
        : t("ui.overview.span.since_reset", { span: compactSpan(w.window_hours) });
  return (
    <div className="ov-head">
      <h2>
        {t("ui.overview.opportunities", { n: summary.opportunities, count: String(summary.opportunities) })}
      </h2>
      <p>{headlineTail(report)}</p>
      <div className="vitals">
        <div className="vital">
          <div className="label">{short ? t("ui.overview.db_time") : t("ui.overview.db_time_per_day")}</div>
          <div className="value">{w ? duration(short ? w.total_time_ms : perDay(w.total_time_ms, w.window_hours)) : "—"}</div>
          <div className="hint" title={w ? t("ui.overview.workload_title", { queryCount: w.query_count, queries: count(w.query_count), runCount: w.total_executions, runs: count(w.total_executions), span }) : undefined}>
            {w
              ? short
                ? span
                : t("ui.overview.workload_hint", { span, queryCount: w.query_count, queries: count(w.query_count), runCount: perDay(w.total_executions, w.window_hours), runs: count(perDay(w.total_executions, w.window_hours)) })
              : t("ui.overview.no_workload")}
          </div>
        </div>
        <WaitsVital report={report} onOpen={onOpenWaits} />
        <div className="vital">
          <div className="label">{t("common.metric.size")}</div>
          <div className="value">{bytes(snapshot.server.database_size_bytes)}</div>
          <div className="hint">
            {t("ui.overview.schema_counts", {
              tableCount: snapshot.tables.length,
              tables: String(snapshot.tables.length),
              indexCount: snapshot.indexes.length,
              indexes: String(snapshot.indexes.length),
            })}
          </div>
        </div>
      </div>
      <HealthLine report={report} onOpen={onOpenWaits} />
      {w?.timeline && w.timeline.starts.length >= 2 ? (
        <WorkloadTrend
          timeline={w.timeline}
          windowStart={w.window_start}
          flags={flags}
          onSelect={(range) =>
            setState({ changes: changeRequest({ recent_start: new Date(range.from).toISOString(), recent_end: new Date(range.to).toISOString() }) })
          }
          selection={requestedRange(changes, end)}
          onCompare={compare}
          events={events}
          actions={<MarkButton connection={snapshot.connection} />}
        />
      ) : w && trendNote(w) && can(snapshot.server.engine, "periods") ? (
        <p className="trend-note">
          {trendNote(w)}{" "}
          <button type="button" className="link-button" onClick={compare} title={t("ui.common.compare_periods", { key: "V" })}>
            <GitCompareArrows size={12} /> {t("ui.common.what_changed")}
          </button>
        </p>
      ) : null}
    </div>
  );
}

/** "2 min", "5 h", "3 days": short enough for a card's hint line. */
function compactSpan(hours: number): string {
  if (hours >= 47) {
    const days = Math.round(hours / 24);
    return t("ui.overview.span.days", { n: days, count: String(days) });
  }
  if (hours >= 1.5) return t("ui.overview.span.hours", { count: String(Math.round(hours)) });
  return t("ui.overview.span.minutes", { count: String(Math.max(1, Math.round(hours * 60))) });
}

function perDay(value: number, hours: number): number {
  return hours > 0 ? (value * 24) / hours : value;
}

function headlineTail(report: Report): string {
  const w = report.snapshot.workload;
  const share = report.summary.top_share;
  const span = w ? windowPhrase(w) : "";
  if (report.summary.opportunities === 0) return w ? t("ui.overview.headline.nothing_over", { span }) : t("ui.overview.headline.nothing");
  if (w && share != null && share >= 0.05) return t("ui.overview.headline.top_share", { share: percent(share), span });
  if (w) return t("ui.overview.headline.based_on", { source: w.source, span });
  return t("ui.overview.headline.no_workload");
}

/** When an inactive finding's statements last ran. Unchanged since Culprit's first scan, that scan is only the latest
 * they can have run: they may have stopped long before, so it isn't "last ran". */
function lastRan(lastSeen: string, firstScan: string | null): string {
  const first = firstScan != null ? Date.parse(firstScan) : NaN;
  if (Math.abs(first - Date.parse(lastSeen)) < 1000) return t("ui.overview.not_run_since_first_scan", { ago: ago(firstScan) });
  return t("ui.overview.last_ran", { ago: ago(lastSeen) });
}

function FindingRow({ finding, selected, dismissed, firstScan }: { finding: Finding; selected: boolean; dismissed: boolean; firstScan: string | null }) {
  const share = finding.impact.db_time_share;
  const fill = share != null ? Math.sqrt(share) : finding.impact.score / 100;
  return (
    <button type="button"
      className={`frow ${selected ? "selected" : ""} ${dismissed || !isActionable(finding) ? "dismissed" : ""}`}
      onClick={() => setState({ finding: finding.id, changes: null })}
    >
      <span className={`sev ${isActionable(finding) ? finding.severity : "low"}`} />
      <span className="title">{finding.title}</span>
      <span className="impact">
        <span className="v">{share != null ? percent(share) : ""}</span>
        <span className="bar">
          <span style={{ width: `${Math.max(4, Math.min(100, fill * 100))}%`, background: `var(--${isActionable(finding) ? finding.severity : "low"})` }} />
        </span>
      </span>
      <span className="meta">
        {finding.inactive && finding.last_seen ? `${lastRan(finding.last_seen, firstScan)} · ` : ""}
        {actionLabel(finding)} · {categoryLabel(finding.category)} · {finding.impact.label}
        {finding.subject.kind !== "query" ? ` · ${finding.subject.name}` : ""}
      </span>
    </button>
  );
}

function Locked({ report }: { report: Report }) {
  return (
    <div className="notice" style={{ margin: "12px 4px 4px" }}>
      <div>
        <b>{t("ui.overview.locked", { n: report.locked.length, count: String(report.locked.length) })}</b>{" "}
        {t("ui.overview.locked_needs", {
          n: report.locked.length,
          checks: report.locked.map((l) => l.check.title).join(", "),
          missing: [...new Set(report.locked.flatMap((l) => l.missing))].join(", "),
        })}{" "}
        {report.snapshot.server.missing[0]?.fix && <span className="muted">{report.snapshot.server.missing[0].reason}</span>}
      </div>
    </div>
  );
}
