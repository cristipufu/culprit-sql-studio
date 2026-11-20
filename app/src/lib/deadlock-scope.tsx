import { FileWarning } from "lucide-react";
import type { DeadlockReport } from "../gen/DeadlockReport";
import { t } from "./i18n";

/** Shared or unresolved history does not establish a count for the current database. */
export function databaseDeadlockGraphCount(report: DeadlockReport | null | undefined): number | null {
  return report?.scope === "database" ? report.deadlocks.length : null;
}

/** History remains visible while its database attribution is withheld. */
export function DeadlockScopeNotice({ report }: { report: DeadlockReport }) {
  if (databaseDeadlockGraphCount(report) !== null) return null;
  return <p className="dl-inline-note" role="status">
    {report.scope === "server" && <strong>{t("ui.replication.scope.server")}</strong>}
    <span>{t("evidence.deadlocks.database_scope")}</span>
  </p>;
}

/** No returned graph from an unqualified source is an unknown database result, rather than a healthy zero. */
export function DeadlockUnavailableHistory({ report }: { report: DeadlockReport }) {
  return <>
    <FileWarning size={22} />
    <DeadlockScopeNotice report={report} />
    <p className="muted">{report.note ?? report.source}</p>
  </>;
}
