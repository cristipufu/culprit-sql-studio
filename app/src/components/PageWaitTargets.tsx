import { useState } from "react";
import type { ActivityScopeBasis } from "../gen/ActivityScopeBasis";
import type { DefinitionPayload } from "../gen/DefinitionPayload";
import type { DiagnosticEvidenceRef } from "../gen/DiagnosticEvidenceRef";
import type { MappedPageTarget } from "../gen/MappedPageTarget";
import type { PageWaitExplanation } from "../gen/PageWaitExplanation";
import type { PageWaitObservation } from "../gen/PageWaitObservation";
import type { PageWaitReport } from "../gen/PageWaitReport";
import type { PageWaitRecordedRequest } from "../gen/PageWaitRecordedRequest";
import type { PageWaitRecheck } from "../gen/PageWaitRecheck";
import type { PageWaitRelation } from "../gen/PageWaitRelation";
import type { PageWaitRequestState } from "../gen/PageWaitRequestState";
import type { PageWaitResolution } from "../gen/PageWaitResolution";
import type { PageWaitSnapshot } from "../gen/PageWaitSnapshot";
import type { PageWaitTargetKind } from "../gen/PageWaitTargetKind";
import type { PageTargetIdentityBasis } from "../gen/PageTargetIdentityBasis";
import type { WaitResourceKind } from "../gen/WaitResourceKind";
import { digits } from "../lib/format";
import { key, t } from "../lib/i18n";
import { observationRows } from "../lib/observation-rows";
import { pageWaitReferenceKey } from "../lib/page-wait-reference";
import { NativeReference, NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Native metadata describes the wait target and affected request, never an unobserved latch owner. */
export function PageWaitTargets({ evidence, report }: { evidence: PageWaitSnapshot; report: PageWaitReport }) {
  const rows = observationRows(evidence.observations, evidence.source, (row) => pageWaitReferenceKey(row.request.key, row.resource, row.task_wait));
  return <ObservationList collapseEntries source={evidence.source} report={report}
    scope={t("ui.page_waits.scope")} entriesLabel={t("ui.page_waits.inspect")}
    time={t("ui.memory_failures.read_between", { from: evidence.observed_from, until: evidence.observed_until })}
    renderEntry={(entry) => {
      const row = rows.get(pageWaitReferenceKey(entry.request_key, entry.resource, entry.task));
      return row ? <PageWaitCard key={entry.observation_key} row={row} entry={entry} />
        : <div className="operational-card" key={entry.observation_key}><p>{t("ui.memory_failures.missing_row")}</p></div>;
    }}>
    {report.recorded_incident && <RecordedIncident
      key={recordedContextKey(report.recorded_incident.original)}
      incident={report.recorded_incident} />}
  </ObservationList>;
}

const RECORDED_SQL_PREVIEW_CHARACTERS = 512;

function RecordedIncident({ incident }: { incident: NonNullable<PageWaitReport["recorded_incident"]> }) {
  return <article className="operational-card recorded-page-wait">
    <h4>{t("view.page_waits.recorded.title")}</h4>
    <p className="muted">{t("view.page_waits.recorded.context", {
      at: incident.observed_at, session: incident.request.session.id,
      request: incident.request.request?.id ?? t("ui.memory_failures.not_reported"), resource: incident.resource.native.id,
    })}</p>
    <p className="muted">{t("view.page_waits.recorded.qualification")}</p>
    <RecordedStatement sql={incident.sql} />
    <RecordedApplication application={incident.client?.application} />
  </article>;
}

function RecordedRequest({ request }: { request: PageWaitRecordedRequest }) {
  return <details className="recorded-page-request">
    <summary>{t("view.page_waits.recorded_request.title")}</summary>
    <p className="muted">{t("view.page_waits.recorded_request.context", {
      at: request.observed_at, session: request.request.session.id,
      request: request.request.request?.id ?? t("ui.memory_failures.not_reported"),
    })}</p>
    <p className="muted">{t("view.page_waits.recorded_request.qualification")}</p>
    <RecordedStatement sql={request.sql} />
    <RecordedApplication application={request.client?.application} />
  </details>;
}

function RecordedStatement({ sql }: { sql: DefinitionPayload }) {
  const [showStatement, setShowStatement] = useState(false);
  const excerpt = sql.state === "available" ? recordedSqlPreview(sql.text) : null;
  return <>
    {sql.state === "available" && excerpt ? <>
      <pre className="observation-statement">{excerpt.text}</pre>
      {excerpt.truncated && <details onToggle={(event) => setShowStatement(event.currentTarget.open)}>
        <summary>{t("ui.page_waits.recorded.full_statement")}</summary>
        {showStatement && <pre className="observation-statement">{sql.text}</pre>}
      </details>}
    </> : sql.state === "unavailable" && <p className="muted">{t("ui.statement_grants.unavailable", { reason: sql.reason })}</p>}
  </>;
}

function RecordedApplication({ application }: { application: string | null | undefined }) {
  return application && <dl>
    <NativeValue label={t("ui.memory_participants.application")} value={application} />
  </dl>;
}

function recordedContextKey(reference: DiagnosticEvidenceRef): string {
  return JSON.stringify([reference.capture.observation_id, reference.record_id, reference.manifest_sha256, reference.payload_sha256]);
}

function recordedSqlPreview(text: string): { text: string; truncated: boolean } {
  let preview = "";
  let characters = 0;
  for (const character of text) {
    if (characters === RECORDED_SQL_PREVIEW_CHARACTERS) return { text: `${preview}…`, truncated: true };
    preview += character;
    characters++;
  }
  return { text: preview, truncated: false };
}

function PageWaitCard({ row, entry }: { row: PageWaitObservation; entry: PageWaitExplanation }) {
  const target = row.resolution === "observed-at-both-bounds" && row.request.scope === "request-database" ? row.target : null;
  const linksQualified = [row.before_check, row.after_check].every((check) => check?.request === "same-statement"
    && (!row.task_wait || check.wait === "original-observed"));
  return <article className="operational-card">
    <h4>{entry.title}</h4>
    <p>{entry.explanation}</p>
    <details>
      <summary>{t("ui.memory_failures.investigation")}</summary>
      <p>{entry.investigation}</p><p>{entry.verify}</p>
      {entry.recorded_request && <RecordedRequest key={recordedContextKey(entry.recorded_request.original)} request={entry.recorded_request} />}
      <RequestMetadata row={row} />
      {row.task_wait && <TaskWaitMetadata task={row.task_wait} />}
      <RecheckMetadata before={row.before_check} after={row.after_check} />
      <dl>
        <NativeValue label={t("ui.page_waits.resolution")} value={t(RESOLUTION[row.resolution])} />
        {linksQualified && row.statement_identity && <NativeValue label={t("ui.page_waits.statement_identity")} value={<NativeReference identity={row.statement_identity} />} />}
        {linksQualified && row.statement_identity && row.plan_identity && <NativeValue label={t("ui.page_waits.plan_identity")} value={<NativeReference identity={row.plan_identity} />} />}
      </dl>
      {target && <TargetMetadata target={target} />}
      {(!row.task_wait || linksQualified) && row.request.statement.state === "available" && <details>
        <summary>{t("ui.page_waits.original_statement")}</summary>
        <pre className="observation-statement">{row.request.statement.text}</pre>
      </details>}
    </details>
  </article>;
}

const WAIT_KIND: Record<WaitResourceKind, string> = {
  "memory-page": key("ui.page_waits.wait_kind.memory"),
  "page-io": key("ui.page_waits.wait_kind.io"),
  "data-lock": key("ui.page_waits.wait_kind.other"),
};
const SCOPE: Record<ActivityScopeBasis, string> = {
  "request-database": key("ui.page_waits.request_scope.database"),
  "transaction-database": key("ui.page_waits.request_scope.transaction"),
  "session-database": key("ui.page_waits.request_scope.session"),
  "blocking-reference": key("ui.page_waits.request_scope.reference"),
  "other-database": key("ui.page_waits.request_scope.other"),
};
const RESOLUTION: Record<PageWaitResolution, string> = {
  "observed-at-both-bounds": key("ui.page_waits.resolution.observed"),
  "outside-database": key("view.page_waits.unresolved.outside_database"),
  unsupported: key("view.page_waits.unresolved.unsupported"),
  "permission-denied": key("view.page_waits.unresolved.permission_denied"),
  unavailable: key("view.page_waits.unresolved.unavailable"),
  "changed-metadata": key("view.page_waits.unresolved.changed_metadata"),
  "unverified-source": key("view.page_waits.unresolved.unverified_source"),
  "unrecognized-address": key("view.page_waits.unresolved.unrecognized_address"),
  limit: key("view.page_waits.unresolved.limit"),
};

const REQUEST_CHECK: Record<PageWaitRequestState, string> = {
  "same-statement": key("ui.page_waits.recheck.request.same"),
  missing: key("ui.page_waits.recheck.request.missing"),
  changed: key("ui.page_waits.recheck.request.changed"),
  unverified: key("ui.page_waits.recheck.request.unverified"),
};
const WAIT_CHECK: Record<PageWaitRelation, string> = {
  "original-observed": key("ui.page_waits.recheck.wait.original"),
  "no-longer-observed": key("ui.page_waits.recheck.wait.no_longer"),
  different: key("ui.page_waits.recheck.wait.different"),
  unqualified: key("ui.page_waits.recheck.wait.unqualified"),
};

function RecheckMetadata({ before, after }: { before: PageWaitRecheck | undefined; after: PageWaitRecheck | undefined }) {
  return <details>
    <summary>{t("ui.page_waits.rechecks")}</summary>
    <Recheck title={t("ui.page_waits.recheck.before")} check={before} />
    <Recheck title={t("ui.page_waits.recheck.after")} check={after} />
  </details>;
}

function Recheck({ title, check }: { title: string; check: PageWaitRecheck | undefined }) {
  return <div>
    <h5>{title}</h5>
    {check ? <dl>
      <NativeValue label={t("ui.page_waits.recheck.at")} value={<code>{check.observed_at}</code>} />
      <NativeValue label={t("ui.page_waits.recheck.request")} value={t(REQUEST_CHECK[check.request])} />
      <NativeValue label={t("ui.page_waits.recheck.wait")} value={t(WAIT_CHECK[check.wait])} />
    </dl> : <p className="muted">{t("ui.page_waits.recheck.missing")}</p>}
  </div>;
}

function RequestMetadata({ row }: { row: PageWaitObservation }) {
  const request = row.request;
  const counters = request.request_counters;
  const waitType = row.task_wait ? row.task_wait.wait_type : request.wait?.native_type;
  const waitMs = row.task_wait ? row.task_wait.wait_duration_ms : request.wait?.elapsed_ms;
  return <dl>
    <NativeValue label={t("ui.page_waits.original_from")} value={<code>{row.observed_from}</code>} />
    <NativeValue label={t("ui.page_waits.original_until")} value={<code>{row.observed_until}</code>} />
    <NativeValue label={t("ui.memory_participants.session")} value={<code>{request.key.session.id}</code>} />
    <NativeValue label={t("ui.memory_participants.generation")} value={<code>{request.key.session.generation}</code>} />
    <NativeValue label={t("ui.memory_participants.request")} value={request.key.request && <code>{request.key.request.id}</code>} />
    <NativeValue label={t("ui.memory_participants.request_generation")} value={request.key.request && <code>{request.key.request.generation}</code>} />
    <NativeValue label={t("ui.page_waits.request_scope")} value={t(SCOPE[request.scope])} />
    <NativeValue label={t("ui.native_failures.database_context")} value={request.database} />
    <NativeValue label={t("ui.memory_participants.application")} value={request.client?.application} />
    <NativeValue label={t("ui.page_waits.state")} value={request.state} />
    <NativeValue label={t("ui.page_waits.wait_kind")} value={t(WAIT_KIND[row.resource.kind])} />
    <NativeValue label={t("ui.page_waits.native_wait")} value={waitType && <code>{waitType}</code>} />
    {row.task_wait && <NativeValue label={t("ui.page_waits.coordinator_wait")} value={request.wait?.native_type && <code>{request.wait.native_type}</code>} />}
    <NativeValue label={t("ui.page_waits.raw_resource")} value={<NativeReference identity={row.resource.native} />} />
    <NativeValue label={t("ui.page_waits.elapsed")} value={request.elapsed_ms == null ? null : t("common.duration.ms", { n: digits(request.elapsed_ms) })} />
    <NativeValue label={t("ui.page_waits.wait_ms")} value={waitMs == null ? null : t("common.duration.ms", { n: digits(waitMs) })} />
    <NativeValue label={t("ui.page_waits.cpu")} value={counters?.cpu_ms == null ? null : t("common.duration.ms", { n: digits(counters.cpu_ms) })} />
    <NativeValue label={t("ui.page_waits.logical_reads")} value={counters?.logical_reads == null ? null : digits(counters.logical_reads)} />
    <NativeValue label={t("ui.page_waits.physical_reads")} value={counters?.physical_reads == null ? null : digits(counters.physical_reads)} />
    <NativeValue label={t("ui.page_waits.writes")} value={counters?.writes == null ? null : digits(counters.writes)} />
  </dl>;
}

function TaskWaitMetadata({ task }: { task: NonNullable<PageWaitObservation["task_wait"]> }) {
  return <details>
    <summary>{t("ui.page_waits.task.details")}</summary>
    <p>{t("ui.page_waits.task.qualification")}</p>
    <dl>
      <NativeValue label={t("ui.memory_participants.observed_at_label")} value={<code>{task.observed_at}</code>} />
      <NativeValue label={t("ui.task_space.execution_context")} value={digits(String(task.execution_context_id))} />
      <NativeValue label={t("ui.page_waits.task.address")} value={<code>{task.task_address}</code>} />
      <NativeValue label={t("ui.page_waits.task.resource_address")} value={task.resource_address && <code>{task.resource_address}</code>} />
      <NativeValue label={t("ui.page_waits.task.blocking_address")} value={task.blocking_task_address && <code>{task.blocking_task_address}</code>} />
      <NativeValue label={t("ui.page_waits.task.blocking_session")} value={task.blocking_session_id == null ? null : digits(String(task.blocking_session_id))} />
      <NativeValue label={t("ui.page_waits.task.blocking_context")} value={task.blocking_execution_context_id == null ? null : digits(String(task.blocking_execution_context_id))} />
    </dl>
  </details>;
}

const TARGET_KIND: Record<PageWaitTargetKind, string> = {
  "index-leaf": key("view.page_waits.kind.leaf"),
  "index-branch": key("view.page_waits.kind.branch"),
  "heap-data": key("view.page_waits.kind.heap"),
  pfs: key("view.page_waits.kind.pfs"),
  gam: key("view.page_waits.kind.gam"),
  sgam: key("view.page_waits.kind.sgam"),
  iam: key("view.page_waits.kind.iam"),
  unclassified: key("view.page_waits.kind.unclassified"),
};
const IDENTITY_BASIS: Record<PageTargetIdentityBasis, string> = {
  "database-incarnation": key("ui.page_waits.basis.database"),
  "local-database-file-epoch": key("ui.page_waits.basis.local"),
  "local-tempdb-primary-file-epoch": key("ui.page_waits.basis.tempdb_primary"),
};
const HEADER_FIELDS = [
  ["database_id", key("ui.page_waits.header.database_id")],
  ["file_id", key("ui.page_waits.header.file_id")],
  ["page_id", key("ui.page_waits.header.page_id")],
  ["header_version", key("ui.page_waits.header.version")],
  ["page_type", key("ui.page_waits.header.type")],
  ["level", key("ui.page_waits.header.level")],
  ["object_id", key("ui.page_waits.header.object_id")],
  ["index_id", key("ui.page_waits.header.index_id")],
  ["partition_id", key("ui.page_waits.header.partition_id")],
  ["allocation_unit_id", key("ui.page_waits.header.allocation_unit_id")],
  ["next_file_id", key("ui.page_waits.header.next_file_id")],
  ["next_page_id", key("ui.page_waits.header.next_page_id")],
] as const;

function TargetMetadata({ target }: { target: MappedPageTarget }) {
  return <details>
    <summary>{t("ui.page_waits.target_metadata")}</summary>
    <p>{t(TARGET_KIND[target.kind])}</p>
    <dl>
      <NativeValue label={t("ui.page_waits.identity_basis")} value={t(IDENTITY_BASIS[target.identity_basis])} />
      <NativeValue label={t("ui.page_waits.database_identity")} value={<NativeReference identity={target.database} />} />
      <NativeValue label={t("ui.page_waits.file_identity")} value={<NativeReference identity={target.file} />} />
      {HEADER_FIELDS.map(([field, label]) => <NativeValue key={field} label={t(label)} value={target.header[field] == null ? null : <code>{digits(String(target.header[field]))}</code>} />)}
      <NativeValue label={t("ui.page_waits.header.lsn")} value={target.header.lsn && <code>{target.header.lsn}</code>} />
      <NativeValue label={t("ui.page_waits.schema")} value={target.object && <code>{target.object.schema}</code>} />
      <NativeValue label={t("ui.page_waits.object")} value={target.object && <code>{target.object.name}</code>} />
      <NativeValue label={t("ui.page_waits.index")} value={target.object?.index_name && <code>{target.object.index_name}</code>} />
    </dl>
  </details>;
}
