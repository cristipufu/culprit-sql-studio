import type { IndexedViewIndexEntry } from "../gen/IndexedViewIndexEntry";
import type { IndexedViewsReport } from "../gen/IndexedViewsReport";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { IndexConfigurationDetails } from "./IndexConfigurations";
import { NativeValue, nativeTextPreview } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

const VALUE_CHARS = 160;

function IndexedViewDetails({ entry, qualification }: { entry: IndexedViewIndexEntry; qualification: string }) {
  const usage = entry.usage.kind === "observed" ? entry.usage.data : null;
  const usageState = usage ? (usage.row_present ? "present" : "absent") : entry.usage.kind;
  return <article className="operational-card">
    <h4><code>{nativeTextPreview(entry.parent_schema, VALUE_CHARS)}.{nativeTextPreview(entry.parent_name, VALUE_CHARS)} · {nativeTextPreview(entry.index_name, VALUE_CHARS)}</code></h4>
    <p className="muted">{qualification}</p>
    <p>{t("view.indexed_views.index_state", { unique: String(entry.unique), disabled: String(entry.disabled) })}</p>
    <dl>
      <NativeValue label={t("view.database_properties.native_database_id")} value={digits(String(entry.parent_object.database_id))} />
      <NativeValue label={t("view.table_memory.object_id")} value={digits(String(entry.parent_object.object_id))} />
      <NativeValue label={t("ui.index_configuration.index_id")} value={digits(String(entry.index_id))} />
      <NativeValue label={t("ui.indexed_views.native_type")} value={nativeTextPreview(entry.native_type, VALUE_CHARS)} />
      <NativeValue label={t("ui.index_configuration.created")} value={entry.parent_object.created_without_offset} />
      <NativeValue label={t("ui.index_configuration.modified")} value={entry.parent_modified_without_offset} />
      <NativeValue label={t("ui.indexed_views.reserved_bytes")} value={entry.reserved_bytes == null ? null : digits(entry.reserved_bytes)} />
    </dl>
    <details><summary>{t("ui.indexed_views.usage")}</summary>
      <p>{t("view.indexed_views.usage", { state: usageState })}</p>
      {usage?.row_present && <dl>
        <NativeValue label={t("view.indexed_views.user_seeks")} value={usage.user_seeks == null ? null : digits(usage.user_seeks)} />
        <NativeValue label={t("view.indexed_views.user_scans")} value={usage.user_scans == null ? null : digits(usage.user_scans)} />
        <NativeValue label={t("view.indexed_views.user_lookups")} value={usage.user_lookups == null ? null : digits(usage.user_lookups)} />
        <NativeValue label={t("view.indexed_views.user_updates")} value={usage.user_updates == null ? null : digits(usage.user_updates)} />
        <NativeValue label={t("ui.indexed_views.last_seek")} value={usage.last_user_seek_without_offset} />
        <NativeValue label={t("ui.indexed_views.last_scan")} value={usage.last_user_scan_without_offset} />
        <NativeValue label={t("ui.indexed_views.last_lookup")} value={usage.last_user_lookup_without_offset} />
        <NativeValue label={t("ui.indexed_views.last_update")} value={usage.last_user_update_without_offset} />
      </dl>}
    </details>
    <details><summary>{t("ui.indexed_views.columns", { count: entry.native_columns, shown: entry.columns.length })}</summary>
      <dl>{entry.columns.map((column) => <NativeValue key={column.index_column_id} label={nativeTextPreview(column.name, VALUE_CHARS)} value={t("ui.indexed_views.column", { id: digits(String(column.column_id)), ordinal: digits(String(column.key_ordinal)), included: String(column.included), descending: String(column.descending) })} />)}</dl>
      {entry.columns_limited && <p className="muted">{t("view.indexed_views.presentation_limited", { shown: entry.columns.length, total: entry.native_columns })}</p>}
    </details>
    {entry.configuration && <IndexConfigurationDetails entry={entry.configuration} qualifications={[qualification]} />}
  </article>;
}

/** The backend qualifies current and original sources separately. Current scan labels never join an original view. */
export function IndexedViews({ reports }: { reports: IndexedViewsReport[] }) {
  return <>{reports.map((report, position) => <ObservationList
    key={`${report.context?.observed_from ?? "scan"}:${report.context?.observed_until ?? "scan"}:${position}`}
    source={t("ui.indexed_views.source")}
    scope={report.context ? t("ui.index_configuration.original_scope") : t("ui.index_configuration.scan_scope")}
    report={{ ...report, sources: [report.source] }} collapseEntries
    time={report.context && t("ui.memory_failures.read_between", { from: report.context.observed_from, until: report.context.observed_until })}
    renderEntry={(entry) => <IndexedViewDetails key={`${entry.parent_object.database_id}:${entry.parent_object.object_id}:${entry.parent_object.created_without_offset}:${entry.parent_modified_without_offset}:${entry.index_id}`} entry={entry} qualification={report.qualification} />}
  />)}</>;
}
