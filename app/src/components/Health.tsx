import { evidencePresentation } from "../lib/engines";
import { useState, type MouseEvent } from "react";
import type { Engine } from "../gen/Engine";
import type { OpenTransaction } from "../gen/OpenTransaction";
import type { Pressure } from "../gen/Pressure";
import type { Report } from "../gen/Report";
import type { SignalStatus } from "../gen/SignalStatus";
import type { ResourceSample } from "../gen/ResourceSample";
import type { Signal } from "../gen/Signal";
import { dateFormat, duration, percent, percentValue } from "../lib/format";
import { findingStatus, transactionState, worstFinding } from "../lib/health";
import { t } from "../lib/i18n";
import { tx } from "../lib/message-parts";
import { SIGNAL_SCOPE, signalValue } from "../lib/waits";
import { Replication, StorageLatency } from "./BeyondTime";
import { OperationalHealth, hasOperationalHealth } from "./OperationalHealth";
import { MemoryFailureSourceIssue, RetainedMemoryFailures, memoryFailureReadIssue } from "./RetainedMemoryFailures";
import { NativeFailureSourceIssue, RetainedNativeFailures, nativeFailureReadIssue } from "./RetainedNativeFailures";

const RANK: Record<Signal["status"], number> = { bad: 0, watch: 1, good: 2, info: 3 };


/** A signal: label, value, when it was measured, and what healthy looks like, in words. */
export function SignalTile({ signal }: { signal: Signal }) {
  return (
    <div className={`signal-tile ${signal.status}`}>
      <div className="signal-head">
        <span className="signal-label" title={signal.label}>{signal.label}</span>
        {signal.status !== "info" && <i className={`sig-dot ${signal.status}`} title={signal.status === "good" ? t("ui.health.status.good") : signal.status === "watch" ? t("ui.health.status.watch") : t("ui.health.status.bad")} />}
      </div>
      <div className="signal-value">{signalValue(signal)}</div>
      <div className="signal-meta">
        <span className="scope">{SIGNAL_SCOPE[signal.scope]}</span>
        {signal.healthy && <span className="healthy">{signal.healthy}</span>}
      </div>
    </div>
  );
}

function OpenTransactionCard({ txn, status }: { txn: OpenTransaction; status: SignalStatus }) {
  // A replication slot has no start time: how far it holds the horizon back is what tells.
  const slot = txn.holder === "replication-slot";
  const behind = txn.xid_age ?? 0;
  const session = <b>{txn.session}</b>;
  const long = status === "bad" || status === "watch";
  return (
    <div className={`open-txn ${long ? "long" : ""}`}>
      <i className={`sig-dot ${status}`} />
      <div>
        <div className="open-txn-title">
          {slot
            ? tx("ui.health.slot_holds", { cleanup: txn.catalog_only ? "catalog" : "all", count: Math.round(behind) }, { slot: session })
            : txn.holder === "prepared-transaction"
              ? tx("ui.health.oldest_prepared", { duration: duration(txn.seconds * 1000) }, { session })
              : tx("ui.health.oldest_open", { duration: duration(txn.seconds * 1000) }, { session })}
          {transactionState(txn) ? ` · ${transactionState(txn)}` : ""}
        </div>
        <div className="open-txn-meta">
          {txn.program ? `${t("ui.health.from_program", { program: txn.program })} ` : ""}
          {txn.held_bytes ? `${t("ui.health.version_store", { mb: String(Math.round(txn.held_bytes / 1024 / 1024)) })} ` : ""}
          {long ? t("ui.health.see_finding") : t("ui.health.recorded")}
        </div>
      </div>
    </div>
  );
}

