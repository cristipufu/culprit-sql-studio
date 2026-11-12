import { ArrowUpRight, Check, ChevronRight, Copy, Info, Layers, Rows3, Table2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { AdviceQuery } from "../gen/AdviceQuery";
import type { CleanupKind } from "../gen/CleanupKind";
import type { IndexAdvice } from "../gen/IndexAdvice";
import type { IndexCandidate } from "../gen/IndexCandidate";
import type { IndexCleanup } from "../gen/IndexCleanup";
import type { Report } from "../gen/Report";
import type { TableAdvice } from "../gen/TableAdvice";
import { api, errorMessage, type ApiError } from "../lib/api";
import { ago, bytes, count, percent, rowCount } from "../lib/format";
import { useHotkeys } from "../lib/hotkeys";
import { language as selectedLanguage, t, useLanguage } from "../lib/i18n";
import { setState } from "../lib/store";
import { Sql } from "./Sql";
import { copyAndTell } from "../lib/clipboard";

type Load = { status: "loading" } | { status: "ready"; advice: IndexAdvice } | { status: "error"; error: ApiError };
type Section = "add" | "cleanup" | "tables";
type Item =
  | { kind: "add"; key: string; candidate: IndexCandidate; rank: number }
  | { kind: "cleanup"; key: string; cleanup: IndexCleanup }
  | { kind: "tables"; key: string; table: TableAdvice };

/** Advice belongs to a connection, report and language. */
const cache = new Map<string, IndexAdvice>();

/** Where a candidate index comes from, in words. */
function sourceLabel(source: IndexCandidate["source"]): string {
  switch (source) {
    case "optimizer":
      return t("ui.index_advisor.source.optimizer");
    case "plan-scan":
      return t("ui.index_advisor.source.plan_scan");
    case "foreign-key":
      return t("ui.index_advisor.source.foreign_key");
    case "engine-built":
      return t("ui.index_advisor.source.engine_built");
  }
}

/** A cleanup's badge: its kind in words, and its tone. */
function cleanupKind(kind: CleanupKind): { label: string; tone: string } {
  switch (kind) {
    case "duplicate":
      return { label: t("ui.index_advisor.cleanup.duplicate"), tone: "medium" };
    case "prefix":
      return { label: t("ui.index_advisor.cleanup.prefix"), tone: "low" };
    case "overlap":
      return { label: t("ui.index_advisor.cleanup.overlap"), tone: "low" };
    case "unused":
      return { label: t("ui.index_advisor.cleanup.unused"), tone: "medium" };
  }
}

/** What a table's shape costs, in words, with its icon. */
function tableKind(kind: TableAdvice["kind"]): { label: string; icon: ReactNode } {
  switch (kind) {
    case "scan-heavy":
      return { label: t("ui.index_advisor.table.scan_heavy"), icon: <Rows3 size={14} /> };
    case "forwarded-fetches":
      return { label: t("ui.index_advisor.table.forwarded_fetches"), icon: <Layers size={14} /> };
    case "over-indexed":
      return { label: t("ui.index_advisor.table.over_indexed"), icon: <Table2 size={14} /> };
  }
}

function sqlOf(item: Item): string | null {
  if (item.kind === "add") return item.candidate.sql;
  if (item.kind === "cleanup") return item.cleanup.sql;
  return item.table.sql;
}

function findingOf(item: Item): string | null {
  if (item.kind === "add") return item.candidate.finding_id;
  if (item.kind === "cleanup") return item.cleanup.finding_id;
  return item.table.finding_id;
}

function tableOf(item: Item): string {
  if (item.kind === "add") return `${item.candidate.schema}.${item.candidate.table}`;
  if (item.kind === "cleanup") return `${item.cleanup.schema}.${item.cleanup.table}`;
  return `${item.table.schema}.${item.table.table}`;
}

/**
 * The index advisor: one ranked plan for the database. Indexes to add are ordered by the database time of the queries
 * that need them (not by the optimizer's own impact guess), each with those queries, the requests it merges, copy-only
 * DDL, its write cost and how to verify it. Then indexes to clean up and tables whose shape costs reads or writes.
 */
export default function IndexAdvisor({ report, onOpenTable }: { report: Report; onOpenTable: (key: string) => void }) {
  const id = report.snapshot.id;
  const language = useLanguage();
  const key = JSON.stringify([report.snapshot.connection, id, language]);
  const [loaded, setLoaded] = useState<{ key: string; value: Load }>(() => ({ key, value: cache.has(key) ? { status: "ready", advice: cache.get(key)! } : { status: "loading" } }));
  const load: Load = loaded.key === key ? loaded.value : cache.has(key) ? { status: "ready", advice: cache.get(key)! } : { status: "loading" };
  const [filter, setFilter] = useState<Section | null>(null);
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [focus, setFocus] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const ready = (advice: IndexAdvice) => {
      setLoaded({ key, value: { status: "ready", advice } });
      const first = advice.candidates[0] ? "add:0" : advice.cleanups[0] ? "cleanup:0" : null;
      setOpen(new Set(first ? [first] : []));
      setFocus(0);
      setFilter(null);
    };
    const cached = cache.get(key);
    if (cached) {
      ready(cached);
      return;
    }
    let live = true;
    const ownsRequest = () => live && selectedLanguage() === language;
    setLoaded({ key, value: { status: "loading" } });
    api()
      .then((client) => ownsRequest() ? client.indexAdvice(report.snapshot.connection, id) : null)
      .then((advice) => {
        if (!advice || !ownsRequest()) return;
        cache.set(key, advice);
        ready(advice);
      })
      .catch((e) => ownsRequest() && setLoaded({ key, value: { status: "error", error: errorMessage(e) } }));
    return () => {
      live = false;
    };
  }, [id, key, language, report.snapshot.connection]);

  const advice = load.status === "ready" ? load.advice : null;
  const items = useMemo<Item[]>(() => {
    if (!advice) return [];
    const out: Item[] = [];
    if (!filter || filter === "add") advice.candidates.forEach((candidate, i) => out.push({ kind: "add", key: `add:${i}`, candidate, rank: i + 1 }));
    if (!filter || filter === "cleanup") advice.cleanups.forEach((cleanup, i) => out.push({ kind: "cleanup", key: `cleanup:${i}`, cleanup }));
    if (!filter || filter === "tables") advice.tables.forEach((table, i) => out.push({ kind: "tables", key: `tables:${i}`, table }));
    return out;
  }, [advice, filter]);

  const toggle = (key: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const copy = async (sql: string | null) => {
    if (!sql) return;
    await copyAndTell(sql, t("ui.index_advisor.copied"));
  };
  const openFinding = (fid: string | null) => fid && setState({ view: "overview", finding: fid });
  const openQuery = (q: AdviceQuery) => setState({ view: "queries", query: q.id });

  const current = items[Math.min(focus, items.length - 1)];
  useHotkeys({
    j: () => setFocus((f) => Math.min(items.length - 1, f + 1)),
    k: () => setFocus((f) => Math.max(0, f - 1)),
    ArrowDown: () => setFocus((f) => Math.min(items.length - 1, f + 1)),
    ArrowUp: () => setFocus((f) => Math.max(0, f - 1)),
    Enter: () => current && toggle(current.key),
    c: () => current && void copy(sqlOf(current)),
    o: () => current && openFinding(findingOf(current)),
    t: () => current && onOpenTable(tableOf(current)),
  });

  useEffect(() => {
    listRef.current?.querySelector(".ia-item.focus")?.scrollIntoView({ block: "nearest" });
  }, [focus, filter]);

  if (load.status === "loading") {
    return (
      <div className="ia" aria-busy="true">
        <Header advice={null} report={report} />
        <div className="ia-tiles">
          {[0, 1, 2].map((i) => (
            <div key={i} className="ia-tile ia-skel" />
          ))}
        </div>
        {[0, 1, 2].map((i) => (
          <div key={i} className="ia-skel ia-skel-row" />
        ))}
      </div>
    );
  }
  if (load.status === "error") {
    return (
      <div className="ia">
        <Header advice={null} report={report} />
        <div className="ia-note">
          <Info size={14} />
          <div>
            <b>{load.error.message}</b>
            {load.error.hint && <div className="muted">{load.error.hint}</div>}
          </div>
        </div>
      </div>
    );
  }

  const a = load.advice;
  const neededShare = a.candidates.reduce((sum, c) => sum + (c.db_time_share ?? 0), 0);
  const freed = a.cleanups.reduce((sum, c) => sum + (c.size_bytes ?? 0), 0);
  const empty = a.candidates.length + a.cleanups.length + a.tables.length === 0;
  const indexCount = report.snapshot.indexes.length;
  const checkedArgs = { indexCount, indexes: count(indexCount), tableCount: report.snapshot.tables.length, tables: count(report.snapshot.tables.length) };
  const requests = report.snapshot.server.capabilities.includes("missing-indexes");
  const usage = report.snapshot.server.capabilities.includes("index-usage");

  return (
    <div className="ia">
      <Header advice={a} report={report} />
      {empty ? (
        <div className="ia-empty">
          <span className="ia-empty-mark">
            <Check size={18} />
          </span>
          <h3>{t("ui.index_advisor.empty")}</h3>
          <p>
            {requests
              ? usage
                ? t("ui.index_advisor.checked_requests_unused", checkedArgs)
                : t("ui.index_advisor.checked_requests", checkedArgs)
              : usage
                ? t("ui.index_advisor.checked_unused", checkedArgs)
                : t("ui.index_advisor.checked", checkedArgs)}
          </p>
        </div>
      ) : (
        <div className="ia-tiles" role="tablist" aria-label={t("ui.index_advisor.show")}>
          <Tile on={filter === "add"} n={a.candidates.length} label={t("ui.index_advisor.tile.add")} sub={neededShare > 0 ? t("ui.index_advisor.tile.needed_by", { share: percent(Math.min(1, neededShare)) }) : a.candidates.length ? t("ui.index_advisor.tile.from_plans") : t("ui.index_advisor.tile.nothing_missing")} onClick={() => (setFilter((f) => (f === "add" ? null : "add")), setFocus(0))} />
          <Tile on={filter === "cleanup"} n={a.cleanups.length} label={t("ui.index_advisor.tile.cleanup")} sub={freed > 0 ? t("ui.index_advisor.tile.frees", { size: bytes(freed) }) : t("ui.index_advisor.tile.nothing_idle")} onClick={() => (setFilter((f) => (f === "cleanup" ? null : "cleanup")), setFocus(0))} />
          <Tile on={filter === "tables"} n={a.tables.length} label={t("ui.index_advisor.tile.tables", { n: a.tables.length })} sub={a.tables.length ? t("ui.index_advisor.tile.shape_costs") : t("ui.index_advisor.tile.all_in_shape")} onClick={() => (setFilter((f) => (f === "tables" ? null : "tables")), setFocus(0))} />
        </div>
      )}

      {a.notes.map((note) => (
        <div key={note} className="ia-note">
          <Info size={14} />
          <span>{note}</span>
        </div>
      ))}

      <div className="ia-list" ref={listRef}>
        {(["add", "cleanup", "tables"] as Section[]).map((section) => {
          const list = items.filter((i) => i.kind === section);
          if (!list.length) return null;
          return (
            <section key={section} className="ia-section">
              <h3>
                {section === "add" ? t("ui.index_advisor.section.add") : section === "cleanup" ? t("ui.index_advisor.section.cleanup") : t("ui.index_advisor.section.tables")}
                <span className="ia-count">{list.length}</span>
                {section === "add" && <span className="ia-h-note">{t("ui.index_advisor.section.add_note")}</span>}
              </h3>
              {list.map((item) => {
                const index = items.indexOf(item);
                const props = {
                  focused: index === focus,
                  onFocus: () => setFocus(index),
                  expanded: open.has(item.key),
                  onToggle: () => (toggle(item.key), setFocus(index)),
                  onCopy: () => void copy(sqlOf(item)),
                  onFinding: () => openFinding(findingOf(item)),
                  onTable: () => onOpenTable(tableOf(item)),
                  onQuery: openQuery,
                };
                if (item.kind === "add") return <CandidateRow key={item.key} c={item.candidate} rank={item.rank} {...props} />;
                if (item.kind === "cleanup") return <CleanupRow key={item.key} c={item.cleanup} {...props} />;
                return <TableRow key={item.key} table={item.table} {...props} />;
              })}
            </section>
          );
        })}
      </div>
      {!empty && (
        <div className="ia-keys muted">
          <span className="kbd">j</span>
          <span className="kbd">k</span> {t("ui.common.keys.move")} · <span className="kbd">↵</span> {t("ui.common.keys.open")} · <span className="kbd">c</span> {t("ui.index_advisor.keys.copy")} · <span className="kbd">o</span> {t("ui.index_advisor.keys.finding")} · <span className="kbd">t</span> {t("ui.index_advisor.keys.table")}
        </div>
      )}
    </div>
  );
}

