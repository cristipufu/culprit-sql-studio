import { useState, type ReactNode } from "react";
import type { MeasurementScope } from "../gen/MeasurementScope";
import type { ReplicationCondition } from "../gen/ReplicationCondition";
import type { ReplicationEntry } from "../gen/ReplicationEntry";
import type { ReplicationField } from "../gen/ReplicationField";
import type { ReplicationReport } from "../gen/ReplicationReport";
import type { SourceState } from "../gen/SourceState";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import "./replication-evidence.css";

const PAGE_SIZE = 10;

function Disclosure({ summary, children, className }: { summary: ReactNode; children: ReactNode; className?: string }) {
  const [open, setOpen] = useState(false);
  return <details className={className} onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>{summary}</summary>
    {open && children}
  </details>;
}

/** Fields arrive already worded and formatted by the shared report. Native date/position strings stay intact. */
export function ReplicationFields({ fields }: { fields: ReplicationField[] }) {
  return <dl className="replication-fields">{fields.map((field, index) => <div key={`${field.key}:${index}`}>
    <dt>{field.label}</dt>
    <dd className={field.value == null || field.value === "" ? "muted" : undefined}>
      {field.value == null ? t("ui.memory_failures.not_reported") : field.value === "" ? t("ui.replication.empty_value") : field.value}
    </dd>
  </div>)}</dl>;
}

function sourceState(state: SourceState): string {
  switch (state) {
    case "collected": return t("ui.replication.source.collected");
    case "empty": return t("ui.replication.source.empty");
    case "partial": return t("ui.replication.source.partial");
    case "unavailable": return t("ui.replication.source.unavailable");
    case "not-applicable": return t("ui.replication.source.not_applicable");
  }
}

function scopeLabel(scope: MeasurementScope): string {
  switch (scope) {
    case "database": return t("ui.replication.scope.database");
    case "pool": return t("ui.replication.scope.pool");
    case "node": return t("ui.replication.scope.node");
    case "cluster": return t("ui.replication.scope.cluster");
    case "server": return t("ui.replication.scope.server");
    case "session": return t("ui.replication.scope.session");
    case "file": return t("ui.replication.scope.file");
    case "unknown": return t("ui.replication.scope.unknown");
  }
}

function conditionLabel(condition: ReplicationCondition): string {
  switch (condition) {
    case "movement-suspended": return t("view.replication.condition.movement_suspended");
    case "not-synchronizing": return t("view.replication.condition.not_synchronizing");
    case "disconnected": return t("view.replication.condition.disconnected");
    case "receiver-stopped": return t("view.replication.condition.receiver_stopped");
    case "applying-stopped": return t("view.replication.condition.applying_stopped");
    case "threads-stopped": return t("view.replication.condition.threads_stopped");
    case "connecting": return t("view.replication.condition.connecting");
    case "slot-invalid": return t("view.replication.condition.slot_invalid");
    case "wal-unreserved": return t("view.replication.condition.wal_unreserved");
    case "replay-paused": return t("view.replication.condition.replay_paused");
  }
}

function ReadContext({ report }: { report: ReplicationReport }) {
  const context = report.context;
  const bounds = context.kind === "native" ? context.context : context;
  return <div className="replication-read">
    <p className="muted">{context.kind === "native" ? t("ui.replication.clock.native") : t("ui.replication.clock.collector")}</p>
    <p className="replication-clock"><time dateTime={bounds.observed_from}>{bounds.observed_from}</time><span aria-hidden> – </span><time dateTime={bounds.observed_until}>{bounds.observed_until}</time></p>
    <dl className="replication-counts">
      <div><dt>{t("ui.replication.native_rows")}</dt><dd>{digits(String(report.native_rows))}</dd></div>
      <div><dt>{t("ui.replication.observed_rows")}</dt><dd>{digits(String(report.observed_rows))}</dd></div>
      <div><dt>{t("ui.replication.state_rows")}</dt><dd>{digits(String(report.state_rows))}</dd></div>
      <div><dt>{t("ui.replication.presented_rows")}</dt><dd>{digits(String(report.presented_rows))}</dd></div>
    </dl>
    {!report.read_complete && <p>{t("ui.common.read_partial")}</p>}
    {report.entries_limited && <p className="replication-limit">{t("ui.replication.presentation_limit")}</p>}
    <ul className="replication-sources">{report.sources.map((source) => <li key={source.source}>
      <span className="replication-source"><code>{source.source}</code><span className="replication-badge">{sourceState(source.state)}</span></span>
      {source.reason != null && <p>{source.reason}</p>}
    </li>)}</ul>
    {report.limitations.length > 0 && <Disclosure className="replication-qualifications" summary={t("ui.replication.qualifications")}>
      <ul>{report.limitations.map((limitation, index) => <li key={index}>{limitation}</li>)}</ul>
    </Disclosure>}
  </div>;
}

