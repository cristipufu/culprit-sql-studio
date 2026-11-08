import type { Report } from "../gen/Report";
import type { Actionability } from "../gen/Actionability";
import type { OperationalCounterKind } from "../gen/OperationalCounterKind";
import { bytes, count, percent } from "../lib/format";
import { t } from "../lib/i18n";
import { slot, withElements } from "../views/message-parts";
import { TerminationHistoryView } from "./TerminationHistory";
import { NodeResourcesView } from "./NodeResources";

export function hasOperationalHealth(report: Report): boolean {
  const o = report.snapshot.operations;
  return !!o && (!!o.spaces.length || !!o.counters.length || !!o.sequences.length || !!o.multixacts.length || !!o.sqlite || !!o.cluster || !!o.termination_history || !!o.node_resources || !!report.snapshot.sources?.some((s) => ["docdb-terminations", "docdb-node-resources"].includes(s.source) && s.state === "unavailable"));
}

const COUNTERS: Record<OperationalCounterKind, () => string> = {
  "archive-failures": () => t("ui.operational_health.counter.archive_failures"),
  "subscription-apply-errors": () => t("ui.operational_health.counter.subscription_apply_errors"),
  "subscription-sync-errors": () => t("ui.operational_health.counter.subscription_sync_errors"),
  "standby-conflicts": () => t("ui.operational_health.counter.standby_conflicts"),
  "redo-log-waits": () => t("ui.operational_health.counter.redo_log_waits"),
  "checksum-failures": () => t("ui.operational_health.counter.checksum_failures"),
  "connection-failures": () => t("ui.operational_health.counter.connection_failures"),
};

const ACTIONABILITY: Record<Actionability, () => string> = {
  "needs-attention": () => t("ui.operational_health.actionability.needs_attention"),
  "optional-improvement": () => t("ui.operational_health.actionability.optional_improvement"),
  information: () => t("ui.operational_health.actionability.information"),
  history: () => t("ui.operational_health.actionability.history"),
};

const scopeLabel = (scope: string) => (scope === "server" ? t("ui.operational_health.scope.server") : t("ui.operational_health.scope.database"));