export function HealthTab({
  pressure,
  resources,
  report,
  engineLabel,
  engine,
}: {
  pressure: Pressure | null;
  resources: ResourceSample[];
  report: Report;
  engineLabel: string;
  engine?: Engine;
}) {
  const signals = [...(pressure?.signals ?? [])].sort((a, b) => RANK[a.status] - RANK[b.status]);
  const memoryFailures = report.snapshot.operations.memory_failures;
  const memoryReport = report.memory_failures;
  const memoryIssue = memoryFailureReadIssue(memoryFailures?.source, report.snapshot.sources ?? [], report.snapshot.options?.collect_memory_failures === true);
  const nativeFailures = report.snapshot.operations.native_failures;
  const nativeReport = report.native_failures;
  const nativeIssue = nativeFailureReadIssue(nativeFailures?.source, report.snapshot.sources ?? [], report.snapshot.options?.collect_native_failures === true);
  if (!signals.length && !resources.length && !hasOperationalHealth(report) && !(memoryFailures && memoryReport) && !memoryIssue && !(nativeFailures && nativeReport) && !nativeIssue) {
    return (
      <div className="waits-empty">
        <h3>{t("ui.health.empty")}</h3>
        <p>
          {t(evidencePresentation(engine ?? report.snapshot.server.engine).health_empty)}
        </p>
      </div>
    );
  }
  const uptime = pressure?.uptime_hours;
  return (
    <div className="health-tab">
      {signals.length > 0 && (
        <>
          <div className="health-intro">
            {t("ui.health.intro")} {uptime != null ? `${t("ui.health.since_start", { duration: duration(uptime * 3_600_000), engine: engineLabel })} ` : ""}
            {t("ui.health.coverage")}
          </div>
          <div className="signal-grid">
            {signals.filter((s) => s.scope !== "since-startup" && s.scope !== "query-history").map((s) => <SignalTile key={s.key} signal={s} />)}
          </div>
          {signals.some((s) => s.scope === "since-startup" || s.scope === "query-history") && (
            <details className="historical-signals">
              <summary>{t("ui.health.historical")}</summary>
              <div className="signal-grid">
                {signals.filter((s) => s.scope === "since-startup" || s.scope === "query-history").map((s) => <SignalTile key={s.key} signal={s} />)}
              </div>
            </details>
          )}
        </>
      )}
      {report.snapshot.server.platform === "azure-sql-database" && !signals.some((s) => s.key === "cpu-use") && (
        <p className="muted">{t("ui.health.cpu_unmeasured")}</p>
      )}
      {pressure?.oldest_transaction && <OpenTransactionCard txn={pressure.oldest_transaction} status={findingStatus(worstFinding(report, (f) => f.check === "held-cleanup"))} />}
      {engine && <StorageLatency files={pressure?.files ?? []} engine={engine} report={report} />}
      <Replication replicas={pressure?.replicas ?? []} report={report} />
      <OperationalHealth report={report} />
      {memoryFailures && memoryReport && <RetainedMemoryFailures key={memoryFailures.observed_until} evidence={memoryFailures} report={memoryReport} />}
      {!(memoryFailures && memoryReport) && memoryIssue && <MemoryFailureSourceIssue source={memoryIssue} />}
      {nativeFailures && nativeReport && <RetainedNativeFailures key={nativeFailures.observed_until} evidence={nativeFailures} report={nativeReport} />}
      {!(nativeFailures && nativeReport) && nativeIssue && <NativeFailureSourceIssue source={nativeIssue} />}
      {resources.length >= 4 && <ResourceCharts samples={resources} signals={signals} />}
    </div>
  );
}

// ── resource limits (Azure SQL Database) ───────────────────────────────────────────────────────────

const VW = 600;
const VH = 120;
const LIMIT = 95;

type Pick = (s: ResourceSample) => number | null;

/** The tier's limits charted, each with the signal that grades it. */
function dimensions(): { key: string; label: string; pick: Pick }[] {
  return [
    { key: "log-write-use", label: t("ui.health.dimension.log_writes"), pick: (s) => s.log_write_pct },
    { key: "cpu-use", label: t("ui.health.dimension.cpu"), pick: (s) => s.cpu_pct },
    { key: "data-io", label: t("ui.health.dimension.data_reads"), pick: (s) => s.data_io_pct },
    { key: "workers-use", label: t("ui.health.dimension.workers"), pick: (s) => s.workers_pct },
  ];
}

const timeFmt = () => dateFormat({ timeZone: "UTC", hour: "2-digit", minute: "2-digit", hour12: false });

function ResourceCharts({ samples, signals }: { samples: ResourceSample[]; signals: Signal[] }) {
  const times = samples.map((s) => Date.parse(s.at));
  return (
    <section className="resource-charts">
      <div className="resource-head">
        <h4>{t("ui.health.limits")}</h4>
        <span className="muted">
          {t("ui.health.limits_about", { from: timeFmt().format(times[0]!), to: timeFmt().format(times[times.length - 1]!) })}
        </span>
      </div>
      <div className="resource-grid">
        {dimensions().map((d) => (
          <ResourceChart key={d.key} label={d.label} status={signals.find((s) => s.key === d.key)?.status ?? "info"} times={times} values={samples.map(d.pick)} />
        ))}
      </div>
    </section>
  );
}

