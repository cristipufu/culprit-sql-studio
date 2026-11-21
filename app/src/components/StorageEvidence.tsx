import type { StorageEntry } from "../gen/StorageEntry";
import type { StorageFileSummary } from "../gen/StorageFileSummary";
import type { StorageReport } from "../gen/StorageReport";
import { t } from "../lib/i18n";
import { NativeValue, nativeTextPreview } from "./NativeObservationFields";
import { ObservationList, ObservationReadQualifications } from "./ObservationList";

/** Exact values, native populations and file-size meanings come from the shared Rust interpretation. */
export function StorageEvidence({ report }: { report: StorageReport }) {
  const bounds = report.context.kind === "native" ? report.context.context : report.context;
  return (
    <ObservationList
      collapseEntries
      source={t("view.storage.title")}
      report={report}
      scope={t("view.storage.scope")}
      time={t("ui.memory_failures.read_between", {
        from: bounds.observed_from,
        until: bounds.observed_until,
      })}
      renderEntry={renderStorageEntry}
    >
      {report.file_summary && <FileCensus key={JSON.stringify(report.file_summary)} summary={report.file_summary} />}
    </ObservationList>
  );
}

function renderStorageEntry(entry: StorageEntry) {
  return (
    <article className="operational-card" key={`${entry.source}:${entry.observation_key}`}>
      <h4>{entry.title}</h4>
      <p>{entry.explanation}</p>
      <p className="muted">
        {t(entry.scope === "server" ? "ui.beyond_time.scope.server" : "ui.beyond_time.scope.this_database")}
      </p>
      <dl>
        {entry.fields.map((field) => (
          <NativeValue key={field.key} label={field.label} value={field.value} />
        ))}
      </dl>
    </article>
  );
}

/** Counts and independent byte quantities come from the complete original census, before row presentation limits. */
function FileCensus({ summary }: { summary: StorageFileSummary }) {
  return (
    <details className="storage-file-census">
      <summary>{summary.title}</summary>
      <p>{summary.summary}</p>
      <ObservationReadQualifications sources={[{ source: summary.source, state: summary.source_state }]} />
      <dl>
        {summary.fields.map((field) => (
          <NativeValue
            key={field.key}
            label={field.label}
            value={field.value == null ? null : nativeTextPreview(field.value, 512)}
          />
        ))}
      </dl>
      {summary.limitations.map((limitation, index) => <p className="muted" key={index}>{limitation}</p>)}
    </details>
  );
}
