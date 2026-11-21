import type { IndexConfigurationEntry } from "../gen/IndexConfigurationEntry";
import type { IndexConfigurationReport } from "../gen/IndexConfigurationReport";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeValue, nativeTextPreview } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

const CONFIGURATION_VALUE_CHARS = 160;

/** These values and qualifications are supplied by the one backend catalog interpreter. */
export function IndexConfigurationDetails({ entry, qualifications }: { entry: IndexConfigurationEntry; qualifications: readonly string[] }) {
  return <details className="td-chunks">
    <summary>{t("ui.index_configuration.title")}</summary>
    {qualifications.map((qualification) => <p className="muted" key={qualification}>{qualification}</p>)}
    <dl>
      {entry.fields.map((field) => <NativeValue key={field.key} label={field.label} value={nativeTextPreview(field.text, CONFIGURATION_VALUE_CHARS)} />)}
      <NativeValue label={t("view.database_properties.native_database_id")} value={digits(String(entry.parent_object.database_id))} />
      <NativeValue label={t("view.table_memory.object_id")} value={digits(String(entry.parent_object.object_id))} />
      <NativeValue label={t("ui.index_configuration.index_id")} value={digits(String(entry.index_id))} />
      <NativeValue label={t("ui.index_configuration.created")} value={entry.parent_object.created_without_offset} />
      <NativeValue label={t("ui.index_configuration.modified")} value={entry.parent_modified_without_offset} />
    </dl>
    <p className="muted">{t("ui.index_configuration.partitions", { retained: entry.retained_partitions, shown: entry.partitions.length })}</p>
    {entry.partitions.map((partition) => <details key={partition.partition_id}>
      <summary>{t("ui.index_configuration.partition", { number: digits(String(partition.partition_number)), id: digits(partition.partition_id) })}</summary>
      <dl>
        <NativeValue label={t("ui.index_configuration.hobt")} value={digits(partition.hobt_id)} />
        {partition.fields.map((field) => <NativeValue key={field.key} label={field.label} value={nativeTextPreview(field.text, CONFIGURATION_VALUE_CHARS)} />)}
      </dl>
    </details>)}
    <details><summary>{t("ui.memory_failures.limitations")}</summary>
      {entry.limitations.map((limitation) => <p className="muted" key={limitation}>{limitation}</p>)}
    </details>
  </details>;
}

/** Original contexts remain separate; current scan labels and clocks never qualify their membership. */
export function IndexConfigurations({ reports }: { reports: IndexConfigurationReport[] }) {
  return <>{reports.map((report, position) => <ObservationList key={`${report.dataset}:${report.context?.observed_from ?? "scan"}:${position}`}
    collapseEntries source={t("ui.index_configuration.source")}
    scope={report.context ? t("ui.index_configuration.original_scope") : t("ui.index_configuration.scan_scope")}
    report={report}
    time={report.context && t("ui.memory_failures.read_between", { from: report.context.observed_from, until: report.context.observed_until })}
    renderEntry={(entry) => <article className="operational-card" key={`${entry.parent_object.database_id}:${entry.parent_object.object_id}:${entry.parent_object.created_without_offset}:${entry.index_id}`}>
      <h4><code>{nativeTextPreview(entry.parent_schema, CONFIGURATION_VALUE_CHARS)}.{nativeTextPreview(entry.parent_name, CONFIGURATION_VALUE_CHARS)} · {entry.index_name ? nativeTextPreview(entry.index_name, CONFIGURATION_VALUE_CHARS) : t("view.index_configuration.heap")}</code></h4>
      <IndexConfigurationDetails entry={entry} qualifications={[report.qualification]} />
    </article>} />)}</>;
}