function Header({ advice, report }: { advice: IndexAdvice | null; report: Report }) {
  return (
    <header className="ia-head">
      <h2>{t("ui.index_advisor.title")}</h2>
      <p className="ia-sub">
        {t("ui.index_advisor.sub", {
          state: advice ? (advice.counters ? t("ui.index_advisor.usage_counted", { counters: advice.counters }) : t("ui.index_advisor.no_usage_counters")) : t("ui.index_advisor.loading"),
          ago: ago(report.snapshot.taken_at),
        })}
      </p>
    </header>
  );
}

function Tile({ on, n, label, sub, onClick }: { on: boolean; n: number; label: string; sub: string; onClick: () => void }) {
  return (
    <button type="button" role="tab" aria-selected={on} className={`ia-tile ${on ? "on" : ""} ${n === 0 ? "zero" : ""}`} onClick={onClick} disabled={n === 0}>
      <span className="ia-tile-n">{n}</span>
      <span className="ia-tile-label">{label}</span>
      <span className="ia-tile-sub">{sub}</span>
    </button>
  );
}

type RowProps = {
  focused: boolean;
  onFocus: () => void;
  expanded: boolean;
  onToggle: () => void;
  onCopy: () => void;
  onFinding: () => void;
  onTable: () => void;
  onQuery: (q: AdviceQuery) => void;
};

