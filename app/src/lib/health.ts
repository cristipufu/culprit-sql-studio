import { isActionable } from "./findings";
import { t } from "./i18n";
import type { FileIo } from "../gen/FileIo";
import type { OpenTransaction } from "../gen/OpenTransaction";
import type { Finding } from "../gen/Finding";
import type { QueryStat } from "../gen/QueryStat";
import type { Replica } from "../gen/Replica";
import type { Report } from "../gen/Report";
import type { SignalStatus } from "../gen/SignalStatus";
import type { TableStat } from "../gen/TableStat";

/** Measurements are facts. Warning colors come from the report's assessed findings, not a second set of UI ratios. */
export function findingStatus(finding: Finding | undefined): SignalStatus {
  if (!finding || !isActionable(finding) || finding.severity === "low") return "info";
  return finding.severity === "high" ? "bad" : "watch";
}

export function worstFinding(report: Report, matches: (f: Finding) => boolean): Finding | undefined {
  const rank = { high: 2, medium: 1, low: 0 };
  return report.findings.filter((f) => isActionable(f) && matches(f)).sort((a, b) => rank[b.severity] - rank[a.severity])[0];
}

export function querySignalStatus(report: Report, query: QueryStat, checks: string[]): SignalStatus {
  return findingStatus(worstFinding(report, (f) => checks.includes(f.check) && f.queries.some((q) => q.id === query.id)));
}

export function tableSignalWarning(report: Report, table: TableStat, checks: string[]): boolean {
  const name = `${table.schema}.${table.name}`.toLowerCase();
  const status = findingStatus(worstFinding(report, (f) => checks.includes(f.check) &&
    (f.subject.name.toLowerCase() === name || f.subject.name.toLowerCase().startsWith(`${name}.`))));
  return status === "watch" || status === "bad";
}

export function storageStatus(report: Report, file: FileIo): SignalStatus {
  return findingStatus(worstFinding(report, (f) => (f.facts?.storage_files ?? []).some((source) =>
    source.name === file.name && source.kind === file.kind && source.database === file.database &&
    source.scope === (file.scope ?? null) && source.stats_reset === (file.stats_reset ?? null) &&
    source.write_operation === (file.write_operation ?? null))));
}

export function replicaStatus(report: Report, replica: Replica): SignalStatus {
  return findingStatus(worstFinding(report, (f) => f.check === "replica-lag" && f.subject.name === replica.name));
}

/**
 * What an open transaction is doing, in the app's language: its activity said from the catalog, or the engine's own
 * word where it gives one (as `OpenTransaction::state_words` says it in Rust).
 */
export function transactionState(txn: OpenTransaction): string | null {
  const own = txn.state ?? null;
  switch (txn.activity) {
    case "reported":
      return own;
    case "idle_in_transaction":
      return own ?? t("model.transaction.state.idle_in_transaction");
    case "running":
      return t("model.transaction.state.running");
    case "snapshot":
      return t("model.transaction.state.snapshot");
    case "slot_active":
      return t("model.transaction.state.slot_active");
    case "slot_inactive":
      return t("model.transaction.state.slot_inactive");
    case "prepared":
      return t("model.transaction.state.prepared");
    default:
      return own;
  }
}

/**
 * How a replica stands, in the app's language: its condition said from the catalog, with the engine's own word where
 * it gives one (as `Replica::state_words` says it in Rust).
 */
export function replicaState(replica: Replica): string | null {
  const own = replica.state ?? null;
  switch (replica.condition) {
    case "reported":
      return own;
    case "stopped":
      return own ?? t("model.replica.state.stopped");
    case "replicating":
      return t("model.replica.state.replicating");
    case "receiving_stopped":
      return own != null ? t("model.replica.state.receiving_stopped", { thread: own }) : t("model.replica.state.receiving_stopped_unknown");
    case "applying_stopped":
      return t("model.replica.state.applying_stopped");
    case "left_configured":
      return t("model.replica.state.left_configured");
    case "stopped_by_hand":
      return t("model.replica.state.stopped_by_hand");
    case "not_receiving":
      return t("model.replica.state.not_receiving");
    case "restoring_from_archive":
      return t("model.replica.state.restoring_from_archive");
    case "not_streaming":
      return t("model.replica.state.not_streaming");
    case "slot_active":
      return t("model.replica.state.slot_active");
    case "slot_inactive":
      return t("model.replica.state.slot_inactive");
    default:
      return own;
  }
}
