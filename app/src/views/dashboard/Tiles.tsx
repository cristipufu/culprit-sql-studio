import { ChevronRight, Copy, FileText, RefreshCw, SquarePen } from "lucide-react";
import type { MouseEvent, ReactNode } from "react";
import type { Report } from "../../gen/Report";
import type { Tile } from "../../gen/Tile";
import type { TileItem } from "../../gen/TileItem";
import type { TileStatus } from "../../gen/TileStatus";
import { copyAndTell } from "../../lib/clipboard";
import { STATUS_LABEL } from "../../lib/dashboard";
import { canOpenFixInEditor } from "../../lib/editor-sql";
import { dateLocale, percent, when } from "../../lib/format";
import { t } from "../../lib/i18n";
import { keys } from "../../lib/keys";
import { querySummary } from "../../lib/query-summary";
import { scan, setState } from "../../lib/store";
import { openLive } from "../Live";
import { slot, tx, withElements } from "../../lib/message-parts";
import { rememberQueryTab } from "../Queries";

/** Open a query in Queries, on one of its tabs. */
export function openQuery(id: string, tab?: "summary" | "plan" | "history"): void {
  if (tab) rememberQueryTab(tab);
  setState({ view: "queries", query: id });
}

export function openFinding(id: string): void {
  setState({ view: "overview", finding: id, changes: null });
}

type FrameProps = {
  tile: Tile;
  /** What clicking the tile (or Enter on it) opens, in a few words. */
  action?: string;
  onOpen?: () => void;
  className?: string;
  children?: ReactNode;
  /** Shown instead of the detail line. */
  foot?: ReactNode;
};

/** Every tile: status and title, the takeaway first, then the picture, then one quiet line. */
export function Frame({ tile, action, onOpen, className = "", children, foot }: FrameProps) {
  const open = (e: MouseEvent) => {
    if (!onOpen) return;
    // Rows, chips and charts inside the tile do their own thing.
    if ((e.target as HTMLElement).closest("button, a, [data-own-click]")) return;
    onOpen();
  };
  const titleId = `dash-${tile.key}-title`;
  return (
    <section
      className={`dash-tile t-${tile.key} ${tile.status} ${onOpen ? "openable" : ""} ${className}`}
      data-tile={tile.key}
      // biome-ignore lint/a11y/noNoninteractiveTabindex: Enter or Space opens the tile's view; its own buttons stay separate stops
      tabIndex={0}
      aria-labelledby={titleId}
      aria-describedby={`dash-${tile.key}-takeaway`}
      onClick={open}
      onKeyDown={(e) => {
        if (onOpen && e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) {
          e.preventDefault();
          onOpen();
        }
      }}
    >
      <header className="dash-tile-head">
        <span className={`dash-dot ${tile.status}`} title={STATUS_LABEL[tile.status]} aria-label={STATUS_LABEL[tile.status]} role="img" />
        <h3 id={titleId}>{tile.title}</h3>
        {onOpen && action && (
          <button type="button" className="dash-open" onClick={onOpen} tabIndex={-1} aria-hidden>
            {action} <ChevronRight size={12} />
          </button>
        )}
      </header>
      <p className="dash-takeaway" id={`dash-${tile.key}-takeaway`}>
        {tile.takeaway}
      </p>
      {children}
      {foot !== undefined ? foot : tile.detail ? <p className="dash-detail">{tile.detail}</p> : null}
    </section>
  );
}

export function metricText(tile: Tile, key: string): string | null {
  return tile.metrics.find((m) => m.key === key)?.text ?? null;
}

export function metricValue(tile: Tile, key: string): number {
  return tile.metrics.find((m) => m.key === key)?.value ?? 0;
}

export function Fix({ sql }: { sql: string }) {
  return (
    <div className="dash-fix" data-own-click>
      <code>{sql}</code>
      <button type="button" className="btn small" onClick={() => copyAndTell(sql, t("ui.common.copied"))} aria-label={t("ui.dashboard.copy_statement")}>
        <Copy size={12} /> {t("ui.common.copy")}
      </button>
    </div>
  );
}

