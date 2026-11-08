import { t, useLanguage } from "./lib/i18n";
import { AlertCircle } from "lucide-react";
import { lazy, Suspense, useEffect } from "react";
import { CommandPalette } from "./components/CommandPalette";
import { ScreenshotButton } from "./components/ScreenshotButton";
import { SignInDialog } from "./components/SignInDialog";
import { Sidebar } from "./components/Sidebar";
import { WindowPicker } from "./components/WindowPicker";
import { editorScope } from "./lib/editor-tabs";
import { when, windowLabel } from "./lib/format";
import { useHotkeys } from "./lib/hotkeys";
import { installPlanPaste } from "./lib/plans";
import { getState, lastConnection, loadConnections, openConnection, scan, setState, useStore, type View } from "./lib/store";
import { Connections } from "./views/Connections";
import { Live } from "./views/Live";
import { Overview } from "./views/Overview";
import { Queries } from "./views/Queries";
import { Schema } from "./views/Schema";
import { PasswordNeeded } from "./components/PasswordNeeded";
import { DashboardSkeleton } from "./components/DashboardSkeleton";
import { startUpdateChecks } from "./lib/updates";
import { windowUnknown } from "./lib/rates";
import { errorMessage } from "./lib/api";
import { toast as notify } from "./lib/store";

const Editor = lazy(() => import("./views/Editor"));
// Where a connection opens; its skeleton (and stylesheet) are in the main bundle so the grid never jumps.
const Dashboard = lazy(() => import("./views/Dashboard"));
// Statements coding agents want to run; loaded after first paint, polls only in the desktop app.
const ApprovalDialog = lazy(() => import("./components/ApprovalDialog"));
const Plans = lazy(() => import("./views/Plans").then((m) => ({ default: m.Plans })));

function viewTitle(view: View): string {
  switch (view) {
    case "dashboard":
      return t("ui.common.view.dashboard");
    case "overview":
      return t("ui.common.view.findings");
    case "queries":
      return t("ui.common.view.queries");
    case "live":
      return t("ui.common.view.live");
    case "schema":
      return t("ui.common.view.schema");
    case "editor":
      return t("ui.common.view.editor");
    case "plans":
      return t("ui.common.view.plans");
    case "connections":
      return t("ui.common.view.connections");
  }
}

/** The sources a scan's query history comes from; when one is missing, its reason says why there is no workload. */


const VIEW_KEYS: View[] = ["dashboard", "overview", "queries", "live", "schema", "editor", "plans"];

export function App() {
  // A new language re-renders the whole app: every component words itself with t() as it renders.
  useLanguage();
  const view = useStore((s) => s.view);
  const active = useStore((s) => s.active);
  const environment = useStore((s) => s.connections.find((c) => c.name === s.active)?.environment);
  const palette = useStore((s) => s.palette);
  const toast = useStore((s) => s.toast);

  useEffect(() => installPlanPaste(() => getState().view), []);
  useEffect(() => startUpdateChecks(), []);

  useEffect(() => {
    let cancelled = false;
    loadConnections().then((connections) => {
      if (cancelled || getState().active !== null) return;
      const last = lastConnection();
      if (last && connections.some((c) => c.name === last)) {
        // The dashboard is where it opens: fetch its code alongside the cached report, not after it.
        void import("./views/Dashboard").catch(() => {});
        return openConnection(last);
      }
    }).catch((error) => { if (!cancelled) notify(errorMessage(error).message); });
    return () => { cancelled = true; };
  }, []);

  useHotkeys({
    "mod+k": () => setState((s) => ({ palette: !s.palette })),
    "mod+r": () => {
      const name = getState().active;
      if (name) scan(name);
    },
    ...Object.fromEntries(
      VIEW_KEYS.map((v, i) => [
        String(i + 1),
        () => {
          if (getState().active || v === "plans") setState({ view: v });
        },
      ]),
    ),
    Escape: () => {
      if (getState().palette) setState({ palette: false });
    },
  });

  return (
    <div className="shell">
      <Sidebar />
      <main className={`main ${view !== "connections" && environment === "production" ? "is-production" : ""}`}>
        {view !== "connections" && <TopBar />}
        <div className="view">
          {view === "plans" ? (
            <Suspense fallback={<div className="empty"><div className="spinner" /></div>}>
              <Plans />
            </Suspense>
          ) : view === "connections" || !active ? (
            <Connections />
          ) : (
            <Workspace view={view} name={active} />
          )}
        </div>
      </main>
      {palette && <CommandPalette />}
      <SignInDialog />
      {toast && <div className="toast">{toast}</div>}
      <Suspense fallback={null}>
        <ApprovalDialog />
      </Suspense>
    </div>
  );
}