function Reference({ entry }: { entry: ReplicationEntry }) {
  return <Disclosure className="replication-reference" summary={t("ui.replication.reference")}>
    <dl className="replication-fields">
      <div><dt>{t("ui.replication.source")}</dt><dd><code>{entry.reference.source}</code></dd></div>
      <div><dt>{t("ui.replication.observation")}</dt><dd><code>{entry.reference.observation_key}</code></dd></div>
      {entry.session && <>
        <div><dt>{t("ui.replication.native_session")}</dt><dd><code>{entry.session.id}</code></dd></div>
        <div><dt>{t("ui.replication.session_generation")}</dt><dd><code>{entry.session.generation}</code></dd></div>
      </>}
      {entry.unresolved_session_id != null && <div><dt>{t("ui.replication.unresolved_session")}</dt><dd><code>{entry.unresolved_session_id}</code></dd></div>}
    </dl>
  </Disclosure>;
}

export function ReplicationEntryEvidence({ entry }: { entry: ReplicationEntry }) {
  return <Disclosure className="replication-entry" summary={<>
    <span className="replication-entry-title">{entry.title}</span>
    <span className="replication-badge">{scopeLabel(entry.scope)}</span>
    {entry.conditions.map((condition) => <span className="replication-badge" key={condition}>{conditionLabel(condition)}</span>)}
  </>}>
    <div className="replication-entry-body">
      <p>{entry.explanation}</p>
      <ReplicationFields fields={entry.fields} />
      {entry.groups.map((group, index) => <Disclosure className="replication-group" key={`${group.title}:${index}`} summary={group.title}>
        <ReplicationFields fields={group.fields} />
      </Disclosure>)}
      <Disclosure className="replication-investigation" summary={t("ui.replication.investigate")}>
        <p>{entry.investigation}</p><p>{entry.verify}</p>
      </Disclosure>
      <Reference entry={entry} />
    </div>
  </Disclosure>;
}

function Body({ report }: { report: ReplicationReport }) {
  const [page, setPage] = useState(0);
  const lastPage = Math.max(0, Math.ceil(report.entries.length / PAGE_SIZE) - 1);
  const currentPage = Math.min(page, lastPage);
  const first = currentPage * PAGE_SIZE;
  return <div className="replication-body">
    <ReadContext report={report} />
    <div className="replication-entries">{report.entries.slice(first, first + PAGE_SIZE).map((entry) => <ReplicationEntryEvidence entry={entry} key={`${entry.reference.source}:${entry.reference.observation_key}`} />)}</div>
    {report.entries.length > PAGE_SIZE && <div className="actions replication-pages">
      <button type="button" className="btn" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>{t("ui.termination_history.previous")}</button>
      <span className="muted">{t("ui.memory_failures.page", { first: digits(String(first + 1)), last: digits(String(Math.min(first + PAGE_SIZE, report.entries.length))), total: digits(String(report.entries.length)) })}</span>
      <button type="button" className="btn" disabled={currentPage === lastPage} onClick={() => setPage(currentPage + 1)}>{t("ui.termination_history.next")}</button>
    </div>}
  </div>;
}

/** Native detail remains useful with no legacy replica rows. Closed disclosures mount no native field tables. */
export function ReplicationEvidence({ report }: { report: ReplicationReport }) {
  return <section className="replication-evidence" aria-label={report.title}>
    <Disclosure className="replication-disclosure" summary={<>
      <strong>{report.title}</strong>
      <span className="replication-summary">{report.summary}</span>
      {!report.read_complete && <span className="replication-badge">{t("ui.replication.partial_read")}</span>}
      {report.entries_limited && <span className="replication-badge">{t("ui.replication.limited")}</span>}
    </>}>
      <Body report={report} />
    </Disclosure>
  </section>;
}