function Keys({ keys, include, shared = 0 }: { keys: string[]; include?: string[]; shared?: number }) {
  return (
    <span className="ia-keys-chips">
      {keys.map((k, i) => (
        <span key={`${k}-${i}`} className={`ia-chip ${i < shared ? "shared" : ""}`}>
          {k}
        </span>
      ))}
      {include && include.length > 0 && <span className="ia-chip include" title={t("ui.index_advisor.included_title", { columns: include.join(", ") })}>+ {include.length === 1 ? include[0] : t("ui.index_advisor.included", { n: include.length })}</span>}
    </span>
  );
}

function Body({ expanded, children }: { expanded: boolean; children: ReactNode }) {
  return (
    <div className={`ia-body ${expanded ? "open" : ""}`}>
      <div className="ia-body-inner">{expanded && children}</div>
    </div>
  );
}

function SqlBlock({ sql, onCopy }: { sql: string | null; onCopy: () => void }) {
  if (!sql) return null;
  return (
    <div className="ia-sql">
      <div className="ia-sql-bar">
        <span className="muted">{t("ui.index_advisor.run_yourself")}</span>
        <button type="button" className="btn ghost" onClick={onCopy}>
          <Copy size={12} /> {t("ui.common.copy")} <span className="kbd">c</span>
        </button>
      </div>
      <Sql text={sql} pretty={false} className="ia-sql-code" />
    </div>
  );
}

