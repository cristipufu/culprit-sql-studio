import type { ReactNode } from "react";
import type { Engine } from "../gen/Engine";
import type { FileIo } from "../gen/FileIo";
import type { Finding } from "../gen/Finding";
import type { QueryStat } from "../gen/QueryStat";
import type { Replica } from "../gen/Replica";
import type { Report } from "../gen/Report";
import type { TableStat } from "../gen/TableStat";
import { serverName, statementMetrics } from "../lib/engines";
import { bytes, count, duration, percent, rowCount, when } from "../lib/format";
import { key, t } from "../lib/i18n";
import { waitKey } from "../lib/waits";
import { querySignalStatus, replicaState, replicaStatus, storageStatus, tableSignalWarning, worstFinding } from "../lib/health";
import { SignalTile } from "./Health";

/**
 * Signals beyond run time: what a query costs besides its duration (failed runs, planning, JIT, lock waits, log, result
 * size), storage latency and replication for the server, and the hot spots of a table. Each block leads with one
 * sentence that says what matters, and says plainly what the engine doesn't measure.
 */

type Status = "good" | "watch" | "bad" | "info";
type Row = { key: string; label: string; value: string; note: string; status: Status; takeaway?: string };

const RANK: Record<Status, number> = { bad: 0, watch: 1, good: 2, info: 3 };

function engineName(report: Report): string {
  return serverName(report.snapshot.server.engine, report.snapshot.server.product);
}

