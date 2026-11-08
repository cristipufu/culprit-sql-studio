import { Activity, ChevronsUpDown, CircleAlert, CircleArrowUp, CircleDot, Gauge, GitCompareArrows, ListOrdered, Monitor, Moon, Plus, RefreshCw, RotateCw, Search, SquareTerminal, Sun, Table2 } from "lucide-react";
import type { ReactNode } from "react";
import { ago } from "../lib/format";
import { EngineBadge } from "./EngineBadge";
import { groupByEnvironment } from "../lib/environments";
import { currentReport, openConnection, scan, setState, useStore, type View } from "../lib/store";
import { can } from "../lib/engines";
import { languages, setLanguage, t, useLanguage } from "../lib/i18n";
import { useTheme } from "../lib/theme";
import { keys } from "../lib/keys";
import { installUpdate, restartToUpdate, useUpdate } from "../lib/updates";

/** An update the app found: offered quietly at the foot of the sidebar, installed when clicked, restarted on demand. */
function UpdateItem() {
  const update = useUpdate();
  switch (update.status) {
    case "available":
      return (
        <button type="button" className="update-item" onClick={() => void installUpdate()} title={t("ui.sidebar.update_title", { version: update.info.version, current: update.info.current })}>
          <CircleArrowUp size={13} /> {t("ui.sidebar.update_to", { version: update.info.version })}
        </button>
      );
    case "downloading":
      return (
        <div className="update-item busy" role="status">
          <span className="spinner" style={{ width: 12, height: 12 }} />
          {update.progress == null ? t("ui.sidebar.downloading", { version: update.info.version }) : t("ui.sidebar.downloading_percent", { version: update.info.version, percent: Math.round(update.progress * 100) })}
        </div>
      );
    case "ready":
      return (
        <button type="button" className="update-item" onClick={() => void restartToUpdate()} title={t("ui.sidebar.restart_title", { version: update.info.version })}>
          <RotateCw size={13} /> {t("ui.sidebar.restart_to_update")}
        </button>
      );
    case "failed":
      return (
        <button type="button" className="update-item failed" onClick={() => void installUpdate()} title={`${update.error} ${t("ui.sidebar.click_to_retry")}`}>
          <CircleAlert size={13} /> {t("ui.sidebar.update_failed")}
        </button>
      );
    default:
      return null;
  }
}

function ConnectionList() {
  const connections = useStore((s) => s.connections);
  const active = useStore((s) => s.active);
  if (connections.length === 0) return null;
  return (
    <div className="side-connections">
      <div className="sidebar-section side-section-row">
        {t("ui.common.view.connections")}
        <button type="button" className="side-add" onClick={() => setState({ view: "connections" })} aria-label={t("ui.sidebar.manage_connections")} title={t("ui.sidebar.manage_connections")}>
          <Plus size={13} />
        </button>
      </div>
      {groupByEnvironment(connections).flatMap((g) =>
        g.items.map((c) => (
          <button type="button"
            key={c.name}
            className={`nav-item conn-item ${c.name === active ? "active" : ""}`}
            title={`${c.display} · ${t("ui.common.environment", { environment: c.environment })}`}
            onClick={() => (c.name === active ? setState({ view: "dashboard" }) : openConnection(c.name))}
          >
            <EngineBadge engine={c.engine} environment={c.environment} product={c.last_report?.product} />
            <span className="conn-name">{c.name}</span>
            {c.last_report && <span className="count">{c.last_report.opportunities}</span>}
          </button>
        )),
      )}
    </div>
  );
}

const NAV: { view: View; label: () => string; key: string; icon: ReactNode }[] = [
  { view: "dashboard", label: () => t("ui.common.view.dashboard"), key: "1", icon: <Gauge size={15} /> },
  { view: "overview", label: () => t("ui.common.view.findings"), key: "2", icon: <CircleDot size={15} /> },
  { view: "queries", label: () => t("ui.common.view.queries"), key: "3", icon: <ListOrdered size={15} /> },
  { view: "live", label: () => t("ui.common.view.live"), key: "4", icon: <Activity size={15} /> },
  { view: "schema", label: () => t("ui.common.view.schema"), key: "5", icon: <Table2 size={15} /> },
  { view: "editor", label: () => t("ui.common.view.editor"), key: "6", icon: <SquareTerminal size={15} /> },
  { view: "plans", label: () => t("ui.common.view.plans"), key: "7", icon: <GitCompareArrows size={15} /> },
];

