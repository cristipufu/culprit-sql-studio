import type { Report } from "../gen/Report";
import { t } from "./i18n";

export type QueryHistoryEmptyState = {
  kind: "choose-group" | "unavailable";
  title: string;
  detail: string;
  sourceReason: string | null;
};

/** A saved catalog describes available history even when the default view cannot combine its groups. */
export function queryHistoryEmptyState(report: Report): QueryHistoryEmptyState | null {
  if (report.snapshot.workload) return null;
  const catalog = report.snapshot.runtime_populations;
  if (report.snapshot.options?.workload_population == null
    && catalog?.partitioned && catalog.requires_selection && catalog.populations.length > 0) {
    const source = report.snapshot.sources?.find((outcome) => outcome.source === catalog.source);
    const missing = report.snapshot.server.missing.find((capability) => capability.capability === report.summary.no_workload_capability);
    return {
      kind: "choose-group",
      title: t("ui.population_picker.label"),
      detail: t("ui.population_picker.required"),
      sourceReason: ((source?.state === "partial" || source?.state === "unavailable") ? source.reason : null) ?? missing?.reason ?? null,
    };
  }
  return {
    kind: "unavailable",
    title: t("ui.queries.no_history"),
    detail: report.summary.no_workload ?? t("ui.queries.no_history_detail"),
    sourceReason: null,
  };
}
