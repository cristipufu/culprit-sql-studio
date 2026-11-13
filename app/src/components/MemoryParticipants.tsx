import type { ExecutionGrantObservation } from "../gen/ExecutionGrantObservation";
import type { ExecutionGrantState } from "../gen/ExecutionGrantState";
import type { MemoryParticipantExplanation } from "../gen/MemoryParticipantExplanation";
import type { MemoryParticipantsReport } from "../gen/MemoryParticipantsReport";
import type { NativeIdentity } from "../gen/NativeIdentity";
import type { RequestMemorySnapshot } from "../gen/RequestMemorySnapshot";
import type { SessionMemoryObservation } from "../gen/SessionMemoryObservation";
import type { SessionMemorySnapshot } from "../gen/SessionMemorySnapshot";
import { type ReactNode } from "react";
import { digits } from "../lib/format";
import { key, t } from "../lib/i18n";
import { nativeIdentityKey } from "../lib/native-identity";
import { observationRows } from "../lib/observation-rows";
import { NativeProperties, NativeReference, NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

export function RequestMemoryParticipants({ evidence, report }: { evidence: RequestMemorySnapshot; report: MemoryParticipantsReport }) {
  return <ParticipantList observations={evidence.observations} source={evidence.source} report={report}
    scope={t("ui.memory_participants.request_scope")} time={t("ui.memory_participants.observed_at", { at: evidence.observed_at })}
    renderNative={(row) => <RequestMetadata row={row} />} />;
}

export function SessionMemoryParticipants({ evidence, report }: { evidence: SessionMemorySnapshot; report: MemoryParticipantsReport }) {
  return <ParticipantList observations={evidence.observations} source={evidence.source} report={report}
    scope={t("ui.memory_failures.scope.server")} time={t("ui.memory_participants.observed_between", { from: evidence.observed_from, until: evidence.observed_until })}
    renderNative={(row) => <SessionMetadata row={row} />} />;
}

function ParticipantList<Row extends { identity: NativeIdentity; source: string }>({ observations, source, report, scope, time, renderNative }: {
  observations: Row[];
  source: string;
  report: MemoryParticipantsReport;
  scope: string;
  time: ReactNode;
  renderNative: (row: Row) => ReactNode;
}) {
  const rows = observationRows(observations, source, (row) => nativeIdentityKey(row.identity));
  return <ObservationList collapseEntries source={source} time={time} report={report} scope={scope}
    entriesLabel={t("ui.memory_participants.inspect")} renderEntry={(entry) => {
      const row = rows.get(nativeIdentityKey(entry.observation_identity));
      return row ? <ParticipantCard key={entry.observation_key} entry={entry}>{renderNative(row)}</ParticipantCard>
        : <div className="operational-card" key={entry.observation_key}><p>{t("ui.memory_failures.missing_row")}</p></div>;
    }} />;
}

function ParticipantCard({ entry, children }: { entry: MemoryParticipantExplanation; children: ReactNode }) {
  return <article className="operational-card">
    <h4>{entry.title}</h4>
    <p>{entry.explanation}</p>
    <details>
      <summary>{t("ui.memory_failures.investigation")}</summary>
      <p>{entry.investigation}</p>
      <p>{entry.verify}</p>
      {children}
    </details>
  </article>;
}

const GRANT_FIELDS = [
  ["requested_kibibytes", key("ui.memory_participants.requested")],
  ["required_kibibytes", key("ui.memory_participants.required")],
  ["granted_kibibytes", key("ui.memory_participants.reserved")],
  ["used_kibibytes", key("ui.memory_participants.used")],
  ["maximum_used_kibibytes", key("ui.memory_participants.peak")],
  ["ideal_kibibytes", key("ui.memory_participants.ideal")],
] as const;

const GRANT_COUNTS = [
  ["degree_of_parallelism", key("ui.memory_participants.degree_of_parallelism")],
  ["reserved_workers", key("ui.memory_participants.reserved_workers")],
  ["used_workers", key("ui.memory_participants.used_workers")],
  ["maximum_used_workers", key("ui.memory_participants.maximum_used_workers")],
] as const;

const GRANT_STATE: Record<ExecutionGrantState, () => string> = {
  waiting: () => t("ui.memory_participants.state.waiting"),
  granted: () => t("ui.memory_participants.state.granted"),
};

function RequestMetadata({ row }: { row: ExecutionGrantObservation }) {
  const request = row.request;
  return <>
    <dl>
      <NativeValue label={t("ui.memory_participants.state")} value={GRANT_STATE[row.state]()} />
      <NativeValue label={t("ui.memory_participants.observed_at_label")} value={<code>{row.observed_at}</code>} />
      <NativeValue label={t("ui.memory_participants.session")} value={request && <code>{request.key.session.id}</code>} />
      <NativeValue label={t("ui.memory_participants.generation")} value={request && <code>{request.key.session.generation}</code>} />
      <NativeValue label={t("ui.memory_participants.request")} value={request?.key.request && <code>{request.key.request.id}</code>} />
      <NativeValue label={t("ui.memory_participants.request_generation")} value={request?.key.request && <code>{request.key.request.generation}</code>} />
      <NativeValue label={t("ui.memory_participants.application")} value={request?.client?.application} />
      <NativeValue label={t("ui.native_failures.database_context")} value={request?.database} />
      <NativeValue label={t("ui.memory_participants.requested_at")} value={<code>{row.requested_without_offset}</code>} />
      <NativeValue label={t("ui.memory_participants.granted_at")} value={row.granted_without_offset && <code>{row.granted_without_offset}</code>} />
      {GRANT_FIELDS.map(([field, label]) => <NativeValue key={field} label={t(label)}
        value={row[field] == null ? null : t("common.bytes.kib", { n: digits(row[field]) })} />)}
      <NativeValue label={t("ui.memory_participants.wait")} value={row.wait_ms == null ? null : t("common.duration.ms", { n: digits(row.wait_ms) })} />
      <NativeValue label={t("ui.memory_participants.timeout")} value={row.timeout_seconds == null ? null : t("common.duration.s", { n: digits(row.timeout_seconds) })} />
      {GRANT_COUNTS.map(([field, label]) => <NativeValue key={field} label={t(label)} value={row[field] == null ? null : digits(row[field])} />)}
      <NativeValue label={t("ui.memory_participants.native_resource")} value={row.resource && <NativeReference identity={row.resource} />} />
      <NativeValue label={t("ui.memory_participants.workload_group")} value={row.workload_group && <NativeReference identity={row.workload_group} />} />
    </dl>
    <NativeProperties properties={row.native_properties} />
    {row.request_association === "generation-checked" && row.statement_relationship === "observed-current-statement" && request?.statement.state === "available" && <details>
      <summary>{t("ui.memory_participants.current_statement")}</summary>
      <pre className="observation-statement">{request.statement.text}</pre>
    </details>}
  </>;
}

const SESSION_FIELDS = [
  ["current_bytes", key("ui.memory_participants.tracked_current")],
  ["controlled_current_bytes", key("ui.memory_participants.controlled_current")],
  ["high_water_bytes", key("ui.memory_participants.session_high_water")],
  ["controlled_high_water_bytes", key("ui.memory_participants.controlled_high_water")],
] as const;

function SessionMetadata({ row }: { row: SessionMemoryObservation }) {
  const context = row.current_context;
  return <><dl>
    <NativeValue label={t("ui.memory_participants.session")} value={<code>{row.session.id}</code>} />
    <NativeValue label={t("ui.memory_participants.generation")} value={<code>{row.session.generation}</code>} />
    <NativeValue label={t("ui.memory_participants.observed_at_label")} value={<code>{row.observed_at}</code>} />
    <NativeValue label={t("ui.memory_participants.tracking_enabled")} value={t(row.tracking_enabled ? "ui.common.yes" : "ui.common.no")} />
    {SESSION_FIELDS.map(([field, label]) => <NativeValue key={field} label={t(label)}
      value={row[field] == null ? null : t("common.bytes.b", { n: digits(row[field]) })} />)}
    {context && <>
      <NativeValue label={t("ui.memory_participants.context_at")} value={<code>{context.observed_at}</code>} />
      <NativeValue label={t("ui.memory_participants.current_event")} value={<code>{context.request.id}</code>} />
      <NativeValue label={t("ui.memory_participants.event_generation")} value={<code>{context.request.generation}</code>} />
      <NativeValue label={t("ui.native_failures.database_context")} value={context.database} />
      <NativeValue label={t("ui.memory_participants.query_digest")} value={context.query_hash && <code>{context.query_hash}</code>} />
    </>}
  </dl>
    {!context && <p className="muted">{t("view.session_memory.context.unavailable")}</p>}
    <NativeProperties properties={row.native_properties} />
  </>;
}