export function severityOf(status: TileStatus | null): "high" | "medium" | "low" {
  return status === "bad" ? "high" : status === "watch" ? "medium" : "low";
}

export function queryLabel(item: TileItem, report: Report): ReactNode {
  const q = item.query_id ? report.snapshot.workload?.queries.find((x) => x.id === item.query_id) : undefined;
  if (!q) return item.label;
  const s = querySummary(q.text, q.object_name, q.resolved_write_target);
  return (
    <>
      <span className="dash-verb">{s.verb}</span> {s.target ?? ""}
      {s.predicate && <span className="dash-pred"> · {s.predicate}</span>}
    </>
  );
}

export function HealthTile({ tile, onOpen }: { tile: Tile; onOpen: () => void }) {
  if (tile.status === "unavailable") return <Frame tile={tile} />;
  // The takeaway names the worst signal; the rows show the next ones, then the healthy ones with their values.
  const notable = tile.items.filter((i) => i.status === "bad" || i.status === "watch");
  const rows = [...notable.slice(1), ...tile.items.filter((i) => i.status !== "bad" && i.status !== "watch")].slice(0, 3);
  return (
    <Frame tile={tile} action={t("ui.dashboard.action.all_signals")} onOpen={onOpen}>
      <div className="dash-signals" role="img" aria-label={t("ui.dashboard.health.signals", { n: tile.items.length, count: String(tile.items.length) })}>
        {tile.items.map((i) => (
          <span key={i.id} className={`dash-signal ${i.status ?? "info"}`} title={`${i.label}: ${i.value ?? ""}`} />
        ))}
      </div>
      {rows.length > 0 && (
        <ul className="dash-rows compact">
          {rows.map((i) => (
            <li key={i.id}>
              <button type="button" className="dash-row" onClick={onOpen}>
                <i className={`sig-dot ${i.status}`} aria-hidden />
                <span className="dash-row-label">{i.label}</span>
                <span className="dash-row-value">{i.value}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Frame>
  );
}

/** The four signal tiles share one shape: up to two numbers, then the rows behind them, worst first. */
/** Rows are signals, or queries when `report` is given: those read like Top queries ("UPDATE orders · id = ?"), so two
 * statements on one table stay apart. */
export function SignalsTile({
  tile,
  action,
  onOpen,
  onRow,
  report,
}: {
  tile: Tile;
  action: string;
  onOpen: () => void;
  onRow?: (item: TileItem) => void;
  report?: Report;
}) {
  if (tile.status === "unavailable") return <Frame tile={tile} />;
  return (
    <Frame tile={tile} action={action} onOpen={onOpen}>
      {tile.metrics.length > 0 && (
        <div className="dash-stats compact">
          {tile.metrics.slice(0, 2).map((m) => (
            <div key={m.key} className="dash-stat">
              <b>{m.text}</b>
              <span>{m.label}</span>
            </div>
          ))}
        </div>
      )}
      {tile.items.length > 0 && (
        <ul className="dash-rows compact">
          {tile.items.map((i) => (
            <li key={i.id}>
              <button type="button" className="dash-row" onClick={() => (onRow ? onRow(i) : onOpen())}>
                <i className={`sig-dot ${i.status ?? "info"}`} aria-hidden />
                <span className="dash-row-label">{report ? queryLabel(i, report) : i.label}</span>
                <span className="dash-row-value">{i.value}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Frame>
  );
}

export function DeadlocksTile({ tile, report }: { tile: Tile; report: Report }) {
  const d = report.snapshot.deadlocks;
  if (tile.status === "unavailable" || !d) return <Frame tile={tile} />;
  const open = () => openLive("deadlocks");
  if (d.counted != null && d.deadlocks.length === 0) {
    return (
      <Frame tile={tile} action={t("ui.dashboard.action.deadlocks")} onOpen={open} foot={<p className="dash-detail">{t("ui.dashboard.deadlocks.paste")}</p>}>
        <div className="dash-big">
          <b>{d.counted}</b>
          <span title={d.counted_since ? when(d.counted_since).full : undefined}>{d.counted_since ? t("ui.dashboard.deadlocks.counted_since", { time: when(d.counted_since).short }) : t("ui.dashboard.deadlocks.counted")}</span>
        </div>
      </Frame>
    );
  }
  const end = Date.parse(report.snapshot.taken_at);
  const start = end - d.since_hours * 3_600_000;
  return (
    <Frame tile={tile} action={t("ui.dashboard.action.deadlocks")} onOpen={open}>
      <div className="dash-ticks" role="img" aria-label={t("ui.dashboard.deadlocks.over", { n: d.deadlocks.length, count: String(d.deadlocks.length), h: d.since_hours, hours: String(d.since_hours) })}>
        {d.deadlocks.map((dl) => {
          const at = dl.at ? Date.parse(dl.at) : NaN;
          if (!Number.isFinite(at)) return null;
          const left = Math.min(100, Math.max(0, ((at - start) / (end - start)) * 100));
          return <span key={dl.id} style={{ left: `${left}%` }} title={`${dl.headline}${dl.at ? ` · ${new Date(dl.at).toLocaleString(dateLocale())}` : ""}`} />;
        })}
        <em>{t("ui.dashboard.hours_ago", { hours: String(d.since_hours >= 23.5 ? 24 : Math.round(d.since_hours)) })}</em>
        <em className="end">{t("ui.dashboard.now")}</em>
      </div>
      {tile.items.length > 0 && (
        <ul className="dash-rows compact">
          {tile.items.slice(0, 2).map((i) => (
            <li key={i.id}>
              <button type="button" className="dash-row" onClick={() => (i.finding_id ? openFinding(i.finding_id) : open())}>
                <span className="dash-row-label">{i.label}</span>
                <span className="dash-row-value">{i.value}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Frame>
  );
}

export function IndexesTile({ tile }: { tile: Tile }) {
  const open = () => setState({ view: "schema", advisor: true });
  const adds = metricValue(tile, "add");
  const cleanups = metricValue(tile, "cleanup");
  const candidate = tile.items.find((i) => i.kind === "index-add");
  return (
    <Frame tile={tile} action={t("ui.common.index_advisor")} onOpen={open}>
      {(adds > 0 || cleanups > 0) && (
        <div className="dash-pair">
          <div className={`dash-big ${adds > 0 ? "" : "zero"}`}>
            <b>{adds}</b>
            <span>{t("ui.dashboard.indexes.to_add")}</span>
          </div>
          <div className={`dash-big ${cleanups > 0 ? "" : "zero"}`}>
            <b>{cleanups}</b>
            <span>
              {cleanups > 0 && metricValue(tile, "reclaim") > 0
                ? t("ui.dashboard.indexes.to_clean_up_reclaim", { reclaim: metricText(tile, "reclaim") ?? "" })
                : t("ui.dashboard.indexes.to_clean_up")}
            </span>
          </div>
        </div>
      )}
      {candidate && (
        <ul className="dash-rows compact">
          <li>
            <button type="button" className="dash-row" onClick={() => (candidate.finding_id ? openFinding(candidate.finding_id) : open())}>
              <span className="dash-plus" aria-hidden>
                +
              </span>
              <span className="dash-row-label mono">{candidate.label}</span>
              <span className="dash-row-value">{candidate.value}</span>
            </button>
          </li>
        </ul>
      )}
    </Frame>
  );
}

export function PlansTile({ tile }: { tile: Tile }) {
  if (tile.status === "unavailable") return <Frame tile={tile} />;
  const captured = metricValue(tile, "captured");
  const flagged = metricValue(tile, "flagged");
  const top = tile.items[0]?.query_id;
  return (
    <Frame tile={tile} action={t("ui.common.view.plans")} onOpen={() => (top ? openQuery(top, "plan") : setState({ view: "queries" }))}>
      <div className="dash-meter" role="img" aria-label={t("ui.dashboard.plans.flagged", { n: flagged, flagged: String(flagged), captured: String(captured) })}>
        {Array.from({ length: Math.min(captured, 40) }, (_, i) => (
          <span key={i} className={i < Math.round((flagged / Math.max(captured, 1)) * Math.min(captured, 40)) ? "on" : ""} />
        ))}
      </div>
      {tile.items.length > 0 && (
        <ul className="dash-rows compact">
          {tile.items.slice(0, 2).map((i) => (
            <li key={i.id}>
              <button type="button" className="dash-row" onClick={() => i.query_id && openQuery(i.query_id, "plan")} title={t("ui.dashboard.plans.open", { share: percent(i.share) })}>
                <span className={`sev ${severityOf(i.status)}`} aria-hidden />
                <span className="dash-row-label">{i.label}</span>
                <span className="dash-row-value">{i.value}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Frame>
  );
}

export function StatisticsTile({ tile, report }: { tile: Tile; report: Report }) {
  // Statements from a log the application writes (SQLite): the step is a setting on the connection, not SQL.
  if (!report.snapshot.workload && report.snapshot.server.missing.some((m) => m.capability === "query-log")) {
    return (
      <Frame tile={tile} className="unlocking">
        <div className="dash-actions">
          <span className="muted">{t("ui.dashboard.statistics.log_only")}</span>
          <button type="button" className="btn small" onClick={() => setState({ view: "connections" })}>
            <FileText size={12} /> {t("ui.dashboard.statistics.add_log")}
          </button>
        </div>
      </Frame>
    );
  }
  if (!report.snapshot.workload) {
    const sql = tile.fix_sql;
    return (
      <Frame tile={tile} className="unlocking">
        {sql && (
          <div className="dash-fix" data-own-click>
            <code>{sql}</code>
            <button type="button" className="btn small" onClick={() => copyAndTell(sql, t("ui.common.copied"))} aria-label={t("ui.dashboard.copy_statement")}>
              <Copy size={12} /> {t("ui.common.copy")}
            </button>
            {canOpenFixInEditor(sql, report.snapshot.server.engine) && (
              <button type="button" className="btn small" onClick={() => setState({ view: "editor", editorSql: sql })} title={t("ui.common.open_in_editor_title")}>
                <SquarePen size={12} /> {t("ui.common.open_in_editor")}
              </button>
            )}
          </div>
        )}
        <div className="dash-actions">
          <span className="muted">{t("ui.dashboard.statistics.reads_only")}</span>
          <button type="button" className="btn small" onClick={() => scan(report.snapshot.connection)} title={t("ui.dashboard.statistics.rescan_title", { key: keys("⌘R") })}>
            <RefreshCw size={12} /> {t("ui.dashboard.statistics.rescan")}
          </button>
        </div>
      </Frame>
    );
  }
  const finding = tile.items.find((i) => i.finding_id);
  const chips = tile.metrics.filter((m) => m.key !== "locked" || (m.value ?? 0) > 0);
  return (
    <Frame tile={tile} action={finding ? t("ui.dashboard.action.open_finding") : t("ui.common.view.queries")} onOpen={() => (finding?.finding_id ? openFinding(finding.finding_id) : setState({ view: "queries" }))}>
      <div className="dash-chips">
        {chips.map((m) =>
          m.key === "window-hours" ? (
            <span key={m.key}>
              {withElements(t("ui.dashboard.statistics.covers", { window: slot(0) }), [<b key="window">{m.text}</b>])}
            </span>
          ) : (
            <span key={m.key} className={m.key === "locked" ? "warn" : ""}>
              {m.key === "queries"
                ? tx("ui.dashboard.statistics.queries_count", { n: m.value ?? 0 }, { count: <b>{m.text}</b> })
                : m.key === "plans"
                  ? tx("ui.dashboard.statistics.plans_count", { n: m.value ?? 0 }, { count: <b>{m.text}</b> })
                  : m.key === "locked"
                    ? tx("ui.dashboard.statistics.locked_count", { n: m.value ?? 0 }, { count: <b>{m.text}</b> })
                    : <><b>{m.text}</b> {m.label}</>}
            </span>
          ),
        )}
      </div>
      {tile.fix_sql && tile.status !== "good" && <Fix sql={tile.fix_sql} />}
    </Frame>
  );
}