function Queries({ queries, onQuery, title }: { queries: AdviceQuery[]; onQuery: (q: AdviceQuery) => void; title: string }) {
  if (!queries.length) return null;
  const max = Math.max(0.0001, ...queries.map((q) => q.share ?? 0));
  return (
    <div className="ia-block">
      <div className="ia-label">{title}</div>
      {queries.map((q) => (
        <button type="button" key={q.id} className="ia-query" onClick={() => onQuery(q)} title={t("ui.common.open_in_queries")}>
          <span className="ia-qbar" aria-hidden>
            <span style={{ width: `${Math.max(4, ((q.share ?? 0) / max) * 100)}%` }} />
          </span>
          <span className="ia-qshare">{q.share != null ? percent(q.share) : "—"}</span>
          <span className="ia-qtext">{q.text}</span>
          {q.runs_per_day != null && <span className="ia-qruns">{t("ui.index_advisor.per_day", { count: count(q.runs_per_day) })}</span>}
          <ArrowUpRight size={12} className="ia-go" />
        </button>
      ))}
    </div>
  );
}

function Verify({ steps }: { steps: string[] }) {
  if (!steps.length) return null;
  return (
    <div className="ia-block">
      <div className="ia-label">{t("ui.index_advisor.verify")}</div>
      <ol className="ia-verify">
        {steps.map((s) => (
          <li key={s}>{s}</li>
        ))}
      </ol>
    </div>
  );
}

function Links({ onFinding, hasFinding, onTable }: { onFinding: () => void; hasFinding: boolean; onTable: () => void }) {
  return (
    <div className="ia-links">
      {hasFinding && (
        <button type="button" className="btn ghost" onClick={onFinding}>
          {t("ui.index_advisor.open_finding")} <span className="kbd">o</span>
        </button>
      )}
      <button type="button" className="btn ghost" onClick={onTable}>
        {t("ui.index_advisor.open_table")} <span className="kbd">t</span>
      </button>
    </div>
  );
}