/** Display the source's measurements and period. Findings determine whether they need attention. */
export function OperationalHealth({ report }: { report: Report }) {
  if (!hasOperationalHealth(report)) return null;
  const o = report.snapshot.operations;
  const issues = report.findings.filter((f) => ["storage-capacity", "sequence-capacity", "multixact-age", "operational-errors", "cluster-health", "sqlite-wal"].includes(f.check));
  const flagged = new Set(issues.map((f) => f.subject.name));
  const sequences = [...o.sequences].sort((a, b) => Number(flagged.has(b.name)) - Number(flagged.has(a.name))).slice(0, 100);
  const multixacts = [...o.multixacts].sort((a, b) => b.age - a.age).slice(0, 100);
  return <section className="operational-health">
    <h3>{t("ui.operational_health.title")}</h3>
    <p className="muted">{t("ui.operational_health.intro")}</p>
    {o.node_resources ? <NodeResourcesView key={o.node_resources.observed_until} resources={o.node_resources} /> : report.snapshot.sources?.filter((s) => s.source === "docdb-node-resources" && s.state === "unavailable").map((s) => <div className="operational-card" key={s.source}><h4>{t("ui.operational_health.node_resources_unavailable")}</h4><p>{s.reason}</p></div>)}
    {o.termination_history ? <TerminationHistoryView key={o.termination_history.requested_end} history={o.termination_history} report={report} /> : report.snapshot.sources?.filter((s) => s.source === "docdb-terminations" && s.state === "unavailable").map((s) => <div className="operational-card" key={s.source}><h4>{t("ui.operational_health.terminations_unavailable")}</h4><p>{s.reason}</p></div>)}
    {o.spaces.length > 0 && <div className="operational-grid">{o.spaces.map((s) => {
      const growth = s.growth_enabled === null ? "unmeasured" : s.growth_enabled ? "configured" : "unavailable";
      return <div className="operational-card" key={`${s.scope}:${s.name}`}>
        <h4>{s.name}</h4>
        <p>{withElements(t("ui.operational_health.space_used", { used: slot(0), total: bytes(s.total_bytes) }), [<b key="used">{bytes(s.used_bytes)}</b>])}</p>
        <p className="muted">{scopeLabel(s.scope)} · {s.limit_bytes !== null ? t("ui.operational_health.growth_limit", { growth, limit: bytes(s.limit_bytes) }) : t("ui.operational_health.growth", { growth })} {t("ui.operational_health.free_space_unmeasured")}</p>
        {s.parts.length > 0 && <dl>{s.parts.map((p) => <div key={p.name}><dt>{p.name}</dt><dd>{bytes(p.bytes)}</dd></div>)}</dl>}
        {s.reuse_wait && <p>{withElements(t("ui.operational_health.log_reuse", { reason: slot(0) }), [<code key="reason">{s.reuse_wait}</code>])}</p>}
      </div>;
    })}</div>}
    {o.sqlite && <div className="operational-card">
      <h4>{t("ui.operational_health.sqlite_wal")}</h4><p>{withElements(t("ui.operational_health.sqlite_wal_size", { size: slot(0) }), [<b key="size">{bytes(o.sqlite.wal_bytes)}</b>])}</p>
      <p className="muted">{t("ui.operational_health.sqlite_wal_note")}</p>
    </div>}
    {o.cluster && <div className="operational-card">
      <h4>{t("ui.operational_health.galera")}</h4>
      <dl>{([["ready", t("ui.operational_health.galera_ready"), o.cluster.ready], ["connected", t("ui.operational_health.galera_connected"), o.cluster.connected], ["primary", t("ui.operational_health.galera_primary"), o.cluster.primary_component]] as const).map(([key, name, value]) => <div key={key}><dt>{name}</dt><dd>{value === null ? t("ui.operational_health.not_measured") : value ? t("ui.common.yes") : t("ui.common.no")}</dd></div>)}</dl>
      {o.cluster.local_state && <p>{o.cluster.local_state}</p>}
      {o.cluster.flow_control_paused !== null && <p className="muted">{t("ui.operational_health.flow_control", { share: percent(o.cluster.flow_control_paused) })}</p>}
    </div>}
    {o.counters.length > 0 && <details><summary>{t("ui.operational_health.counters", { count: String(o.counters.length) })}</summary><div className="operational-grid">{o.counters.map((c) => <div className="operational-card" key={`${c.kind}:${c.subject}`}>
      <h4>{COUNTERS[c.kind]()}</h4><p>{withElements(t("ui.operational_health.counter_recorded", { subject: c.subject, count: slot(0) }), [<b key="count">{count(c.count)}</b>])}</p>
      <p className="muted">{c.since ? t("ui.operational_health.counter_since", { scope: scopeLabel(c.scope), since: c.since }) : t("ui.operational_health.counter_reset_unknown", { scope: scopeLabel(c.scope) })}</p>
      {c.before && <p>{t("ui.operational_health.counter_between", { count: count(c.count - c.before.count), from: c.before.at, to: report.snapshot.taken_at })}</p>}
      {c.last_event_at && <p className="muted">{t("ui.operational_health.counter_last_event", { at: c.last_event_at })}</p>}
    </div>)}</div></details>}
    {o.sequences.length > 0 && <details><summary>{t("ui.operational_health.sequences", { count: String(o.sequences.length) })}</summary>{o.sequences.length > 100 && <p className="muted">{t("ui.operational_health.sequences_first_100")}</p>}<div className="operational-grid">{sequences.map((s) => <div className="operational-card" key={s.name}>
      <h4>{s.name}</h4><dl><div><dt>{t("ui.operational_health.sequence_allocation")}</dt><dd>{s.current}</dd></div><div><dt>{t("ui.operational_health.sequence_increment")}</dt><dd>{s.increment}</dd></div><div><dt>{t("ui.operational_health.sequence_bounds")}</dt><dd>{t("ui.operational_health.sequence_range", { minimum: s.minimum, maximum: s.maximum })}</dd></div></dl>
      <p className="muted">{t("ui.operational_health.sequence_cycling", { cycling: s.cycles ? "enabled" : "disabled" })}</p>
    </div>)}</div></details>}
    {o.multixacts.length > 0 && <details><summary>{t("ui.operational_health.multixacts", { count: String(o.multixacts.length) })}</summary>{o.multixacts.length > 100 && <p className="muted">{t("ui.operational_health.multixacts_first_100")}</p>}<div className="operational-grid">{multixacts.map((m) => <div className="operational-card" key={m.subject}>
      <h4>{m.subject}</h4><p>{t("ui.operational_health.multixact_age", { age: count(m.age) })}</p><p className="muted">{m.freeze_max_age !== null ? t("ui.operational_health.multixact_freeze", { max: count(m.freeze_max_age) }) : t("ui.operational_health.multixact_no_freeze")}</p>
    </div>)}</div></details>}
    {issues.length > 0 && <details><summary>{t("ui.operational_health.related_findings", { count: String(issues.length) })}</summary><ul>{issues.map((f) => <li key={f.id}>{f.title} <span className="muted">· {ACTIONABILITY[f.actionability]()}</span></li>)}</ul></details>}
  </section>;
}
