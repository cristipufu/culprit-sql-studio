import { useState } from "react";
import type { MemoryFailureExplanation } from "../gen/MemoryFailureExplanation";
import type { MemoryFailureJsonDocument } from "../gen/MemoryFailureJsonDocument";
import type { MemoryFailureReport } from "../gen/MemoryFailureReport";
import type { MemoryFailureSnapshot } from "../gen/MemoryFailureSnapshot";
import type { MssqlOutOfMemoryObservation } from "../gen/MssqlOutOfMemoryObservation";
import type { SourceOutcome } from "../gen/SourceOutcome";
import { digits } from "../lib/format";
import { key, t } from "../lib/i18n";

const PAGE_SIZE = 10;

/** The backend explains the source's classifications. The UI preserves native values and independently read bounds. */
export function RetainedMemoryFailures({ evidence, report }: { evidence: MemoryFailureSnapshot; report: MemoryFailureReport }) {
  const [page, setPage] = useState(0);
  const lastPage = Math.max(0, Math.ceil(report.entries.length / PAGE_SIZE) - 1);
  const currentPage = Math.min(page, lastPage);
  const first = currentPage * PAGE_SIZE;
  const entries = report.entries.slice(first, first + PAGE_SIZE);
  const observations = new Map(evidence.observations.map((observation) => [observation.position, observation]));
  return <section className="operational-health" aria-label={report.title}>
    <h3>{report.title}</h3>
    <p>{report.summary}</p>
    <p className="muted">{evidence.source} · {memoryScope(evidence.scope)}</p>
    <p className="muted">{t("ui.memory_failures.read_between", { from: evidence.observed_from, until: evidence.observed_until })}</p>
    {readQualification(report.source_state) && <p className="muted">{readQualification(report.source_state)}</p>}
    <details>
      <summary>{t("ui.memory_failures.limitations")}</summary>
      {report.source_reason && <p className="muted">{report.source_reason}</p>}
      {report.limitations.map((limitation, index) => <p className="muted" key={index}>{limitation}</p>)}
    </details>
    <div className="operational-grid memory-failure-grid">
      {entries.map((entry) => {
        const observation = observations.get(entry.position);
        return observation
          ? <MemoryFailureRow key={entry.position} explanation={entry} observation={observation} />
          : <div className="operational-card" key={entry.position}><p>{t("ui.memory_failures.missing_row")}</p></div>;
      })}
    </div>
    {report.entries.length > PAGE_SIZE && <div className="actions">
      <button type="button" className="btn" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>{t("ui.termination_history.previous")}</button>
      <span className="muted">{t("ui.memory_failures.page", { first: digits(String(first + 1)), last: digits(String(Math.min(first + PAGE_SIZE, report.entries.length))), total: digits(String(report.entries.length)) })}</span>
      <button type="button" className="btn" disabled={currentPage === lastPage} onClick={() => setPage(currentPage + 1)}>{t("ui.termination_history.next")}</button>
    </div>}
  </section>;
}

/** A rejected read must remain visible even when it could not admit a native wrapper or a derived explanation. */
export function memoryFailureReadIssue(source: string | undefined, sources: SourceOutcome[]): SourceOutcome | undefined {
  return sources.find((outcome) => outcome.source === (source ?? "memory failures") && (outcome.state === "partial" || outcome.state === "unavailable"));
}

export function MemoryFailureSourceIssue({ source }: { source: SourceOutcome }) {
  return <section className="operational-health">
    <h3>{t("view.memory_failures.title")}</h3>
    <p className="muted">{readQualification(source.state)}</p>
    <details>
      <summary>{t("ui.memory_failures.limitations")}</summary>
      <p>{source.reason ?? t("view.memory_failures.limit.source_unknown")}</p>
    </details>
  </section>;
}

function readQualification(state: SourceOutcome["state"] | null): string | null {
  switch (state) {
    case "partial": return t("ui.common.read_partial");
    case "unavailable": return t("ui.dashboard.status.unavailable");
    default: return null;
  }
}

