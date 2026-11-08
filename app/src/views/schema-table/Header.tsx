import { ChevronRight, Copy } from "lucide-react";
import { useState, type ReactNode } from "react";
import { TableSignalFacts } from "../../components/BeyondTime";
import type { Report } from "../../gen/Report";
import type { TableStat } from "../../gen/TableStat";
import { copyAndTell } from "../../lib/clipboard";
import { ago, bytes, count, percent } from "../../lib/format";
import { tableSignalWarning } from "../../lib/health";
import { t } from "../../lib/i18n";
import { tableFindings, tableKey } from "../../lib/schema-model";
import { setState } from "../../lib/store";

/** The table's name, size, activity and what Culprit found about it, above the tabs. */
export function Header({ report, table, findings }: { report: Report; table: TableStat; findings: ReturnType<typeof tableFindings> }) {
  const [all, setAll] = useState(false);
  const { snapshot } = report;
  const data = table.data_bytes ?? 0;
  const index = table.index_bytes ?? 0;
  const total = Math.max(1, data + index);
  // How the rows' own data splits between the rows and the large values kept apart from them, where the engine says.
  const large = table.lob_bytes == null ? null : table.lob_bytes + (table.overflow_bytes ?? 0);
  const own = large == null || table.in_row_bytes == null ? null : table.in_row_bytes + large;
  const averageRow = own != null && own > 0 && table.rows != null && table.rows >= 1 ? own / table.rows : null;
  // SQL Server keeps statistics per object, not per table: show the stalest one Culprit read for this table.
  const stats = snapshot.statistics.filter((s) => s.schema === table.schema && s.table === table.name && s.last_updated);
  const stalest = stats.reduce<string | null>((oldest, s) => (oldest == null || (s.last_updated ?? "") < oldest ? s.last_updated : oldest), null);
  const analyzed = snapshot.server.engine === "mssql" ? stalest : table.last_analyze;
  const knowsStats = snapshot.server.engine === "postgres" || stats.length > 0 || table.last_analyze != null;
  const writes = [table.inserts, table.updates, table.deletes].every((v) => v == null)
    ? null
    : (table.inserts ?? 0) + (table.updates ?? 0) + (table.deletes ?? 0);
  const health: { label: string; why: string }[] = [];
  if (table.is_heap)
    health.push({
      label: t("ui.table_header.no_clustered_index"),
      why: t("ui.table_header.no_clustered_index_why"),
    });
  if (!table.has_primary_key && table.kind !== "virtual")
    health.push({
      label: t("ui.common.no_primary_key"),
      why: t("ui.table_header.no_primary_key_why"),
    });
  if (table.dead_rows != null && table.rows != null && table.dead_rows > Math.max(10_000, table.rows * 0.2))
    health.push({
      label: t("ui.table_header.dead_rows", { count: count(table.dead_rows) }),
      why: t("ui.table_header.dead_rows_why"),
    });
  const rank = { high: 3, medium: 2, low: 1 } as const;
  const sorted = [...findings].sort((a, b) => rank[b.severity] - rank[a.severity]);
  const shown = all ? sorted : sorted.slice(0, 1);

  return (
    <header className="td-head">
      <div className="td-title">
        <h2 title={tableKey(table)}>
          <span className="td-schema">{table.schema}.</span>
          {table.name}
        </h2>
        {table.kind === "virtual" && <span className="tag">{t("ui.table_header.virtual")}</span>}
        {table.kind === "hypertable" && <span className="tag">{t("ui.table_header.hypertable")}</span>}
        <button type="button" className="btn ghost icon" title={t("ui.table_header.copy_name")} onClick={() => copyAndTell(tableKey(table), t("ui.table_header.copied_name"))}>
          <Copy size={13} />
        </button>
        {health.map((h) => (
          <span key={h.label} className="tag medium" title={h.why}>
            {h.label}
          </span>
        ))}
      </div>
      <div className="td-facts">
        <Fact label={t("common.metric.rows")} value={count(table.rows)} />
        {table.storage?.hypertable && <Fact label={t("ui.table_header.chunks")} value={count(table.storage.hypertable.chunks)} detail={table.storage.hypertable.compressed_chunks == null ? undefined : t("ui.table_header.in_columnstore", { count: count(table.storage.hypertable.compressed_chunks) })} />}
        <Fact
          label={t("common.metric.size")}
          value={bytes(table.total_bytes ?? (data + index || null))}
          detail={
            data + index > 0 ? (
              <span className="td-split" title={t("ui.table_header.split", { data: bytes(data), indexes: bytes(index) })}>
                <span className="td-split-bar">
                  <span className="d" style={{ width: `${(data / total) * 100}%` }} />
                  <span className="i" style={{ width: `${(index / total) * 100}%` }} />
                </span>
                <span>
                  {t("ui.table_header.split", { data: bytes(data), indexes: bytes(index) })}
                </span>
              </span>
            ) : null
          }
        />
        {averageRow != null && own != null && large != null && (
          <Fact
            label={t("ui.table_header.average_row")}
            value={bytes(averageRow)}
            warn={tableSignalWarning(report, table, ["large-rows"])}
            detail={<span title={t("ui.table_header.large_values_title")}>{t("ui.table_header.large_values", { percent: percent(large / own) })}</span>}
          />
        )}
        {knowsStats && (
          <Fact
            label={t("ui.table_header.statistics")}
            value={analyzed ? ago(analyzed) : t("ui.table_header.no_update")}
            warn={tableSignalWarning(report, table, ["stale-stats"])}
            detail={
              table.modifications_since_analyze
                ? t("ui.table_header.changes_since", { count: count(table.modifications_since_analyze) })
                : stats.length > 1
                  ? t("ui.table_header.oldest_of", { count: stats.length })
                  : null
            }
          />
        )}
        {table.seq_scans != null && (
          <Fact label={t("ui.table_header.full_scans")} value={count(table.seq_scans)} detail={table.index_scans != null ? t("ui.table_header.index_scans", { count: count(table.index_scans) }) : null} />
        )}
        {writes != null && writes > 0 && (
          <Fact label={t("ui.table_header.writes")} value={count(writes)} detail={t("ui.table_header.writes_split", { inserts: count(table.inserts), updates: count(table.updates), deletes: count(table.deletes) })} />
        )}
        <TableSignalFacts report={report} table={table} />
      </div>
      <HypertableChunks key={tableKey(table)} table={table} />
      {findings.length > 0 && (
        <div className="td-findings">
          {shown.map((f, n) => (
            <div key={f.id} className="td-finding-row">
              <button type="button" className="td-finding" onClick={() => setState({ view: "overview", finding: f.id })} title={t("ui.table_header.open_finding")}>
                <span className={`sev ${f.severity}`} />
                <span className="t">{f.title}</span>
                <ChevronRight size={13} className="muted" />
              </button>
              {n === 0 && findings.length > 1 && (
                <button type="button" className="td-more" onClick={() => setAll((a) => !a)} aria-expanded={all}>
                  {all ? t("ui.table_header.fewer") : t("ui.table_header.more", { count: findings.length - 1 })}
                </button>
              )}
            </div>
          ))}
        </div>
      )}
    </header>
  );
}