export function Sidebar() {
  const active = useStore((s) => s.active);
  const view = useStore((s) => s.view);
  const connection = useStore((s) => s.connections.find((c) => c.name === s.active));
  const report = useStore(currentReport);
  const scanning = useStore((s) => (s.report.status === "ready" && s.report.scanning) || s.report.status === "loading");
  const [theme, , setTheme] = useTheme();
  const language = useLanguage();

  return (
    <aside className="sidebar">
      <div className="sidebar-drag" data-tauri-drag-region />
      <button type="button"
        className="conn-switch"
        onClick={() => setState({ view: "connections" })}
        title={report ? `${active} · ${report.snapshot.server.product} · ${report.snapshot.server.database}` : undefined}
      >
        {connection ? (
          <EngineBadge engine={connection.engine} environment={connection.environment} product={report?.snapshot.server.product ?? connection.last_report?.product} size="md" />
        ) : (
          <span className="env-dot" />
        )}
        <span style={{ minWidth: 0 }}>
          <div className="name">{active ?? t("ui.sidebar.no_connection")}</div>
          {/* Product and database a line each, so a long database name isn't cut off behind the product. */}
          {report ? (
            <>
              <div className="sub">{report.snapshot.server.product}</div>
              <div className="sub">{report.snapshot.server.database}</div>
            </>
          ) : (
            <div className="sub">{connection ? connection.display : t("ui.sidebar.choose_database")}</div>
          )}
        </span>
        <ChevronsUpDown size={14} className="chev" />
      </button>
      <button type="button" className="side-search" onClick={() => setState({ palette: true })} aria-label={keys(t("ui.sidebar.search_label"))}>
        <Search size={13} /> <span className="side-search-text">{t("ui.sidebar.search")}</span> <span className="kbd">{keys("⌘K")}</span>
      </button>
      {(
        <nav className="nav">
          {NAV.filter((n) => (active || n.view === "plans") && (n.view !== "live" || !connection || can(connection.engine, "sessions"))).map((n) => (
            <button type="button" key={n.view} className={`nav-item ${view === n.view ? "active" : ""}`} onClick={() => setState({ view: n.view })}>
              {n.icon}
              {n.label()}
              {n.view === "overview" && report && <span className="count">{report.summary.opportunities}</span>}
              <span className="key">{n.key}</span>
            </button>
          ))}
        </nav>
      )}
      <ConnectionList />
      <div className="sidebar-foot">
        <UpdateItem />
        {active && (
          <button type="button" className="scan-status" onClick={() => scan(active)} title={keys(t("ui.sidebar.rescan_title"))}>
            {scanning ? <span className="spinner" style={{ width: 12, height: 12 }} /> : <RefreshCw size={13} />}
            {scanning ? t("ui.sidebar.scanning") : report ? t("ui.sidebar.scanned", { ago: ago(report.snapshot.taken_at) }) : t("ui.sidebar.scan")}
          </button>
        )}
        <div className="theme-row">
          {t("ui.sidebar.theme")}
          <span className="theme-seg" role="radiogroup" aria-label={t("ui.sidebar.theme")}>
            {(
              [
                ["system", <Monitor size={12} key="s" />, t("ui.sidebar.theme_system")],
                ["light", <Sun size={12} key="l" />, t("ui.sidebar.theme_light")],
                ["dark", <Moon size={12} key="d" />, t("ui.sidebar.theme_dark")],
              ] as const
            ).map(([value, icon, label]) => (
              <button type="button" key={value} role="radio" aria-checked={theme === value} aria-label={label} title={label} className={theme === value ? "on" : ""} onClick={() => setTheme(value)}>
                {icon}
              </button>
            ))}
          </span>
        </div>
        <label className="theme-row">
          {t("ui.sidebar.language")}
          <select className="language-select" value={language} onChange={(e) => setLanguage(e.target.value)}>
            {languages().map((l) => (
              <option key={l.code} value={l.code} lang={l.code}>
                {l.name}
              </option>
            ))}
          </select>
        </label>
      </div>
    </aside>
  );
}
