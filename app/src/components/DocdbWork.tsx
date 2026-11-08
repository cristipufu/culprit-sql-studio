import type { DocdbMetrics } from "../gen/DocdbMetrics";
import { count, duration } from "../lib/format";
import { t } from "../lib/i18n";

/** Native storage entries and RPCs retain their units; they are never PostgreSQL pages or CPU utilization. */
export function DocdbWork({ metrics, basis, runs }: { metrics?: DocdbMetrics | null; basis: "execution" | "loop" | "workload"; runs?: number }) {
  if (!metrics) return null;
  const divisor = basis === "workload" && runs != null && runs > 0 ? runs : 1;
  const label = basis === "workload" ? (runs != null && runs > 0 ? t("ui.docdb_work.basis.per_run") : t("ui.docdb_work.basis.retained")) : basis === "loop" ? t("ui.docdb_work.basis.per_loop") : t("ui.docdb_work.basis.execution");
  const entries: [keyof DocdbMetrics, string, "count" | "time"][] = [
    ["read_requests", t("ui.docdb_work.read_requests"), "count"], ["write_requests", t("ui.docdb_work.write_requests"), "count"],
    ["read_operations", t("ui.docdb_work.read_operations"), "count"], ["write_operations", t("ui.docdb_work.write_operations"), "count"],
    ["rows_scanned", t("ui.docdb_work.rows_scanned"), "count"], ["rows_returned", t("ui.docdb_work.rows_returned"), "count"],
    ["read_ms", t("ui.docdb_work.read_ms"), "time"], ["write_ms", t("ui.docdb_work.write_ms"), "time"],
    ["wait_ms", t("ui.docdb_work.wait_ms"), "time"], ["catalog_ms", t("ui.docdb_work.catalog_ms"), "time"],
    ["retries", t("ui.docdb_work.retries"), "count"], ["conflict_retries", t("ui.docdb_work.conflict_retries"), "count"],
    ["read_restarts", t("ui.docdb_work.read_restarts"), "count"], ["obsolete_rows_scanned", t("ui.docdb_work.obsolete_rows_scanned"), "count"],
    ["seeks", t("ui.docdb_work.seeks"), "count"], ["nexts", t("ui.docdb_work.nexts"), "count"], ["prevs", t("ui.docdb_work.prevs"), "count"],
  ];
  const rows = entries.flatMap(([key, title, unit]) => {
    const value = metrics[key];
    return value == null || !Number.isFinite(value) || value < 0 ? [] : [{ key, title, value: unit === "time" ? duration(value / divisor) : count(value / divisor) }];
  });
  if (rows.length === 0) return null;
  return (
    <section className="beyond" aria-label={t("ui.docdb_work.title")}>
      <h3>{t("ui.docdb_work.title")}</h3>
      <p className="beyond-foot">{t("ui.docdb_work.foot", { basis: label })}</p>
      <div className="beyond-rows" role="list">
        {rows.map((row) => <div className="beyond-row" key={row.key} role="listitem"><i className="sig-dot info" aria-hidden /><span className="beyond-label">{row.title}</span><span className="beyond-value">{row.value}</span></div>)}
      </div>
    </section>
  );
}