function CandidateRow({ c, rank, focused, onFocus, expanded, onToggle, onCopy, onFinding, onTable, onQuery }: RowProps & { c: IndexCandidate; rank: number }) {
  const facts: string[] = [];
  if (c.lookups != null) facts.push(t("ui.index_advisor.lookups", { n: c.lookups, count: count(c.lookups) }));
  if (c.estimated_improvement != null) facts.push(t("ui.index_advisor.cheaper", { share: percent(c.estimated_improvement) }));
  if (c.table_rows != null) facts.push(t("ui.index_advisor.in_table", { rows: rowCount(c.table_rows) }));
  if (c.writes_per_day != null) facts.push(t("ui.index_advisor.writes_per_day", { n: c.writes_per_day, count: count(c.writes_per_day) }));
  return (
    <div className={`ia-item ${focused ? "focus" : ""} ${expanded ? "expanded" : ""}`}>
      <button type="button" className="ia-row" onClick={onToggle} onFocus={onFocus} aria-expanded={expanded}>
        <span className="ia-rank">{rank}</span>
        <span className="ia-main">
          <span className="ia-title">
            <span className="ia-table">{c.table}</span>
            <Keys keys={c.keys} include={c.include} />
          </span>
          <span className="ia-meta">
            <span className={`ia-source ${c.source}`}>{sourceLabel(c.source)}</span>
            {facts.length > 0 && <span> · {facts.join(" · ")}</span>}
            {c.widens && <span> · {t("ui.index_advisor.widens", { index: c.widens })}</span>}
          </span>
        </span>
        <span className="ia-impact" title={t("ui.index_advisor.impact_hint")}>
          {c.db_time_share != null ? (
            <>
              <span className="ia-meter" aria-hidden>
                <span style={{ width: `${Math.max(3, Math.sqrt(c.db_time_share) * 100)}%` }} />
              </span>
              <span className="ia-impact-n">{percent(c.db_time_share)}</span>
              <span className="ia-impact-l">{t("ui.index_advisor.of_db_time")}</span>
            </>
          ) : (
            <span className="ia-impact-l">{c.queries.length ? t("ui.index_advisor.queries", { n: c.queries.length }) : t("ui.index_advisor.no_queries")}</span>
          )}
        </span>
        <ChevronRight size={14} className="ia-chev" />
      </button>
      <Body expanded={expanded}>
        <p className="ia-reason">{c.reason}</p>
        <Queries queries={c.queries} onQuery={onQuery} title={t("ui.index_advisor.queries_title", { n: c.queries.length })} />
        {c.requests.length > 1 && (
          <div className="ia-block">
            <div className="ia-label">{t("ui.index_advisor.merges", { n: c.requests.length })}</div>
            <div className="ia-requests">
              {c.requests.map((r, i) => (
                <div key={i} className="ia-request">
                  <Keys keys={[...r.equality, ...r.inequality]} include={r.include} />
                  <span className="muted">
                    {t("ui.index_advisor.lookups", { n: r.lookups, count: count(r.lookups) })} · {t("ui.index_advisor.cheaper", { share: percent(r.improvement) })}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}
        <SqlBlock sql={c.sql} onCopy={onCopy} />
        {!c.sql && c.widens && <p className="ia-reason muted">{t("ui.index_advisor.widens_note", { index: c.widens })}</p>}
        {c.write_cost && <p className="ia-cost">{c.write_cost}</p>}
        <Verify steps={c.verify} />
        <Links onFinding={onFinding} hasFinding={!!c.finding_id} onTable={onTable} />
      </Body>
    </div>
  );
}

function cleanupMeta(c: IndexCleanup): string {
  const reads = c.reads != null ? t("ui.index_advisor.read_times", { count: count(c.reads) }) : null;
  const other = c.other ?? "";
  switch (c.kind) {
    case "unused":
      return [t("ui.index_advisor.never_read"), c.writes ? t("ui.index_advisor.writes", { n: c.writes, count: count(c.writes) }) : null, c.other ? t("ui.index_advisor.also_covered_by", { index: c.other }) : null].filter(Boolean).join(" · ");
    case "duplicate":
      return [t("ui.index_advisor.same_keys_as", { index: other }), reads].filter(Boolean).join(" · ");
    case "prefix":
      return [t("ui.index_advisor.covered_by", { index: other }), reads].filter(Boolean).join(" · ");
    case "overlap":
      return [t("ui.index_advisor.overlaps", { index: other }), reads].filter(Boolean).join(" · ");
  }
}

function CleanupRow({ c, focused, onFocus, expanded, onToggle, onCopy, onFinding, onTable }: RowProps & { c: IndexCleanup }) {
  const kind = cleanupKind(c.kind);
  const shared = c.other_keys.length ? c.keys.findIndex((k, i) => c.other_keys[i]?.toLowerCase() !== k.toLowerCase()) : 0;
  const prefix = shared === -1 ? c.keys.length : shared;
  return (
    <div className={`ia-item ${focused ? "focus" : ""} ${expanded ? "expanded" : ""}`}>
      <button type="button" className="ia-row" onClick={onToggle} onFocus={onFocus} aria-expanded={expanded}>
        <span className={`ia-pill ${kind.tone}`}>{kind.label}</span>
        <span className="ia-main">
          <span className="ia-title">
            <span className="ia-index">{c.index}</span>
            <span className="ia-on">{t("ui.index_advisor.on_table", { table: c.table })}</span>
          </span>
          <span className="ia-meta">{cleanupMeta(c)}</span>
        </span>
        <span className="ia-impact">
          <span className="ia-impact-n">{bytes(c.size_bytes)}</span>
        </span>
        <ChevronRight size={14} className="ia-chev" />
      </button>
      <Body expanded={expanded}>
        <p className="ia-reason">{c.reason}</p>
        {c.other && (
          <div className="ia-compare">
            <span className="ia-compare-name">{c.index}</span>
            <Keys keys={c.keys} shared={prefix} />
            <span className="ia-compare-name">{c.other}</span>
            <Keys keys={c.other_keys} shared={prefix} />
          </div>
        )}
        <SqlBlock sql={c.sql} onCopy={onCopy} />
        {c.caveats.map((cv) => (
          <p key={cv} className="ia-cost">
            {cv}
          </p>
        ))}
        <Links onFinding={onFinding} hasFinding={!!c.finding_id} onTable={onTable} />
      </Body>
    </div>
  );
}

function TableRow({ table, focused, onFocus, expanded, onToggle, onCopy, onFinding, onTable, onQuery }: RowProps & { table: TableAdvice }) {
  const kind = tableKind(table.kind);
  const facts: string[] = [];
  if (table.full_scans_per_day != null) facts.push(t("ui.index_advisor.full_reads_per_day", { count: count(table.full_scans_per_day) }));
  if (table.rows_scanned_per_day != null) facts.push(t("ui.index_advisor.rows_per_day", { n: table.rows_scanned_per_day, count: count(table.rows_scanned_per_day) }));
  if (table.forwarded_per_day != null) facts.push(t("ui.index_advisor.forwarded_per_day", { count: count(table.forwarded_per_day) }));
  if (table.indexes != null && table.kind === "over-indexed") facts.push(t("ui.index_advisor.indexes", { n: table.indexes, count: count(table.indexes) }));
  if (table.writes_per_day != null) facts.push(t("ui.index_advisor.writes_per_day", { n: table.writes_per_day, count: count(table.writes_per_day) }));
  return (
    <div className={`ia-item ${focused ? "focus" : ""} ${expanded ? "expanded" : ""}`}>
      <button type="button" className="ia-row" onClick={onToggle} onFocus={onFocus} aria-expanded={expanded}>
        <span className="ia-kind-icon">{kind.icon}</span>
        <span className="ia-main">
          <span className="ia-title">
            <span className="ia-table">{table.table}</span>
            <span className="ia-on">{kind.label}</span>
          </span>
          <span className="ia-meta">{facts.join(" · ")}</span>
        </span>
        <span className="ia-impact">{table.rows != null && <span className="ia-impact-l">{rowCount(table.rows)}</span>}</span>
        <ChevronRight size={14} className="ia-chev" />
      </button>
      <Body expanded={expanded}>
        <p className="ia-reason">{table.reason}</p>
        <Queries queries={table.queries} onQuery={onQuery} title={t("ui.index_advisor.read_by")} />
        <SqlBlock sql={table.sql} onCopy={onCopy} />
        <Links onFinding={onFinding} hasFinding={!!table.finding_id} onTable={onTable} />
      </Body>
    </div>
  );
}
