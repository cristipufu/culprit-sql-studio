import type { SessionCensusEntry } from "../gen/SessionCensusEntry";
import type { SessionCensusReport } from "../gen/SessionCensusReport";
import { t } from "../lib/i18n";
import { activityPresentationKey } from "../lib/live-census";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Native identity, exact quantities and qualification come from the same shared Rust census. */
export function SessionCensus({ report }: { report: SessionCensusReport }) {
  return (
    <ObservationList
      collapseEntries
      source={report.native_source.source}
      report={report}
      scope={t("view.session_census.scope")}
      time={<>{t("ui.memory_failures.read_between", {
        from: report.context.observed_from,
        until: report.context.observed_until,
      })}<br />{t("ui.memory_participants.observed_at", { at: report.observed_at })}</>}
      renderEntry={renderEntry}
    />
  );
}

function renderEntry(entry: SessionCensusEntry) {
  return (
    <article className="operational-card" key={activityPresentationKey(entry.activity)}>
      <h4>{entry.title}</h4>
      <p>{entry.state}</p>
      <dl>
        {entry.reference && <NativeValue label={t("ui.completed_spills.capture_reference")} value={<code>{entry.reference.record_id}</code>} />}
        {entry.fields.map((field) => (
          <NativeValue key={field.key} label={field.label} value={field.value} />
        ))}
      </dl>
    </article>
  );
}