function memoryScope(scope: MemoryFailureSnapshot["scope"]): string {
  switch (scope) {
    case "pool": return t("ui.memory_failures.scope.pool");
    case "server": return t("ui.memory_failures.scope.server");
    default: return t("ui.memory_failures.scope.unqualified");
  }
}

function MemoryFailureRow({ observation, explanation }: { observation: MssqlOutOfMemoryObservation; explanation: MemoryFailureExplanation }) {
  return <article className="operational-card">
    <h4>{explanation.title}</h4>
    <p>{explanation.explanation}</p>
    <p className="muted">{t("ui.memory_failures.native_time")} <code>{observation.event_time_without_offset}</code></p>
    <details>
      <summary>{t("ui.memory_failures.investigation")}</summary>
      <p>{explanation.investigation}</p>
      <p>{explanation.verify}</p>
      <dl>
        <div><dt>{t("ui.memory_failures.native_cause")}</dt><dd><code>{observation.oom_cause} · {observation.oom_cause_desc}</code></dd></div>
        <div><dt>{t("ui.memory_failures.native_factor")}</dt><dd><code>{observation.oom_factor} · {observation.oom_factor_desc}</code></dd></div>
      </dl>
      <p className="muted">{t("ui.memory_failures.factor_note")}</p>
      <NativeMemoryValues observation={observation} />
      <NativeMemoryDocuments observation={observation} />
    </details>
  </article>;
}

const MEMORY_FIELDS = [
  ["available_physical_memory_mb", key("ui.memory_failures.memory.available_physical")],
  ["process_memory_usage_mb", key("ui.memory_failures.memory.process")],
  ["non_sos_memory_usage_mb", key("ui.memory_failures.memory.non_sos")],
  ["committed_memory_target_mb", key("ui.memory_failures.memory.committed_target")],
  ["committed_memory_mb", key("ui.memory_failures.memory.committed")],
  ["allocation_potential_memory_mb", key("ui.memory_failures.memory.allocation_potential")],
  ["initial_job_object_memory_limit_mb", key("ui.memory_failures.memory.initial_limit")],
  ["current_job_object_memory_limit_mb", key("ui.memory_failures.memory.current_limit")],
  ["possible_non_sos_leaked_memory_mb", key("ui.memory_failures.memory.possible_non_sos_leak")],
] as const;

function NativeMemoryValues({ observation }: { observation: MssqlOutOfMemoryObservation }) {
  return <>
    <p className="muted">{t("ui.memory_failures.memory_note")}</p>
    <dl>{MEMORY_FIELDS.map(([field, label]) => <div key={field}>
      <dt title={field}>{t(label)}</dt>
      <dd>{observation[field] == null ? t("ui.memory_failures.not_reported") : t("common.bytes.mb", { n: digits(String(observation[field])) })}</dd>
    </div>)}</dl>
  </>;
}

const DOCUMENT_FIELDS = [
  ["oom_resource_pools", key("ui.memory_failures.document.exhausted_pools")],
  ["top_memory_clerks", key("ui.memory_failures.document.top_clerks")],
  ["top_resource_pools", key("ui.memory_failures.document.top_pools")],
  ["possible_leaked_memory_clerks", key("ui.memory_failures.document.possible_leaks")],
] as const;

function NativeMemoryDocuments({ observation }: { observation: MssqlOutOfMemoryObservation }) {
  return <details>
    <summary>{t("ui.memory_failures.documents")}</summary>
    <p className="muted">{t("ui.memory_failures.documents_note")}</p>
    {DOCUMENT_FIELDS.map(([field, label]) => <NativeDocument key={field} name={t(label)} document={observation[field]} />)}
  </details>;
}

function NativeDocument({ name, document }: { name: string; document: MemoryFailureJsonDocument | null }) {
  return <details>
    <summary>{name}</summary>
    {document == null ? <p>{t("ui.memory_failures.sql_null")}</p> : <>
      <p className="muted">{t(document.qualification === "parsed" ? key("ui.memory_failures.document.parsed") : key("ui.memory_failures.document.unparsed"))}</p>
      <pre className="mono" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", maxHeight: "18rem", overflow: "auto" }}>{document.raw}</pre>
    </>}
  </details>;
}
