import type { ActivityScopeBasis } from "../gen/ActivityScopeBasis";
import type { MappedPageTarget } from "../gen/MappedPageTarget";
import type { PageWaitExplanation } from "../gen/PageWaitExplanation";
import type { PageWaitObservation } from "../gen/PageWaitObservation";
import type { PageWaitReport } from "../gen/PageWaitReport";
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
  const rows = observationRows(evidence.observations, evidence.source, (row) => pageWaitReferenceKey(row.request.key, row.resource));
  return <ObservationList collapseEntries source={evidence.source} report={report}
    scope={t("ui.page_waits.scope")} entriesLabel={t("ui.page_waits.inspect")}
    time={t("ui.memory_failures.read_between", { from: evidence.observed_from, until: evidence.observed_until })}
    renderEntry={(entry) => {
      const row = rows.get(pageWaitReferenceKey(entry.request_key, entry.resource));
      return row ? <PageWaitCard key={entry.observation_key} row={row} entry={entry} />
        : <div className="operational-card" key={entry.observation_key}><p>{t("ui.memory_failures.missing_row")}</p></div>;
    }} />;
}

function PageWaitCard({ row, entry }: { row: PageWaitObservation; entry: PageWaitExplanation }) {
  const target = row.resolution === "observed-at-both-bounds" && row.request.scope === "request-database" ? row.target : null;
  const linksQualified = row.resolution !== "missing-request" && row.resolution !== "changed-request" && row.resolution !== "unverified-source";
  return <article className="operational-card">
    <h4>{entry.title}</h4>
    <p>{entry.explanation}</p>
    <details>
      <summary>{t("ui.memory_failures.investigation")}</summary>
      <p>{entry.investigation}</p><p>{entry.verify}</p>
      <RequestMetadata row={row} />
      <dl>
        <NativeValue label={t("ui.page_waits.resolution")} value={t(RESOLUTION[row.resolution])} />
        {linksQualified && row.statement_identity && <NativeValue label={t("ui.page_waits.statement_identity")} value={<NativeReference identity={row.statement_identity} />} />}
        {linksQualified && row.statement_identity && row.plan_identity && <NativeValue label={t("ui.page_waits.plan_identity")} value={<NativeReference identity={row.plan_identity} />} />}
      </dl>
      {target && <TargetMetadata target={target} />}
      {row.request.statement.state === "available" && <details>
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
  "missing-request": key("view.page_waits.unresolved.missing_request"),
  "changed-request": key("view.page_waits.unresolved.changed_request"),
  "changed-metadata": key("view.page_waits.unresolved.changed_metadata"),
  "unverified-source": key("view.page_waits.unresolved.unverified_source"),
  "unrecognized-address": key("view.page_waits.unresolved.unrecognized_address"),
  limit: key("view.page_waits.unresolved.limit"),
};

function RequestMetadata({ row }: { row: PageWaitObservation }) {
  const request = row.request;
  const counters = request.request_counters;
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
    <NativeValue label={t("ui.page_waits.native_wait")} value={request.wait?.native_type && <code>{request.wait.native_type}</code>} />
    <NativeValue label={t("ui.page_waits.raw_resource")} value={<NativeReference identity={row.resource.native} />} />
    <NativeValue label={t("ui.page_waits.elapsed")} value={request.elapsed_ms == null ? null : t("common.duration.ms", { n: digits(request.elapsed_ms) })} />
    <NativeValue label={t("ui.page_waits.wait_ms")} value={request.wait?.elapsed_ms == null ? null : t("common.duration.ms", { n: digits(request.wait.elapsed_ms) })} />
    <NativeValue label={t("ui.page_waits.cpu")} value={counters?.cpu_ms == null ? null : t("common.duration.ms", { n: digits(counters.cpu_ms) })} />
    <NativeValue label={t("ui.page_waits.logical_reads")} value={counters?.logical_reads == null ? null : digits(counters.logical_reads)} />
    <NativeValue label={t("ui.page_waits.physical_reads")} value={counters?.physical_reads == null ? null : digits(counters.physical_reads)} />
    <NativeValue label={t("ui.page_waits.writes")} value={counters?.writes == null ? null : digits(counters.writes)} />
  </dl>;
}

const TARGET_KIND: Record<PageWaitTargetKind, string> = {
  "index-leaf": key("view.page_waits.kind.leaf"),
  "index-branch": key("view.page_waits.kind.branch"),
  "heap-data": key("view.page_waits.kind.heap"),
  allocation: key("view.page_waits.kind.allocation"),
  unclassified: key("view.page_waits.kind.unclassified"),
};
const IDENTITY_BASIS: Record<PageTargetIdentityBasis, string> = {
  "database-incarnation": key("ui.page_waits.basis.database"),
  "local-database-file-epoch": key("ui.page_waits.basis.local"),
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