function ResourceChart({ label, status, times, values }: { label: string; status: SignalStatus; times: number[]; values: (number | null)[] }) {
  const [hover, setHover] = useState<number | null>(null);
  const known = values.filter((v): v is number => v != null);
  if (known.length < 4) return null;
  const from = times[0]!;
  const to = times[times.length - 1]!;
  const x = (time: number) => ((time - from) / Math.max(1, to - from)) * VW;
  const y = (v: number) => VH - (Math.min(100, Math.max(0, v)) / 100) * VH;
  const points = values.map((v, i) => (v == null ? null : [x(times[i]!), y(v)] as const)).filter((p): p is readonly [number, number] => p != null);
  const line = points.map(([px, py], i) => `${i ? "L" : "M"}${px.toFixed(1)},${py.toFixed(1)}`).join("");
  const area = `${line}L${points[points.length - 1]![0]},${VH}L${points[0]![0]},${VH}Z`;
  const atLimit = known.filter((v) => v >= LIMIT).length / known.length;
  const peakIndex = values.reduce<number>((best, v, i) => (v != null && v > (values[best] ?? -1) ? i : best), 0);
  const peak = values[peakIndex] ?? 0;
  const ticks = [0, 0.25, 0.5, 0.75, 1].map((f) => from + f * (to - from));
  const onMove = (e: MouseEvent<HTMLDivElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    const at = from + ((e.clientX - box.left) / box.width) * (to - from);
    let best = 0;
    times.forEach((time, i) => {
      if (Math.abs(time - at) < Math.abs(times[best]! - at)) best = i;
    });
    setHover(best);
  };
  const hv = hover != null ? values[hover] : null;
  return (
    <div className="resource-chart">
      <div className="resource-chart-head">
        <span className="resource-label">
          <i className={`sig-dot ${status}`} /> {label}
        </span>
        <span className="resource-sentence">
          {atLimit >= 0.01
            ? tx("ui.health.at_limit", {}, { share: <b>{percent(atLimit)}</b> })
            : t("ui.health.recorded_peak", { peak: percentValue(peak) })}
        </span>
      </div>
      <div className="resource-plot" onMouseMove={onMove} onMouseLeave={() => setHover(null)}>
        <div className="resource-yaxis" aria-hidden>
          <span style={{ top: "0%" }}>100%</span>
          <span style={{ top: "50%" }}>50%</span>
          <span style={{ top: "100%" }}>0</span>
        </div>
        <div className="resource-canvas">
          <svg viewBox={`0 0 ${VW} ${VH}`} preserveAspectRatio="none" role="img" aria-label={t("ui.health.chart_label", { dimension: label, share: percent(atLimit), peak: percentValue(peak) })}>
            <line className="grid" x1="0" x2={VW} y1={y(50)} y2={y(50)} />
            <line className="grid" x1="0" x2={VW} y1={y(100)} y2={y(100)} />
            <line className="limit" x1="0" x2={VW} y1={y(LIMIT)} y2={y(LIMIT)} />
            <path className="area" d={area} />
            <path className="line" d={line} vectorEffect="non-scaling-stroke" />
            {hover != null && <line className="cursor" x1={x(times[hover]!)} x2={x(times[hover]!)} y1="0" y2={VH} vectorEffect="non-scaling-stroke" />}
          </svg>
          <span className="peak" style={{ left: `${(x(times[peakIndex]!) / VW) * 100}%`, top: `${(y(peak) / VH) * 100}%` }} title={t("ui.health.peak", { peak: percentValue(peak), time: timeFmt().format(times[peakIndex]!) })} />
          <span className="limit-label">95%</span>
          {hover != null && hv != null && (
            <span className="readout" style={{ left: `${(x(times[hover]!) / VW) * 100}%` }}>
              <b>{percentValue(hv)}</b> {timeFmt().format(times[hover]!)}
            </span>
          )}
        </div>
      </div>
      <div className="resource-xaxis" aria-hidden>
        {ticks.map((tick, i) => (
          <span key={i} style={{ left: `${i * 25}%` }}>
            {timeFmt().format(tick)}
          </span>
        ))}
      </div>
    </div>
  );
}