function HypertableChunks({ table }: { table: TableStat }) {
  const [shown, setShown] = useState(20);
  const hypertable = table.storage?.hypertable;
  if (!hypertable) return null;
  const chunks = hypertable.chunk_details;
  return (
    <details className="td-chunks">
      <summary>{hypertable.dimensions.length > 0 ? t("ui.table_header.chunks_by", { dimensions: hypertable.dimensions.join(", ") }) : t("ui.table_header.managed_chunks")}</summary>
      {chunks == null ? <p className="muted">{t("ui.table_header.chunks_unread")}</p> : (
        <>
          <div className="td-chunk-list">
            <table>
              <thead><tr><th>{t("ui.table_header.chunk")}</th><th>{t("common.metric.rows")}</th><th>{t("common.metric.size")}</th><th>{t("ui.table_header.storage")}</th></tr></thead>
              <tbody>{chunks.slice(0, shown).map((chunk) => <tr key={`${chunk.schema}.${chunk.name}`}>
                <td title={`${chunk.schema}.${chunk.name}`}>{chunk.name}</td>
                <td>{count(chunk.rows)}</td><td>{bytes(chunk.total_bytes)}</td>
                <td>{chunk.compressed ? t("ui.table_header.columnstore") : t("ui.table_header.rowstore")}</td>
              </tr>)}</tbody>
            </table>
          </div>
          {shown < chunks.length && <button type="button" className="btn small" onClick={() => setShown((n) => n + 20)}>{t("ui.table_header.show_more", { more: 20, remaining: chunks.length - shown })}</button>}
          <p className="muted">{t("ui.table_header.chunks_note")}</p>
        </>
      )}
    </details>
  );
}

function Fact({ label, value, detail, warn }: { label: string; value: string; detail?: ReactNode; warn?: boolean }) {
  return (
    <div className={`fact ${warn ? "warn" : ""}`}>
      <div className="l">{label}</div>
      <div className="v">{value}</div>
      {detail && <div className="d">{detail}</div>}
    </div>
  );
}