function verb(text: string): string {
  return (text.trimStart().replace(/^\(+/, "").match(/^[a-z]+/i)?.[0] ?? "").toLowerCase();
}

function lockMs(q: QueryStat): number {
  return q.waits.filter((w) => waitKey(w.category) === "lock").reduce((sum, w) => sum + w.wait_ms, 0);
}

function costRows(report: Report, q: QueryStat): Row[] {
  const metrics = statementMetrics(report.snapshot.server.engine);
  const c = q.costs ?? null;
  const runs = Math.max(1, q.executions);
  const rows: Row[] = [];

  if (c && (c.failed != null || c.aborted != null)) {
    const aborted = c.aborted ?? 0;
    const failed = c.failed ?? 0;
    const rate = (aborted + failed) / runs;
    const timeout = c.aborted_mean_ms != null && [15, 30, 60, 120].some((s) => Math.abs(c.aborted_mean_ms! / 1000 - s) <= s * 0.1);
    const parts: string[] = [];
    if (aborted > 0) {
      const mean = c.aborted_mean_ms;
      parts.push(
        mean == null
          ? t("ui.beyond_time.cancelled", { count: count(aborted) })
          : timeout
            ? t("ui.beyond_time.cancelled_timeout", { count: count(aborted), duration: duration(mean) })
            : t("ui.beyond_time.cancelled_after", { count: count(aborted), duration: duration(mean) }),
      );
    }
    if (failed > 0) parts.push(c.top_error ? t("ui.beyond_time.errors_recently", { n: failed, count: count(failed), error: c.top_error }) : t("ui.beyond_time.errors", { n: failed, count: count(failed) }));
    if (c.warnings) parts.push(t("ui.beyond_time.warnings", { n: c.warnings, count: count(c.warnings) }));
    const status = "info" as const;
    // Why runs don't finish, when the evidence says: a client timeout, else the recent error.
    const cause = timeout ? "timeout" : failed > 0 && c.top_error ? "error" : "other";
    const error = c.top_error ?? "";
    rows.push({
      key: "unfinished",
      label: t("ui.beyond_time.unfinished"),
      value: aborted + failed === 0 ? t("ui.beyond_time.none") : t("ui.beyond_time.of_runs", { share: percent(rate) }),
      note: parts.length ? parts.join(" · ") : t("ui.beyond_time.every_run_finished"),
      status,
      takeaway: rate >= 0.999 ? t("ui.beyond_time.takeaway.none_finish", { cause, error }) : t("ui.beyond_time.takeaway.some_unfinished", { cause, error, share: percent(rate) }),
    });
  }

  if (c?.plan_ms != null) {
    const share = c.plan_ms / (c.plan_ms + q.total_time_ms);
    const plans = Math.max(1, c.plans ?? 1);
    rows.push({
      key: "planning",
      label: t(metrics.planning_label),
      value: t("ui.beyond_time.of_its_time", { share: percent(share) }),
      note: plans >= runs * 0.9 ? t("ui.beyond_time.plan_every_run", { duration: duration(c.plan_ms / plans) }) : t("ui.beyond_time.plan_reused", { duration: duration(c.plan_ms / plans), n: runs / plans, times: count(runs / plans) }),
      status: "info",
      takeaway: t("ui.beyond_time.takeaway.planning", { planning: metrics.planning, share: percent(share) }),
    });
  } else if (metrics.planning_setting) {
    const tracking = report.snapshot.settings.find((s) => s.name === metrics.planning_setting);
    if (tracking && tracking.value !== "on")
      rows.push({ key: "planning", label: t(metrics.planning_label), value: t("ui.beyond_time.not_measured"), note: t("ui.beyond_time.setting_off", { setting: metrics.planning_setting }), status: "info" });
  }

  if (metrics.compilation_history && c?.plan_ms == null) {
    const history = c?.compile_history;
    rows.push({
      key: "compile-history", label: t("ui.beyond_time.compile_history"),
      value: history ? t("ui.beyond_time.compiles", { n: history.count, count: count(history.count), duration: duration(history.total_ms) }) : t("ui.beyond_time.compiles_unmeasured"),
      note: history ? t("ui.beyond_time.compile_history_note") : t("ui.beyond_time.compile_history_missing"),
      status: "info",
    });
  }

  if (c?.jit_ms != null && q.total_time_ms > 0) {
    const share = c.jit_ms / q.total_time_ms;
    rows.push({
      key: "jit",
      label: t("ui.beyond_time.jit"),
      value: t("ui.beyond_time.of_run_time", { share: percent(share) }),
      note: c.jit_functions ? t("ui.beyond_time.jit_per_run_functions", { duration: duration(c.jit_ms / runs), n: c.jit_functions, functions: count(c.jit_functions) }) : t("ui.beyond_time.per_run", { amount: duration(c.jit_ms / runs) }),
      status: "info",
      takeaway: share >= 0.5 ? t("ui.beyond_time.takeaway.jit_longer") : t("ui.beyond_time.takeaway.jit", { share: percent(share) }),
    });
  }

  const lock = lockMs(q);
  if (lock > 0 && metrics.lock_time) {
    const share = Math.min(1, lock / Math.max(1, q.total_time_ms));
    rows.push({
      key: "lock",
      label: t("ui.beyond_time.lock_waits"),
      value: t("ui.beyond_time.of_its_time", { share: percent(share) }),
      note: t("ui.beyond_time.lock_note", { duration: duration(lock / runs) }),
      status: "info",
      takeaway: t("ui.beyond_time.takeaway.lock", { share: percent(share) }),
    });
  }

  const writes = ["insert", "update", "delete", "merge", "copy", "replace"].includes(verb(q.text));
  if (c?.log_bytes != null && (writes || c.log_bytes / runs >= 65_536)) {
    // Which log the engine writes, for its messages to say it in the reader's words (`{log, select, wal {..} other {..}}`).
    const log = metrics.log;
    const perRow = q.rows != null && q.rows >= 1 ? c.log_bytes / q.rows : null;
    rows.push({
      key: "log",
      label: t(metrics.log_label),
      value: t("ui.beyond_time.per_run", { amount: bytes(c.log_bytes / runs) }),
      note: [c.log_share != null ? t("ui.beyond_time.of_all_log", { share: percent(c.log_share), log }) : null, perRow != null ? t("ui.beyond_time.per_row", { size: bytes(perRow) }) : null].filter(Boolean).join(" · ") || "",
      status: "info",
      takeaway: perRow != null && perRow >= 4096 ? t("ui.beyond_time.takeaway.log_per_row", { size: bytes(perRow), log }) : t("ui.beyond_time.takeaway.log_share", { share: percent(c.log_share ?? 0), log }),
    });
  }

  // Small results are the norm; the row earns its place from a thousand rows a run.
  if (q.rows != null && q.rows / runs >= 1_000 && ["select", "with", "values", "table"].includes(verb(q.text))) {
    const perRun = q.rows / runs;
    const width = c?.row_bytes ?? null;
    rows.push({
      key: "result",
      label: t("ui.beyond_time.result"),
      value: width ? t("ui.beyond_time.result_size", { rows: rowCount(perRun), size: bytes(perRun * width) }) : rowCount(perRun),
      note: width ? t("ui.beyond_time.result_width", { size: bytes(width) }) : t("ui.beyond_time.result_per_run"),
      status: "info",
      takeaway: width ? t("ui.beyond_time.takeaway.result_size", { rows: rowCount(perRun), size: bytes(perRun * width) }) : t("ui.beyond_time.takeaway.result", { rows: rowCount(perRun) }),
    });
  }
  const checks: Record<string, string[]> = {
    unfinished: ["failing-query"], planning: ["planning-time"], jit: ["jit-overhead"],
    lock: ["lock-time"], log: ["log-volume"], result: ["big-result"],
  };
  return rows.map((row) => ({ ...row, status: querySignalStatus(report, q, checks[row.key] ?? []) }));
}

function notKept(report: Report, q: QueryStat): string | null {
  const metrics = statementMetrics(report.snapshot.server.engine);
  const note = metrics.missing_costs_note ?? ((q.costs?.failed ?? 0) > 0 ? metrics.errors_note : null);
  return note ? t(note, { engine: engineName(report) }) : null;
}

/** The query detail's "beyond run time" block: one sentence, then one row per signal the engine keeps. */
export function QueryCostsPanel({ report, query }: { report: Report; query: QueryStat }) {
  const rows = costRows(report, query);
  if (rows.length === 0) return null;
  const worst = [...rows].sort((a, b) => RANK[a.status] - RANK[b.status])[0]!;
  // A warning leads with its takeaway; a row without one (a signal that wasn't measured) leads with the plain line.
  const lead = (worst.status === "bad" || worst.status === "watch" ? worst.takeaway : null) ?? t("ui.beyond_time.recorded_costs");
  const footer = notKept(report, query);
  return (
    <section className="beyond">
      <h3>{t("ui.beyond_time.title")}</h3>
      <p className={`beyond-lead ${worst.status}`}>{lead}</p>
      <div className="beyond-rows" role="list">
        {rows.map((r) => (
          <div key={r.key} className="beyond-row" role="listitem">
            <i className={`sig-dot ${r.status}`} aria-hidden />
            <span className="beyond-label">{r.label}</span>
            <span className="beyond-value">{r.value}</span>
            <span className="beyond-note">{r.note}</span>
          </div>
        ))}
      </div>
      {footer && <p className="beyond-foot">{footer}</p>}
    </section>
  );
}

// ── storage and replication ─────────────────────────────────────────────────────────────────────

function kindLabel(kind: FileIo["kind"]): string {
  switch (kind) {
    case "data":
      return t("ui.beyond_time.file_kind.data");
    case "log":
      return t("ui.beyond_time.file_kind.log");
    case "temp":
      return t("ui.beyond_time.file_kind.temp");
    case "other":
      return t("ui.beyond_time.file_kind.other");
  }
}

/** Storage latency in µs below a tenth of a millisecond, where cache hits and fast SSDs live. */
function latency(ms: number | null): string {
  if (ms == null) return "—";
  return ms < 0.1 ? `${Math.round(ms * 1000)} µs` : duration(ms);
}

function avg(ms: number | null, n: number): number | null {
  return ms != null && n > 0 ? ms / n : null;
}

function fileName(f: FileIo): string {
  const name = f.database && !f.name.startsWith(f.database) ? `${f.database} · ${f.name}` : f.name;
  const scope = f.scope === "server" ? t("ui.beyond_time.scope.server") : f.scope === "database" ? (f.database ?? t("ui.beyond_time.scope.this_database")) : t("ui.beyond_time.scope.unrecorded");
  return scope ? t("ui.beyond_time.file_scope", { file: name, scope }) : name;
}

function storageLead(files: FileIo[], engine: Engine, report: Report): { text: string; status: Status } {
  const timed = files.filter((f) => f.read_ms != null || f.write_ms != null);
  if (timed.length === 0) {
    return engine === "postgres"
      ? { text: t("ui.beyond_time.storage.untimed_postgres"), status: "info" }
      : { text: t("ui.beyond_time.storage.untimed"), status: "info" };
  }
  const slow = [...files].map((f) => ({ f, s: storageStatus(report, f) })).sort((a, b) => RANK[a.s] - RANK[b.s])[0];
  if (slow && (slow.s === "bad" || slow.s === "watch")) {
    const f = slow.f;
    const finding = worstFinding(report, (f) => f.check === "slow-storage");
    return { text: finding ? t("ui.beyond_time.finding_lead", { title: finding.title }) : t("ui.beyond_time.storage.recorded", { file: fileName(f) }), status: slow.s };
  }
  const reads = files.filter((f) => f.kind !== "log" && f.read_ms != null).reduce((a, f) => ({ ms: a.ms + (f.read_ms ?? 0), n: a.n + f.reads }), { ms: 0, n: 0 });
  const logs = files.filter((f) => f.kind === "log" && f.write_ms != null).reduce((a, f) => ({ ms: a.ms + (f.write_ms ?? 0), n: a.n + f.writes }), { ms: 0, n: 0 });
  const kind = files.some((f) => f.kind === "log" && f.write_operation === "flush") ? "flushes" : "writes";
  const read = reads.n > 0 ? latency(reads.ms / reads.n) : null;
  const log = logs.n > 0 ? latency(logs.ms / logs.n) : null;
  const text =
    read != null && log != null
      ? t("ui.beyond_time.storage.reads_and_log", { reads: read, kind, log })
      : read != null
        ? t("ui.beyond_time.storage.reads", { reads: read })
        : log != null
          ? t("ui.beyond_time.storage.log", { kind, log })
          : t("ui.beyond_time.storage.none_timed");
  return { text, status: "info" };
}

/** One file's average time for one kind of operation (`reads`, `writes` or `flushes`), with how many it averages. */
function Latency({ ms, n, kind }: { ms: number | null; n: number; kind: "reads" | "writes" | "flushes" }) {
  if (n <= 0) return <span className="io-cell muted">—</span>;
  const a = avg(ms, n);
  return (
    <span className="io-cell" title={ms != null ? t("ui.beyond_time.latency_total", { n, count: count(n), kind, total: duration(ms) }) : t("ui.beyond_time.latency_untimed", { n, count: count(n), kind })}>
      <b>{a == null ? t("ui.beyond_time.untimed") : latency(a)}</b>
      <span className="muted"> × {count(n)}</span>
    </span>
  );
}

/** Average read and write time per file since startup, slowest first, under one sentence. */
export function StorageLatency({ files, engine, report, compact }: { files: FileIo[]; engine: Engine; report: Report; compact?: boolean }) {
  if (files.length === 0) return null;
  const lead = storageLead(files, engine, report);
  const written = files.filter((f) => f.writes > 0);
  const hasFlushes = written.some((f) => f.write_operation === "flush");
  const writeHeading = hasFlushes ? (written.every((f) => f.write_operation === "flush") ? t("ui.beyond_time.flushes") : t("ui.beyond_time.writes_flushes")) : t("ui.beyond_time.writes");
  return (
    <section className="io-panel">
      {!compact && (
        <div className="io-head">
          <h4>{t("ui.beyond_time.storage.title")}</h4>
          <span className="muted">
            {engine === "postgres" ? t("ui.beyond_time.storage.about_postgres") : t("ui.beyond_time.storage.about")}
          </span>
        </div>
      )}
      <p className={`beyond-lead ${lead.status}`}>{lead.text}</p>
      <div className="io-table" role="table" aria-label={t("ui.beyond_time.storage.label")}>
        <IoRow className="io-th">
          <IoHeader>{t("ui.beyond_time.file")}</IoHeader>
          <IoHeader>{t("ui.beyond_time.reads")}</IoHeader>
          <IoHeader>{writeHeading}</IoHeader>
        </IoRow>
        {files.slice(0, compact ? 8 : 12).map((f) => (
          <IoRow key={`${f.database ?? ""}:${f.name}`}>
            <span className="io-name" role="cell">
              <i className={`sig-dot ${storageStatus(report, f)}`} aria-hidden />
              <span className="io-file" title={f.stats_reset ? t("ui.beyond_time.stats_since", { file: fileName(f), reset: f.stats_reset }) : t("ui.beyond_time.stats_unrecorded", { file: fileName(f) })}>
                {fileName(f)}
              </span>
              <span className={`io-kind ${f.kind}`}>{kindLabel(f.kind)}</span>
            </span>
            <Latency ms={f.read_ms} n={f.reads} kind="reads" />
            <Latency ms={f.write_ms} n={f.writes} kind={f.write_operation === "flush" ? "flushes" : "writes"} />
          </IoRow>
        ))}
      </div>
    </section>
  );
}

function roleLabel(role: Replica["role"]): string {
  switch (role) {
    case "replica":
      return t("ui.beyond_time.role.replica");
    case "this-server":
      return t("ui.beyond_time.role.this_server");
    case "slot":
      return t("ui.beyond_time.role.slot");
  }
}

/** Replicas, this server's own lag when it is one, and replication slots. */
export function Replication({ replicas, report }: { replicas: Replica[]; report: Report }) {
  if (replicas.length === 0) return null;
  const worst = [...replicas].sort((a, b) => RANK[replicaStatus(report, a)] - RANK[replicaStatus(report, b)])[0]!;
  const ws = replicaStatus(report, worst);
  const finding = worstFinding(report, (f) => f.check === "replica-lag");
  const lead = finding ? t("ui.beyond_time.finding_lead", { title: finding.title }) : t("ui.beyond_time.replication.recorded");
  return (
    <section className="io-panel">
      <div className="io-head">
        <h4>{t("ui.beyond_time.replication.title")}</h4>
      </div>
      <p className={`beyond-lead ${ws}`}>{lead}</p>
      <div className="io-table" role="table" aria-label={t("ui.beyond_time.replication.label")}>
        {replicas.map((r) => (
          <IoRow key={`${r.role}:${r.name}`} className="repl">
            <span className="io-name" role="cell">
              <i className={`sig-dot ${replicaStatus(report, r)}`} aria-hidden />
              <span className="io-file">{r.name}</span>
              <span className="io-kind">{roleLabel(r.role)}</span>
            </span>
            <span className="io-cell" role="cell">
              {[replicaState(r), r.sync].filter(Boolean).join(" · ") || "—"}
            </span>
            <span className="io-cell" role="cell">
              {[
                r.lag_seconds != null ? t("ui.beyond_time.replication.behind", { duration: duration(r.lag_seconds * 1000) }) : null,
                r.lag_bytes != null ? (r.role === "slot" ? t("ui.beyond_time.replication.kept", { size: bytes(r.lag_bytes) }) : t("ui.beyond_time.replication.to_apply", { size: bytes(r.lag_bytes) })) : null,
                r.unsent_bytes ? t("ui.beyond_time.replication.unsent", { size: bytes(r.unsent_bytes) }) : null,
                r.error,
              ]
                .filter(Boolean)
                .join(" · ") || "—"}
            </span>
          </IoRow>
        ))}
      </div>
    </section>
  );
}

// ── table hot spots ─────────────────────────────────────────────────────────────────────────────

function Fact({ label, value, detail, warn, title }: { label: string; value: string; detail?: ReactNode; warn?: boolean; title?: string }) {
  return (
    <div className={`fact ${warn ? "warn" : ""}`} title={title}>
      <div className="l">{label}</div>
      <div className="v">{value}</div>
      {detail && <div className="d">{detail}</div>}
    </div>
  );
}

/** Facts for a table's header: how updates land, empty space, lock and latch waits, untrusted constraints, histograms. */
export function TableSignalFacts({ report, table }: { report: Report; table: TableStat }) {
  const { snapshot } = report;
  const facts: ReactNode[] = [];
  const s = table.storage ?? null;
  if (s?.docdb) {
    const native = s.docdb;
    facts.push(<Fact key="docdb-tablets" label={t("ui.beyond_time.docdb.tablets")} value={native.tablets == null ? t("ui.beyond_time.unknown") : count(native.tablets)} detail={native.colocated ? t("ui.beyond_time.docdb.colocated") : t("ui.beyond_time.docdb.distributed")} />);
    facts.push(<Fact key="docdb-sharding" label={t("ui.beyond_time.docdb.sharding")} value={native.hash_key_columns == null ? t("ui.beyond_time.unknown") : native.hash_key_columns > 0 ? t("ui.beyond_time.docdb.hash_columns", { n: native.hash_key_columns }) : t("ui.beyond_time.docdb.range")} detail={native.colocated ? t("ui.beyond_time.docdb.shared_size") : t("ui.beyond_time.docdb.no_heap")} />);
  }
  if (s?.hot_updates != null && (table.updates ?? 0) >= 100) {
    const ratio = s.hot_updates / Math.max(1, table.updates ?? 1);
    facts.push(
      <Fact
        key="hot"
        label={t("ui.beyond_time.hot.label")}
        value={percent(ratio)}
        warn={tableSignalWarning(report, table, ["hot-updates"])}
        detail={s.new_page_updates != null ? t("ui.beyond_time.hot.fillfactor_moved", { fillfactor: String(s.fillfactor ?? 100), moved: count(s.new_page_updates) }) : t("ui.beyond_time.hot.fillfactor", { fillfactor: String(s.fillfactor ?? 100) })}
        title={t("ui.beyond_time.hot.hint")}
      />,
    );
  }
  if (s?.bloat_bytes != null && s.bloat_bytes > 0) {
    const size = table.data_bytes ?? table.total_bytes ?? 0;
    const share = size > 0 ? s.bloat_bytes / size : 0;
    facts.push(
      <Fact
        key="bloat"
        label={t("ui.beyond_time.bloat.label")}
        value={`≈ ${bytes(s.bloat_bytes)}`}
        warn={tableSignalWarning(report, table, ["table-bloat"])}
        detail={t("ui.beyond_time.bloat.share", { share: percent(share) })}
        title={t("ui.beyond_time.bloat.hint")}
      />,
    );
  }
  const contended = snapshot.indexes.filter((i) => i.schema === table.schema && i.table === table.name && i.contention);
  if (contended.length) {
    const lockMs = contended.reduce((a, i) => a + i.contention!.row_lock_wait_ms + i.contention!.page_lock_wait_ms, 0);
    const waits = contended.reduce((a, i) => a + i.contention!.row_lock_waits + i.contention!.page_lock_waits, 0);
    const worst = [...contended].sort((a, b) => b.contention!.row_lock_wait_ms - a.contention!.row_lock_wait_ms)[0]!;
    if (waits > 0)
      facts.push(
        <Fact
          key="locks"
          label={t("ui.beyond_time.lock_waits")}
          value={duration(lockMs)}
          warn={tableSignalWarning(report, table, ["hot-rows", "lock-escalations"])}
          detail={t("ui.beyond_time.locks.detail", { n: waits, waits: count(waits), index: worst.name })}
          title={t("ui.beyond_time.locks.hint")}
        />,
      );
    const latch = contended.reduce((a, i) => a + i.contention!.page_latch_wait_ms, 0);
    const sequential = contended.find((i) => i.contention!.sequential_key && i.contention!.page_latch_waits > 0);
    if (latch >= 1_000)
      facts.push(
        <Fact
          key="latch"
          label={t("ui.beyond_time.latch.label")}
          value={duration(latch)}
          warn={tableSignalWarning(report, table, ["last-page-insert"])}
          detail={sequential ? t("ui.beyond_time.latch.last_page", { index: sequential.name }) : t("ui.beyond_time.latch.hot_pages")}
        />,
      );
  }
  const untrusted = snapshot.constraints.filter((c) => c.schema === table.schema && c.table === table.name);
  if (untrusted.length)
    facts.push(
      <Fact
        key="constraints"
        label={snapshot.server.engine === "postgres" ? t("ui.beyond_time.constraints.not_validated") : t("ui.beyond_time.constraints.untrusted")}
        value={t("ui.beyond_time.constraints.count", { n: untrusted.length })}
        warn
        detail={untrusted.map((c) => c.name).join(", ")}
      />,
    );
  if (snapshot.histograms != null) {
    const columns = snapshot.histograms.filter((h) => h.table.toLowerCase() === table.name.toLowerCase());
    // MariaDB keeps statistics for a column with or without a histogram; one without says so by having no kind.
    facts.push(
      <Fact
        key="histograms"
        label={columns.some((h) => h.kind == null) ? t("ui.beyond_time.histograms.column_statistics") : t("ui.beyond_time.histograms.label")}
        value={columns.length ? t("ui.beyond_time.histograms.columns", { n: columns.length }) : t("ui.beyond_time.none")}
        detail={columns.length ? columns.map((h) => h.column).join(", ") : t("ui.beyond_time.histograms.none")}
      />,
    );
  }
  return <>{facts}</>;
}

// ── finding evidence ────────────────────────────────────────────────────────────────────────────

const CHURN = ["new-connections", "session-length", "sessions-abandoned", "pooled-reuse", "thread-cache-misses", "aborted-connects", "aborted-clients"];

/** The data behind server and object findings, which have no query to show. */
/** The metric the lock-escalations check counts new escalations under, when an earlier scan is there to compare with. */
const ESCALATIONS_SINCE = key("check.lock_escalations.metric.since_previous");

/** SQL Server counts escalations from when it started, with no time for each: what the counts cover, and how many came since the scan before. */
function escalationsSince(startedAt: string | null, previousScan: string | undefined, finding: Finding): string {
  const counted = startedAt
    ? t("ui.beyond_time.escalations.since_start", { started: when(startedAt).short })
    : t("ui.beyond_time.escalations.since_load");
  const since = finding.metrics.find((m) => m.key === ESCALATIONS_SINCE)?.value;
  if (since === undefined || !previousScan) return `${counted} ${t("ui.beyond_time.escalations.next_scan")}`;
  const at = when(previousScan).short;
  return since > 0
    ? `${counted} ${t("ui.beyond_time.escalations.since_previous", { count: count(since), at })}`
    : `${counted} ${t("ui.beyond_time.escalations.none_since_previous", { at })}`;
}

export function SignalEvidence({ report, finding }: { report: Report; finding: Finding }) {
  const { snapshot } = report;
  const p = snapshot.pressure ?? null;
  switch (finding.check) {
    case "slow-storage":
      return p?.files?.length ? (
        <section>
          <h3>{t("ui.beyond_time.evidence")}</h3>
          <StorageLatency files={p.files} engine={snapshot.server.engine} report={report} compact />
        </section>
      ) : null;
    case "replica-lag":
      return p?.replicas?.length ? (
        <section>
          <h3>{t("ui.beyond_time.evidence")}</h3>
          <Replication replicas={p.replicas} report={report} />
        </section>
      ) : null;
    case "connection-churn": {
      const signals = (p?.signals ?? []).filter((s) => CHURN.includes(s.key));
      return signals.length ? (
        <section>
          <h3>{t("ui.beyond_time.evidence")}</h3>
          <div className="signal-grid compact">
            {signals.map((s) => (
              <SignalTile key={s.key} signal={s} />
            ))}
          </div>
        </section>
      ) : null;
    }
    case "lock-escalations": {
      const indexes = snapshot.indexes.filter((i) => {
        const c = i.contention;
        return `${i.schema}.${i.table}` === finding.subject.name && c && c.lock_escalations > 0;
      });
      if (!indexes.length) return null;
      const table = indexes[0]!.table;
      return (
        <section>
          <h3>{t("ui.beyond_time.evidence")}</h3>
          <div className="io-table" role="table" aria-label={t("ui.beyond_time.escalations.label", { table })}>
            <IoRow className="io-th">
              <IoHeader>{t("ui.beyond_time.on", { object: table })}</IoHeader>
              <IoHeader>{t("ui.beyond_time.escalations.table_locks")}</IoHeader>
              <IoHeader>{t("ui.beyond_time.lock_waits")}</IoHeader>
            </IoRow>
            {indexes.map((i) => {
              const c = i.contention!;
              const waits = c.row_lock_waits + c.page_lock_waits;
              return (
                <IoRow key={i.name}>
                  <span className="io-name" role="cell">
                    {i.name}
                  </span>
                  <span className="io-cell" role="cell">
                    {c.lock_escalations > 0 ? <b>{count(c.lock_escalations)}</b> : t("ui.beyond_time.none")}
                  </span>
                  <span className="io-cell" role="cell">
                    {waits > 0 ? `${count(waits)}, ${duration(c.row_lock_wait_ms + c.page_lock_wait_ms)}` : t("ui.beyond_time.none")}
                  </span>
                </IoRow>
              );
            })}
          </div>
          <p className="beyond-foot">
            {t("ui.beyond_time.escalations.explain")}{" "}
            {escalationsSince(snapshot.server.started_at, snapshot.escalations_before?.taken_at, finding)}
          </p>
        </section>
      );
    }
    case "hot-rows":
    case "last-page-insert": {
      const index = snapshot.indexes.find((i) => `${i.schema}.${i.table}.${i.name}` === finding.subject.name);
      const c = index?.contention;
      if (!index || !c) return null;
      const rows: [string, number, number][] = [
        [t("ui.beyond_time.waits.row_locks"), c.row_lock_waits, c.row_lock_wait_ms],
        [t("ui.beyond_time.waits.page_locks"), c.page_lock_waits, c.page_lock_wait_ms],
        [t("ui.beyond_time.waits.page_latches"), c.page_latch_waits, c.page_latch_wait_ms],
      ];
      return (
        <section>
          <h3>{t("ui.beyond_time.evidence")}</h3>
          <div className="io-table" role="table" aria-label={t("ui.beyond_time.waits.label", { index: index.name })}>
            <IoRow className="four io-th">
              <IoHeader>{t("ui.beyond_time.on", { object: index.name })}</IoHeader>
              <IoHeader>{t("ui.beyond_time.waits.waits")}</IoHeader>
              <IoHeader>{t("ui.beyond_time.waits.in_all")}</IoHeader>
              <IoHeader>{t("ui.beyond_time.waits.average")}</IoHeader>
            </IoRow>
            {rows.map(([label, n, ms]) => (
              <IoRow key={label} className={`four ${n > 0 ? "" : "quiet"}`}>
                <span className="io-name" role="cell">
                  {label}
                </span>
                <span className="io-cell" role="cell">
                  {n > 0 ? count(n) : t("ui.beyond_time.none")}
                </span>
                <span className="io-cell" role="cell">
                  {n > 0 ? duration(ms) : "—"}
                </span>
                <span className="io-cell" role="cell">
                  {n > 0 ? <b>{duration(ms / n)}</b> : "—"}
                </span>
              </IoRow>
            ))}
          </div>
          <p className="beyond-foot">
            {c.lock_escalations > 0 ? `${t("ui.beyond_time.waits.escalations", { n: c.lock_escalations, count: count(c.lock_escalations) })} ` : ""}
            {c.sequential_key ? `${index.columns[0] != null ? t("ui.beyond_time.waits.column_grows", { column: index.columns[0] }) : t("ui.beyond_time.waits.key_grows")} ` : ""}
            {t("ui.beyond_time.waits.since_loaded")}
          </p>
        </section>
      );
    }
    default:
      return null;
  }
}

/** A row of a static table (role="table"): read, never focused or operated, unlike a grid's row. */
function IoRow({ className, children }: { className?: string; children: ReactNode }) {
  return (
    // biome-ignore lint/a11y/useFocusableInteractive: a static table's row; only a grid's rows take focus
    <div className={className ? `io-row ${className}` : "io-row"} role="row">
      {children}
    </div>
  );
}

/** A column header of a static table. */
function IoHeader({ children }: { children: ReactNode }) {
  return (
    // biome-ignore lint/a11y/useFocusableInteractive: a static table's column header; only a grid's headers take focus
    <span role="columnheader">{children}</span>
  );
}
