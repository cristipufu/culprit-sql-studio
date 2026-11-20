import type { TempdbMetadataMemoryReport } from "../gen/TempdbMetadataMemoryReport";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** The Rust report already qualified the original native owner, clocks and usage semantics. */
export function TempdbMetadataMemory({ report }: { report: TempdbMetadataMemoryReport }) {
  const bounds = report.context.kind === "native" ? report.context.context : report.context;
  return <ObservationList collapseEntries source={t("provider.mssql.source.tempdb_metadata_memory")}
    report={{ ...report, source_reason: null }} scope={t("ui.tempdb_metadata_memory.scope")}
    time={t("ui.memory_failures.read_between", { from: bounds.observed_from, until: bounds.observed_until })}
    renderEntry={(entry) => <article className="operational-card" key={entry.observation_key}>
      <h4><code>{entry.native_type_name}</code></h4>
      <dl>
        <NativeValue label={t("view.tempdb_metadata_memory.field.consumer")} value={digits(entry.native_consumer_id)} />
        <NativeValue label={t("ui.tempdb_metadata_memory.consumer_description")} value={entry.description} />
        <NativeValue label={t("view.storage.field.observed_at")} value={entry.observed_at} />
        <NativeValue label={t("view.tempdb_metadata_memory.field.reserved")} value={entry.allocated_bytes === null ? null : digits(entry.allocated_bytes)} />
        <NativeValue label={t("view.tempdb_metadata_memory.field.used")} value={entry.used_bytes === null ? null : digits(entry.used_bytes)} />
      </dl>
      <p>{entry.explanation}</p><p>{entry.investigation}</p><p>{entry.verify}</p>
    </article>}>
    <dl>
      <NativeValue label={t("view.tempdb_metadata_memory.field.enabled")} value={report.metadata_enabled === null ? null : digits(report.metadata_enabled ? "1" : "0")} />
      <NativeValue label={t("view.tempdb_metadata_memory.field.feature_observed_at")} value={report.feature_observed_at} />
      <NativeValue label={t("view.tempdb_metadata_memory.field.created")} value={report.tempdb_created_without_offset} />
    </dl>
  </ObservationList>;
}