function TopBar() {
  const view = useStore((s) => s.view);
  const environment = useStore((s) => s.connections.find((c) => c.name === s.active)?.environment);
  const report = useStore((s) => (s.report.status === "ready" ? s.report : null));
  const engine = useStore((s) => s.connections.find((c) => c.name === s.active)?.engine);
  const w = report?.report.snapshot.workload;
  // Before the report loads, SQL Server keeps the picker's place; a scan without query history has nothing to window.
  // An application's log covers its own period: there's no window to choose.
  const windowed = w ? w.windowed && w.log == null : engine === "mssql" && !report;
  const noHistory = report && !w ? report.report.summary.no_workload : undefined;
  const showsWorkload = view === "dashboard" || view === "overview" || view === "queries" || view === "schema";
  if (view === "plans") {
    return (
      <header className="topbar" data-tauri-drag-region>
        <h1>{viewTitle(view)}</h1>
        <span className="context">{t("ui.app.plans_context")}</span>
        <span className="spacer" />
        <ScreenshotButton />
      </header>
    );
  }
  return (
    <header className="topbar" data-tauri-drag-region>
      <h1>{viewTitle(view)}</h1>
      {showsWorkload && windowed && <WindowPicker coveredHours={w?.window_hours ?? null} source={w?.source ?? null} />}
      {showsWorkload && w?.log != null && (
        <span
          className="context"
          title={
            !w.window_start
              ? t("ui.app.log_title", { log: w.log })
              : w.window_end
                ? t("ui.app.log_title_range", { log: w.log, from: when(w.window_start).full, to: when(w.window_end).full })
                : t("ui.app.log_title_from", { log: w.log, from: when(w.window_start).full })
          }
        >
          {windowUnknown(w)
            ? t("ui.app.log_no_timestamps", { source: w.source })
            : t("ui.app.log_window", { source: w.source, window: windowLabel(w.window_hours).replace(/^last /, "") })}
        </span>
      )}
      {showsWorkload && report && !w && (
        <span className="context" title={noHistory ?? t("ui.app.no_statement_statistics")}>
          {t("ui.app.no_query_history")}
        </span>
      )}
      {showsWorkload && !windowed && w && w.log == null && (
        <span
          className="context"
          title={
            w.window_start
              ? t("ui.app.cumulative_title_since", { time: when(w.window_start).full })
              : t("ui.app.cumulative_title")
          }
        >
          {w.window_start
            ? t("ui.app.cumulative_since", { source: w.source, time: when(w.window_start).short })
            : t("ui.app.cumulative_since_reset", { source: w.source })}
        </span>
      )}
      {environment && (
        <span
          className={`env-chip env-${environment}`}
          title={
            environment === "production"
              ? t("ui.app.production_title")
              : t("ui.app.environment_connection", { environment: t("ui.common.environment", { environment }) })
          }
        >
          {t("ui.common.environment", { environment })}
        </span>
      )}
      <span className="spacer" />
      {report?.scanError && (
        // biome-ignore lint/a11y/noNoninteractiveTabindex: focusing it opens the error's details (:focus-within), as hovering does
        <div className="scan-error-anchor" tabIndex={0}>
          <span className="tag high scan-error-badge">
            <AlertCircle size={12} aria-hidden />
            {t("ui.app.last_scan_failed")}
          </span>
          <div className="scan-error-popover" role="tooltip">
            <div className="scan-error-title">
              <span className="sev high" aria-hidden />
              {t("ui.app.last_scan_failed")}
            </div>
            <p className="scan-error-msg selectable">{report.scanError.message}</p>
            {report.scanError.hint && (
              <p className="scan-error-hint selectable">{report.scanError.hint}</p>
            )}
          </div>
        </div>
      )}
      <ScreenshotButton />
    </header>
  );
}

function Workspace({ view, name }: { view: View; name: string }) {
  const state = useStore((s) => s.report);
  const connection = useStore((s) => s.connections.find((c) => c.name === name));
  if (view === "live") return <Live connection={name} />;
  if (view === "editor" && connection) {
    return (
      <Suspense fallback={<div className="empty"><div className="spinner" /></div>}>
        <Editor key={editorScope(connection)} connection={connection} />
      </Suspense>
    );
  }
  if (state.status === "loading" || state.status === "none") return view === "dashboard" ? <DashboardSkeleton /> : <Loading />;
  if (state.status === "error") {
    if (connection?.auth === "password" && (state.error.code === "auth_failed" || !connection.has_secret)) {
      return <PasswordNeeded connection={connection} message={state.error.message} />;
    }
    return (
      <div className="empty">
        <h3>{t("ui.app.scan_failed", { name })}</h3>
        <p>{state.error.message}</p>
        {state.error.hint && <p className="muted">{state.error.hint}</p>}
        <button type="button" className="btn" onClick={() => scan(name)}>
          {t("ui.common.try_again")}
        </button>
      </div>
    );
  }
  const report = state.report;
  if (view === "dashboard" && connection) {
    return (
      <Suspense fallback={<DashboardSkeleton />}>
        <Dashboard key={name} report={report} connection={connection} />
      </Suspense>
    );
  }
  if (view === "queries") return <Queries report={report} />;
  if (view === "schema") return <Schema report={report} />;
  return <Overview report={report} />;
}

function Loading() {
  return (
    <div className="overview">
      <div className="ov-list" style={{ padding: 18, gap: 12, display: "flex", flexDirection: "column" }}>
        <div className="skeleton" style={{ height: 26, width: "45%" }} />
        <div className="skeleton" style={{ height: 16, width: "80%" }} />
        <div className="skeleton" style={{ height: 58 }} />
        {[0, 1, 2, 3, 4].map((i) => (
          <div key={i} className="skeleton" style={{ height: 52, opacity: 1 - i * 0.15 }} />
        ))}
      </div>
      <div className="empty">
        <div className="spinner" />
        {t("ui.app.reading_statistics")}
      </div>
    </div>
  );
}
