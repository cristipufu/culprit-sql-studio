import { Copy, FileText, KeyRound, RefreshCw, SquarePen } from "lucide-react";
import type { Capability } from "../gen/Capability";
import type { MissingCapability } from "../gen/MissingCapability";
import type { Report } from "../gen/Report";

import { scan, setState } from "../lib/store";
import { keys } from "../lib/keys";
import { t } from "../lib/i18n";
import { copyAndTell } from "../lib/clipboard";
import { canOpenFixInEditor } from "../lib/editor-sql";

/** "Culprit reads … only", for each source of query statistics. */
const READS_ONLY: Partial<Record<Capability, () => string>> = {
  "pg-stat-statements": () => t("ui.workload_unlock.reads_only.pg_stat_statements"),
  "query-store": () => t("ui.workload_unlock.reads_only.query_store"),
  "plan-cache": () => t("ui.workload_unlock.reads_only.plan_cache"),
  "performance-schema": () => t("ui.workload_unlock.reads_only.performance_schema"),
  "slow-log": () => t("ui.workload_unlock.reads_only.slow_log"),
};

/**
 * Shown when the database keeps no query statistics Culprit can read. Says what that costs, what one step unlocks it,
 * and offers the exact statement: copy it, or open it in the editor (which asks before it changes a production database).
 */
export function WorkloadUnlock({ report }: { report: Report }) {
  if (report.snapshot.workload || !report.summary.no_workload) return null;
  const missing: MissingCapability | undefined = report.snapshot.server.missing.find((m) => m.capability === report.summary.no_workload_capability);
  if (!missing) return null;
  // A source the application already writes (SQLite's query log): point at the connection's setting, not at SQL.
  if (missing.capability === "query-log") {
    return (
      <div className="unlock">
        <div className="unlock-head">
          <KeyRound size={15} />
          <b>{t("ui.workload_unlock.query_log_title")}</b>
        </div>
        <p>
          {missing.reason} {t("ui.workload_unlock.query_log_until")}
        </p>
        <div className="unlock-foot">
          <span className="muted">{t("ui.workload_unlock.query_log_reads_only")}</span>
          <button type="button" className="btn small" onClick={() => setState({ view: "connections" })}>
            <FileText size={12} /> {t("ui.workload_unlock.add_query_log")}
          </button>
        </div>
      </div>
    );
  }
  const sql = missing.action?.sql ?? null;
  const rest = missing.action ? missing.action.detail : missing.fix;
  return (
    <div className="unlock">
      <div className="unlock-head">
        <KeyRound size={15} />
        <b>{t("ui.workload_unlock.title")}</b>
      </div>
      <p>
        {missing.reason} {t("ui.workload_unlock.until")}
      </p>
      {sql && (
        <div className="unlock-sql">
          <code>{sql}</code>
          <span className="spacer" />
          <button type="button" className="btn small" onClick={() => copyAndTell(sql, t("ui.common.copied"))} aria-label={t("ui.workload_unlock.copy_statement")}>
            <Copy size={12} /> {t("ui.common.copy")}
          </button>
          {canOpenFixInEditor(sql, report.snapshot.server.engine) && (
            <button type="button" className="btn small" onClick={() => setState({ view: "editor", editorSql: sql })} aria-label={t("ui.workload_unlock.open_statement")}>
              <SquarePen size={12} /> {t("ui.common.open_in_editor")}
            </button>
          )}
        </div>
      )}
      {rest && <p className="muted">{rest}</p>}
      <div className="unlock-foot">
        <span className="muted">
          {READS_ONLY[missing.capability]?.() ?? ""}
        </span>
        <button type="button" className="btn small" onClick={() => scan(report.snapshot.connection)} title={keys(t("ui.workload_unlock.rescan_title"))}>
          <RefreshCw size={12} /> {t("ui.workload_unlock.rescan")}
        </button>
      </div>
    </div>
  );
}
