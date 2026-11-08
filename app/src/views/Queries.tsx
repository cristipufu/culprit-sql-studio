import { ChevronLeft, ChevronRight, GitCompareArrows, Search, SquareArrowOutUpRight, SquarePen } from "lucide-react";
import { useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { LookToggle, PlanSurface, rememberLook, rememberedLook, type PlanLook } from "../components/PlanSurface";
import { LastActualRun, PlanSourceToggle, type PlanSource } from "../components/ActualRun";
import { measuresRuns } from "../lib/engines";
import { QueryTrend, Sparkline } from "../components/Trends";
import { RuntimeSpread } from "../components/RuntimeSpread";
import { QueryCostsPanel } from "../components/BeyondTime";
import { DocdbWork } from "../components/DocdbWork";
import { Sql } from "../components/Sql";
import type { Plan } from "../gen/Plan";
import type { QueryStat } from "../gen/QueryStat";
import type { Report } from "../gen/Report";
import { api, errorMessage, type ApiError } from "../lib/api";
import { canOpenQueryInEditor, prepareQueryForEditor } from "../lib/editor-sql";
import { count, duration, oneLine, percent, when } from "../lib/format";
import { useHotkeys } from "../lib/hotkeys";
import { t } from "../lib/i18n";
import { addPlan, compare } from "../lib/plans";
import { querySummary } from "../lib/query-summary";
import { figures, honest, workloadKind, windowPhrase, type Figures } from "../lib/rates";
import { setState, toast, useStore } from "../lib/store";
import { compareSince } from "./Overview";
import { remember, rememberedChoice } from "../lib/prefs";
import { tabListKeys } from "../lib/keys";
import { slot, withElements } from "./message-parts";

const PAGE_SIZE = 100;
type QueryEntry = { query: QueryStat; figures: Figures; search: string };

type SortKey = "share" | "runs" | "avg" | "max" | "rows" | "reads" | "cpu";

type ColumnKey = "runs" | "avg" | "max" | "rows" | "reads";

/** `optional` columns step aside on narrow panes (container queries) before the table would scroll sideways. */
const COLUMNS: { key: ColumnKey; optional?: boolean }[] = [
  { key: "runs" },
  { key: "avg" },
  { key: "max", optional: true },
  { key: "rows", optional: true },
  { key: "reads", optional: true },
];

/** A column's heading; YugabyteDB's reads are the storage entries it records. */
function columnLabel(key: ColumnKey, native: boolean): string {
  switch (key) {
    case "runs":
      return t("ui.queries.col.runs");
    case "avg":
      return t("ui.queries.col.avg");
    case "max":
      return t("ui.queries.col.max");
    case "rows":
      return t("ui.queries.col.rows");
    case "reads":
      return native ? t("ui.queries.col.entries") : t("ui.queries.col.reads");
  }
}

/** Verb, table and key predicate on one line ("SELECT orders · customer_id = @P1"); the statement on hover. */
function QueryCell({ q }: { q: QueryStat }) {
  const s = querySummary(q.text, q.object_name, q.resolved_write_target);
  return (
    <span className="qsum" title={oneLine(q.text)}>
      <b>{s.verb}</b>
      {s.target && <span className="qsum-table">{s.target}</span>}
      {s.predicate ? <span className="qsum-pred">{s.predicate}</span> : !s.target && <span className="qsum-pred">{oneLine(q.text)}</span>}
    </span>
  );
}

/** A column's value for sorting: share, runs and average as the report states them (`figures`: over Culprit's scans
 * where they measured the statement). */
function value(entry: QueryEntry, key: SortKey): number {
  const q = entry.query;
  switch (key) {
    case "share":
      return entry.figures.share;
    case "runs":
      return entry.figures.runs;
    case "avg":
      return entry.figures.mean;
    case "max":
      return q.max_time_ms ?? 0;
    case "rows":
      return q.executions > 0 ? (q.rows ?? 0) / q.executions : 0;
    case "reads":
      return readsPerRun(q) ?? 0;
    case "cpu":
      return q.cpu_time_ms ?? 0;
  }
}

function readsPerRun(query: QueryStat): number | null {
  const reads = query.costs?.docdb?.rows_scanned ?? query.logical_reads;
  return reads != null && query.executions > 0 ? reads / query.executions : null;
}

export function Queries({ report }: { report: Report }) {
  const workload = report.snapshot.workload;
  const native = report.snapshot.server.engine === "yugabyte";
  const selected = useStore((s) => s.query);
  const [sort, setSort] = useState<SortKey>("share");
  const [filter, setFilter] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  const tableRef = useRef<HTMLDivElement>(null);

  const deferredFilter = useDeferredValue(filter);
  const entries = useMemo<QueryEntry[]>(() => (workload?.queries ?? []).map((query) => ({
    query,
    figures: figures(report, query),
    search: `${query.text} ${query.id} ${query.object_name ?? ""}`.toLowerCase(),
  })), [report, workload]);
  const rows = useMemo(() => {
    const q = deferredFilter.trim().toLowerCase();
    return entries.filter((entry) => !q || entry.search.includes(q))
      .sort((a, b) => value(b, sort) - value(a, sort)).map((entry) => entry.query);
  }, [entries, sort, deferredFilter]);

  const currentIndex = Math.max(0, rows.findIndex((q) => q.id === selected));
  const current = rows[currentIndex] ?? null;
  const pageStart = Math.floor(currentIndex / PAGE_SIZE) * PAGE_SIZE;
  const pageRows = rows.slice(pageStart, pageStart + PAGE_SIZE);
  const regressed = useMemo(() => new Set(report.findings.filter((f) => f.check === "regression").flatMap((f) => f.queries.map((r) => r.id))), [report.findings]);

  const select = (index: number) => {
    const next = rows[Math.min(rows.length - 1, Math.max(0, index))];
    if (next) setState({ query: next.id });
  };
  const move = (delta: number) => select(currentIndex + delta);
  useHotkeys({ j: () => move(1), k: () => move(-1), ArrowDown: () => move(1), ArrowUp: () => move(-1),
    PageDown: () => move(PAGE_SIZE), PageUp: () => move(-PAGE_SIZE), Home: () => select(0), End: () => select(rows.length - 1),
    "/": () => searchRef.current?.focus() });

  useEffect(() => {
    tableRef.current?.querySelector("tr.selected")?.scrollIntoView({ block: "nearest" });
  }, [current?.id, pageStart, sort]);

  if (!workload) {
    return (
      <div className="empty">
        <h3>{t("ui.queries.no_history")}</h3>
        <p>{report.summary.no_workload ?? t("ui.queries.no_history_detail")}</p>
      </div>
    );
  }

  // Per-day only over windows long enough to mean it; measured totals otherwise. A burst, or fewer runs than days, is
  // its count, with when it ran (`figures`).
  const rates = honest(workload);

  return (
    <div className="split">
      <div className="pane">
        <div className="toolbar">
          <label className="search">
            <Search size={13} />
            <input ref={searchRef} value={filter} placeholder={t("ui.queries.filter")} onChange={(e) => setFilter(e.target.value)} />
          </label>
          <span className="spacer" />
          <span className="muted">
            {t("ui.queries.shown", { n: workload.query_count, shown: String(rows.length), total: count(workload.query_count), source: workload.source })}
          </span>
        </div>
        <div className="table-wrap" ref={tableRef}>
          <table className="grid">
            <thead>
              <tr>
                <th className={`sortable ${sort === "share" ? "sorted" : ""}`} onClick={() => setSort("share")}>
                  {t("ui.queries.col.share")}
                </th>
                <th className="col-query">{t("ui.queries.col.query")}</th>
                <th className="col-trend" title={t("ui.queries.col.trend_title")}>
                  {t("ui.queries.col.trend")}
                </th>
                {COLUMNS.map((c) => (
                  <th
                    key={c.key}
                    className={`r sortable ${sort === c.key ? "sorted" : ""} ${c.optional ? "col-optional" : ""}`}
                    onClick={() => setSort(c.key)}
                    title={c.key === "reads" && native ? t("ui.queries.col.entries_title") : c.key === "runs" && !rates.perDay ? t("ui.queries.col.runs_title", { suffix: rates.suffix }) : undefined}
                  >
                    {columnLabel(c.key, native)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {pageRows.map((q) => {
                // As the report states it: over Culprit's scans where they measured it.
                const f = figures(report, q);
                const share = f.share;
                return (
                  <tr key={q.id} className={`row ${q.id === current?.id ? "selected" : ""}`} onClick={() => setState({ query: q.id })}>
                    <td>
                      <span className="sharecell" title={f.over ? t("ui.common.over", { over: f.over }) : undefined}>
                        <span className="bar">
                          <span style={{ width: `${Math.max(2, Math.sqrt(share) * 100)}%`, background: share >= 0.15 ? "var(--high)" : share >= 0.03 ? "var(--medium)" : "var(--low)" }} />
                        </span>
                        <span className="num">{percent(share)}</span>
                      </span>
                    </td>
                    <td className="sqlcell col-query">
                      <QueryCell q={q} />
                    </td>
                    <td className="trendcell col-trend">
                      <Sparkline query={q} timeline={workload.timeline} alert={regressed.has(q.id)} />
                    </td>
                    <td className="r">
                      {f.said != null ? (
                        <span className={rates.perDay ? "muted" : undefined} title={f.said}>
                          {f.activity?.incomplete ? "≥ " : ""}
                          {f.activity ? t("ui.queries.runs_observed", { runs: count(f.runs) }) : rates.perDay ? t("ui.queries.runs_in_all", { runs: count(f.runs) }) : count(f.runs)}
                        </span>
                      ) : (
                        <span title={f.over ? t("ui.common.over", { over: f.over }) : undefined}>{f.perDay ? t("ui.queries.runs_per_day", { runs: count(f.runs) }) : count(f.runs)}</span>
                      )}
                    </td>
                    <td className="r">{duration(f.mean)}</td>
                    <td className="r col-optional">{duration(q.max_time_ms)}</td>
                    <td className="r col-optional">{q.rows != null && q.executions > 0 ? count(q.rows / q.executions) : "—"}</td>
                    <td className="r col-optional">{count(readsPerRun(q))}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
        {rows.length > PAGE_SIZE && (
          <nav className="toolbar query-pages" aria-label={t("ui.queries.pages")}>
            <span className="muted" aria-live="polite">{t("ui.queries.page_range", { from: pageStart + 1, to: Math.min(pageStart + PAGE_SIZE, rows.length), total: rows.length })}</span>
            <span className="spacer" />
            <button type="button" className="btn" disabled={pageStart === 0} onClick={() => select(pageStart - PAGE_SIZE)} aria-label={t("ui.queries.previous_page")}><ChevronLeft size={13} /> {t("ui.queries.previous")}</button>
            <button type="button" className="btn" disabled={pageStart + PAGE_SIZE >= rows.length} onClick={() => select(pageStart + PAGE_SIZE)} aria-label={t("ui.queries.next_page")}>{t("ui.queries.next")} <ChevronRight size={13} /></button>
          </nav>
        )}
      </div>
      <div className="pane">{current ? <QueryDetail key={`${report.snapshot.connection}:${report.snapshot.id}:${current.id}`} report={report} query={current} /> : <div className="empty">{t("ui.queries.no_match")}</div>}</div>
    </div>
  );
}

type DetailTab = "summary" | "plan" | "history";
const TAB_KEY = "culprit.query.tab";

function rememberedTab(): DetailTab {
  return rememberedChoice<DetailTab>(TAB_KEY, "summary", ["summary", "plan", "history"]);
}

/** Open Queries on this tab next time it shows a query (the dashboard opens a query on its plan). */
export function rememberQueryTab(tab: DetailTab): void {
  remember(TAB_KEY, tab);
}

function QueryDetail({ report, query }: { report: Report; query: QueryStat }) {
  const w = report.snapshot.workload!;
  // Share, runs and average as the report states them: over Culprit's scans where they measured it, with the totals
  // since the source began counting beside them where those differ twofold.
  const stated = figures(report, query);
  const share = stated.share;
  const rates = honest(w);
  // A burst, or fewer runs than days: its count and when it ran ("runs on Oct 11, 15:51–15:55 UTC").
  const ran = stated.said?.replace(/^\S+ /, "");
  const related = report.findings.filter((f) => f.queries.some((r) => r.id === query.id));
  const focus = related.flatMap((f) => f.queries.filter((r) => r.id === query.id).map((r) => r.plan_node)).find((n) => n != null) ?? null;
  const [tab, setTab] = useState<DetailTab>(rememberedTab);
  const [planId, setPlanId] = useState<string | null>(query.plan_history?.kind === "cache" ? null : query.plan_id ?? query.plans[0]?.id ?? null);
  const insights = query.plan?.insights?.items ?? [];
  const worst = insights.find((i) => i.severity === "high") ? "high" : insights.find((i) => i.severity === "medium") ? "medium" : insights.length ? "low" : null;

  const choose = (next: DetailTab) => {
    setTab(next);
    remember(TAB_KEY, next);
  };
  useHotkeys({ p: () => choose(tab === "plan" ? "summary" : "plan") });

  const tabs: { tab: DetailTab; label: string; extra?: React.ReactNode; hint?: string }[] = [
    { tab: "summary", label: t("ui.queries.tab.summary") },
    {
      tab: "plan",
      label: t("ui.queries.tab.plan"),
      hint: "P",
      extra: insights.length > 0 ? <span className={`tab-count ${worst}`}>{insights.length}</span> : null,
    },
    { tab: "history", label: t("ui.queries.tab.history"), extra: query.plans.length > 1 ? <span className="tab-count">{query.plans.length}</span> : null },
  ];

  return (
    <div className="detail query-detail">
      <div className="qd-head">
        <div className="crumbs">
          <span className="tag mono">{t("ui.queries.query_id", { id: query.id })}</span>
          {query.object_name && <span className="tag mono">{query.object_name}</span>}
          {query.plans.length > 1 && <span className="tag medium">{t("ui.queries.plan_count", { n: query.plans.length, count: String(query.plans.length) })}</span>}
          <span style={{ flex: 1 }} />
          {(() => {
            const editorSql = prepareQueryForEditor(query, report.snapshot.server.engine);
            return (
              canOpenQueryInEditor(query.text, editorSql) && (
                <button type="button" className="btn" onClick={() => setState({ view: "editor", editorSql })}>
                  <SquarePen size={13} /> {t("ui.common.open_in_editor")}
                </button>
              )
            );
          })()}
        </div>
        <div className="qd-tabs" role="tablist" aria-label={t("ui.queries.detail")} onKeyDown={tabListKeys}>
          {tabs.map((item) => (
            <button type="button" key={item.tab} role="tab" aria-selected={tab === item.tab} tabIndex={tab === item.tab ? 0 : -1} className={`qd-tab ${tab === item.tab ? "on" : ""}`} onClick={() => choose(item.tab)}>
              {item.label}
              {item.extra}
              {item.hint && <span className="kbd">{item.hint}</span>}
            </button>
          ))}
        </div>
      </div>
      <div className="qd-body">
        {tab === "summary" && (
          <>
            <section style={{ marginTop: 4 }}>
              <div className="metrics">
                <Metric value={percent(share)} label={t("ui.queries.metric.share")} />
                {stated.activity ? (
                  <Metric value={`${stated.activity.incomplete ? "≥ " : ""}${count(stated.runs)}`} label={t("ui.queries.metric.runs_observed")} />
                ) : ran != null ? (
                  <Metric value={count(stated.runs)} label={ran} />
                ) : stated.over ? (
                  <Metric value={count(stated.runs)} label={stated.perDay ? t("ui.queries.metric.runs_per_day") : rates.runsHeader.toLowerCase()} />
                ) : (
                  <Metric value={count(rates.value(query.executions))} label={t("ui.queries.metric.runs_over", { suffix: rates.suffix })} />
                )}
                <Metric value={duration(stated.mean)} label={t("ui.queries.metric.average")} />
                <Metric value={duration(query.max_time_ms)} label={t("ui.queries.metric.slowest")} />
                {query.rows != null && <Metric value={count(query.rows / Math.max(1, query.executions))} label={t("ui.queries.metric.rows")} />}
                {query.logical_reads != null && <Metric value={count(query.logical_reads / Math.max(1, query.executions))} label={report.snapshot.server.engine === "mysql" ? t("ui.queries.metric.rows_examined") : t("ui.queries.metric.pages_read")} />}
              </div>
              {stated.over && (
                <p className="muted">
                  {stated.activity
                    ? stated.totals
                      ? t("ui.queries.over.activity_totals", { over: stated.over, totals: stated.totals, window: windowPhrase(w) })
                      : t("ui.queries.over.activity", { over: stated.over, window: windowPhrase(w) })
                    : stated.totals
                      ? t("ui.queries.over.all_totals", { over: stated.over, totals: stated.totals, window: windowPhrase(w) })
                      : t("ui.queries.over.all", { over: stated.over, window: windowPhrase(w) })}
                </p>
              )}
              {stated.activity && (
                <p className="muted">
                  {stated.said}. {stated.activity.incomplete ? t("ui.queries.activity.evicted") : t("ui.queries.activity.short")}{" "}
                  {!stated.over && t("ui.queries.activity.cumulative", { window: windowPhrase(w) })}
                </p>
              )}
            </section>
            {insights.length > 0 && (
              <button type="button" className={`plan-teaser ${worst}`} onClick={() => choose("plan")}>
                <span className="pt-label">{t("ui.queries.in_the_plan")}</span>
                <span className="pt-title">{insights[0]!.title}</span>
                {insights.length > 1 && <span className="muted">{t("ui.common.more", { count: insights.length - 1 })}</span>}
                <span className="pt-go">
                  {t("ui.queries.open_the_plan")} <span className="kbd">P</span>
                </span>
              </button>
            )}
            {w.timeline && query.trend && <QueryTrend query={query} timeline={w.timeline} onSince={compareSince} />}
            <RuntimeSpread query={query} engine={report.snapshot.server.engine} windowed={w.windowed} product={report.snapshot.server.product} kind={workloadKind(w)} />
            {query.requests && <section>
              <h3>{t("ui.queries.requests.heading")}</h3>
              <p>{t("ui.queries.requests.summary", { requests: count(query.requests.requests), repeated: count(query.requests.repeated_requests), calls: count(query.requests.most_calls) })}</p>
              <p className="muted">{query.requests.sample_truncated ? t("ui.queries.requests.note_partial") : t("ui.queries.requests.note")}</p>
            </section>}
            <QueryCostsPanel report={report} query={query} />
            <DocdbWork metrics={query.costs?.docdb} basis="workload" runs={query.executions} />
            {related.length > 0 && (
              <section>
                <h3>{t("ui.queries.findings")}</h3>
                {related.map((f) => (
                  <button type="button" key={f.id} className="frow" onClick={() => setState({ view: "overview", finding: f.id })}>
                    <span className={`sev ${f.severity}`} />
                    <span className="title">{f.title}</span>
                    <span />
                  </button>
                ))}
              </section>
            )}
            <section>
              <h3>{t("ui.queries.statement")}</h3>
              <div className="codeblock">
                <Sql text={query.text} />
              </div>
            </section>
          </>
        )}
        {tab === "plan" && <QueryPlans key={query.id} report={report} query={query} focus={focus} planId={planId} setPlanId={setPlanId} />}
        {tab === "history" && <PlanHistory query={query} planId={planId} onOpen={(id) => (setPlanId(id), choose("plan"))} />}
      </div>
    </div>
  );
}

function Metric({ value, label }: { value: string; label: string }) {
  return (
    <div className="metric">
      <div className="value">{value}</div>
      <div className="label">{label}</div>
    </div>
  );
}

/** The plans the engine kept for this query, best first by average time, and when each was used. */
function PlanHistory({ query, planId, onOpen }: { query: QueryStat; planId: string | null; onOpen: (id: string) => void }) {
  if (query.plans.length === 0) {
    return <div className="empty" style={{ padding: 30 }}>{query.plan_history ? t("ui.queries.history.none_retained", { note: query.plan_history.note }) : t("ui.queries.history.none")}</div>;
  }
  const best = Math.min(...query.plans.map((v) => v.mean_time_ms));
  return (
    <section>
      <h3>
        {t("ui.queries.history.heading")} <span className="muted">{query.plans.length}</span>
      </h3>
      {query.plan_history && <p className="muted">{query.plan_history.source}. {query.plan_history.note}</p>}
      <table className="grid history-table">
        <thead>
          <tr>
            <th>{t("ui.queries.history.plan")}</th>
            <th className="r">{t("ui.queries.history.average")}</th>
            <th className="r">{t("ui.queries.history.runs")}</th>
            <th>{t("ui.queries.history.first_seen")}</th>
            <th>{t("ui.queries.history.last_seen")}</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {query.plans.map((v, i) => (
            <tr key={v.id} className={`row ${planId === v.id ? "selected" : ""}`} onClick={() => onOpen(v.id)}>
              <td>
                <b style={query.plan_history?.kind === "cache" ? { display: "block" } : undefined}>{t("ui.queries.plan_name", { id: v.id })}</b>
                {i === 0 && <span className="tag" style={{ marginLeft: query.plan_history?.kind === "cache" ? 0 : 6 }}>{query.plan_history?.kind === "cache" ? t("ui.queries.most_retained_time") : t("ui.queries.most_time")}</span>}
                {v.is_forced && <span className="tag medium" style={{ marginLeft: 6 }}>{t("ui.queries.forced")}</span>}
              </td>
              <td className={`r ${query.plan_history?.kind !== "cache" && query.plans.length > 1 && v.mean_time_ms > best * 2 ? "slow" : ""}`}>{duration(v.mean_time_ms)}</td>
              <td className="r">{count(v.executions)}</td>
              <td className="muted" title={v.first_seen ? seenAt(v.first_seen).full : undefined}>
                {v.first_seen ? seenAt(v.first_seen).short : "—"}
              </td>
              <td className="muted" title={v.last_seen ? seenAt(v.last_seen).full : undefined}>
                {v.last_seen ? seenAt(v.last_seen).short : "—"}
              </td>
              <td className="r">
                <span className="link">{t("ui.common.open_plan")} →</span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

type PlanState = { status: "idle" } | { status: "loading" } | { status: "ready"; plan: Plan } | { status: "error"; error: ApiError };

/** Every plan the engine kept for the query, its numbers, and the selected one drawn as a diagram or a tree. */
function QueryPlans(props: { report: Report; query: QueryStat; focus: number | null; planId: string | null; setPlanId: (id: string | null) => void }) {
  const { report, query, focus, planId, setPlanId } = props;
  const variants = query.plans;
  const cachedHistory = query.plan_history?.kind === "cache";
  const planQuery = cachedHistory && planId !== null ? undefined : query;
  const dominant = variants[0]?.id ?? null;
  const capturedId = query.plan_id ?? (query.plan_history?.kind !== "cache" && variants.length === 1 ? dominant : null);
  const [look, setLook] = useState<PlanLook>(rememberedLook);
  const [source, setSource] = useState<PlanSource>("plan");
  const connection = report.snapshot.connection;
  const selection = JSON.stringify([connection, report.snapshot.id, query.id, planId]);
  const capturedPlan = planId != null && query.plan_history?.captured_plan_id === planId && query.plan_history.captured_plan
    ? query.plan_history.captured_plan
    : query.plan && (planId === capturedId || (planId === null && (variants.length === 0 || cachedHistory))) ? query.plan : null;
  const initial: PlanState = capturedPlan ? { status: "ready", plan: capturedPlan } : { status: "loading" };
  const [loaded, setLoaded] = useState<{ selection: string; state: PlanState }>({ selection, state: initial });
  const state = loaded.selection === selection ? loaded.state : initial;

  useEffect(() => {
    const setPlan = (state: PlanState) => setLoaded({ selection, state });
    if (capturedPlan) {
      setPlan({ status: "ready", plan: capturedPlan });
      return;
    }
    let cancelled = false;
    setPlan({ status: "loading" });
    api()
      .then((client) => client.queryPlan(connection, query.id, planId, report.snapshot.id))
      .then((plan) => !cancelled && setPlan({ status: "ready", plan }))
      .catch((e) => !cancelled && setPlan({ status: "error", error: errorMessage(e) }));
    return () => {
      cancelled = true;
    };
  }, [planId, query, capturedId, connection, variants.length, report.snapshot.id, selection, capturedPlan]);

  const chooseLook = (next: PlanLook) => {
    setLook(next);
    rememberLook(next);
  };
  const best = variants.length > 1 ? Math.min(...variants.map((v) => v.mean_time_ms)) : null;
  const [comparing, setComparing] = useState(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const origin = t("ui.queries.origin", { connection, id: query.id });
  const nameOf = (id: string | null) => (id ? t("ui.queries.plan_name", { id }) : t("ui.queries.query_name", { id: query.id }));

  /** The dominant plan next to the selected one (or the runner-up when the dominant one is selected), faster one first. */
  const compareVariants = async () => {
    const other = planId && planId !== dominant ? planId : variants[1]?.id;
    if (!dominant || !other) return;
    setComparing(true);
    try {
      const client = await api();
      const load = (id: string) =>
        id === query.plan_history?.captured_plan_id && query.plan_history.captured_plan ? Promise.resolve(query.plan_history.captured_plan) : id === capturedId && query.plan ? Promise.resolve(query.plan) : state.status === "ready" && id === planId ? Promise.resolve(state.plan) : client.queryPlan(connection, query.id, id, report.snapshot.id);
      const [first, second] = await Promise.all([load(dominant), load(other)]);
      if (!mounted.current) return;
      // The faster plan is the baseline, so the comparison reads "the slow plan does X where the fast one does Y".
      const speed = (id: string) => variants.find((v) => v.id === id)?.mean_time_ms ?? 0;
      const a = { plan: first, name: nameOf(dominant), origin, source: { connection, queryId: query.id, planId: dominant }, query: query.plan_history?.kind === "cache" ? undefined : { ...query, plan_id: dominant } };
      const b = { plan: second, name: nameOf(other), origin, source: { connection, queryId: query.id, planId: other }, query: query.plan_history?.kind === "cache" ? undefined : { ...query, plan_id: other } };
      if (speed(dominant) <= speed(other)) compare(a, b);
      else compare(b, a);
    } catch (e) {
      toast(errorMessage(e).message);
    } finally {
      setComparing(false);
    }
  };

  return (
    <section>
      <h3 className="plan-bar">
        {variants.length > 1 ? (
          <>
            {t("ui.queries.plans")} <span className="muted">{variants.length}</span>
          </>
        ) : (
          <span className="muted plan-source">{query.plan?.kind === "actual" ? t("ui.queries.actual_plan") : report.snapshot.server.engine === "mssql" ? (report.snapshot.workload?.source ?? t("ui.queries.tab.plan")) : t("ui.queries.estimated_plan")}</span>
        )}
        <span className="actions">
          {variants.length > 1 && (
            <button type="button" className="btn ghost" disabled={comparing} title={t("ui.queries.compare_title")} onClick={compareVariants}>
              {comparing ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <GitCompareArrows size={13} />} {t("ui.queries.compare")}
            </button>
          )}
          {state.status === "ready" && (
            <button type="button" className="btn ghost" title={t("ui.queries.open_in_plans_title")} onClick={() => addPlan(state.plan, nameOf(planId), origin, { source: { connection, queryId: query.id, planId }, query: planQuery ? { ...planQuery, plan_id: planId } : undefined })}>
              <SquareArrowOutUpRight size={13} /> {t("ui.queries.open_in_plans")}
            </button>
          )}
          {measuresRuns(report.snapshot.server.engine) && <PlanSourceToggle value={source} onChange={setSource} />}
          <LookToggle look={look} onChange={chooseLook} />
        </span>
      </h3>
      {cachedHistory && <p className="muted">{query.plan_history!.source}. {query.plan_history!.note}</p>}
      {(variants.length > 1 || (cachedHistory && variants.length > 0)) && (
        <div className="plan-variants">
          {cachedHistory && <button type="button" className={`variant ${planId === null ? "on" : ""}`} onClick={() => setPlanId(null)}>
            <span className="v-name">{t("ui.queries.current_estimate")}</span>
            <span className="v-stats">{t("ui.queries.current_estimate_detail")}</span>
          </button>}
          {variants.map((v, i) => (
            <button type="button" key={v.id} className={`variant ${planId === v.id ? "on" : ""}`} onClick={() => setPlanId(v.id)}>
              <span className="v-name">
                {t("ui.queries.plan_name", { id: v.id })}
                {i === 0 && <span className="tag" style={{ marginLeft: 6 }}>{cachedHistory ? t("ui.queries.most_retained_time") : t("ui.queries.most_time")}</span>}
                {v.is_forced && <span className="tag medium" style={{ marginLeft: 6 }}>{t("ui.queries.forced")}</span>}
              </span>
              <span className="v-stats">
                {withElements(t("ui.queries.variant_stats", { duration: slot(0), runs: count(v.executions) }), [
                  <b key="avg" className={!cachedHistory && best != null && v.mean_time_ms > best * 2 ? "slow" : ""}>{duration(v.mean_time_ms)}</b>,
                ])}
                {v.last_seen && (
                  <span className="muted" title={seenAt(v.last_seen).full}>
                    {" "}
                    · {t("ui.queries.last_seen", { time: seenAt(v.last_seen).short })}
                  </span>
                )}
              </span>
            </button>
          ))}
        </div>
      )}
      {source === "actual" && <LastActualRun connection={connection} engine={report.snapshot.server.engine} query={query} planId={planId} reportId={report.snapshot.id} look={look} />}
      {source === "plan" && state.status === "loading" && (
        <div className="empty" style={{ padding: 30 }}>
          <div className="spinner" />
          {t("ui.queries.loading_plan")}
        </div>
      )}
      {source === "plan" && state.status === "error" && (
        <div className="notice">
          <div>
            <b>{t("ui.queries.no_plan")}</b> {state.error.message}
            {state.error.hint && <div className="muted">{state.error.hint}</div>}
          </div>
        </div>
      )}
      {source === "plan" && state.status === "ready" && <PlanSurface key={`${query.id}-${planId}`} plan={state.plan} query={planQuery ? { ...planQuery, plan_id: planId } : undefined} focus={planId === capturedId ? focus : null} look={look} />}
    </section>
  );
}

/** A first or last seen time as people say it ("today 3:55 PM"); sources that write no zone mean UTC. */
function seenAt(text: string): { short: string; full: string } {
  const iso = /[zZ]$|[+-]\d\d:?\d\d$/.test(text) ? text : `${text.replace(" ", "T")}Z`;
  return when(iso);
}
