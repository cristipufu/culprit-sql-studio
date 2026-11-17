import type { CompletedSpillEntry } from "../gen/CompletedSpillEntry";
import type { CompletedSpillReference } from "../gen/CompletedSpillReference";
import type { CompletedSpillReport } from "../gen/CompletedSpillReport";
import { digits, findingMetric } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList, ObservationSourceIssue } from "./ObservationList";

/** The shared explanation owns counter meaning and definition binding; the UI only discloses it. */
export function CompletedSpills({ report }: { report: CompletedSpillReport }) {
  return <div className="completed-spills">
    <ObservationList collapseEntries source={report.sources.map((source) => source.source).join(", ")}
      report={report} scope={t("ui.completed_spills.scope")} entriesLabel={t("ui.completed_spills.inspect")}
      time={<>{t("ui.memory_failures.read_between", { from: report.context.observed_from, until: report.context.observed_until })}<br />
        {t("ui.completed_spills.counts", { native: digits(String(report.native_rows)), observed: digits(String(report.observed_rows)) })}
        {!report.read_complete && <><br />{t("ui.common.read_partial")}</>}
      </>}
      renderEntry={(entry) => <CompletedSpill key={position(entry.observation)} entry={entry} />} />
    {report.definition_read_complete != null && <details>
      <summary>{t("ui.completed_spills.definitions")}</summary>
      {report.definition_context && <p className="muted">{t("ui.memory_failures.read_between", {
        from: report.definition_context.observed_from, until: report.definition_context.observed_until,
      })}</p>}
      {report.definition_read_complete === false && <p className="muted">{t("ui.common.read_partial")}</p>}
      <ObservationSourceIssue sources={report.definition_sources} title={t("ui.completed_spills.definitions")} />
    </details>}
  </div>;
}

function CompletedSpill({ entry }: { entry: CompletedSpillEntry }) {
  return <article className="operational-card">
    <h4>{entry.title}</h4><p>{entry.explanation}</p>
    <dl>{entry.metrics.map((metric) => <NativeValue key={metric.key} label={metric.label} value={findingMetric(metric)} />)}</dl>
    <details>
      <summary>{t("ui.completed_spills.statement")}</summary>
      {entry.statement_preview != null
        ? <pre className="observation-statement"><code>{entry.statement_preview}</code></pre>
        : <p className="muted">{t("ui.completed_spills.statement_unavailable")}</p>}
      {entry.statement_preview_truncated && <p className="muted">{t("ui.completed_spills.statement_truncated")}</p>}
      {entry.definition && <EvidencePosition reference={entry.definition} />}
    </details>
    <details><summary>{t("ui.memory_failures.investigation")}</summary>
      <p>{entry.investigation}</p><p>{entry.verify}</p><EvidencePosition reference={entry.observation} />
    </details>
  </article>;
}

function EvidencePosition({ reference }: { reference: CompletedSpillReference }) {
  return <dl><NativeValue label={reference.kind === "native-read" ? t("ui.completed_spills.read_reference") : t("ui.completed_spills.capture_reference")}
    value={<code>{position(reference)}</code>} /></dl>;
}

function position(reference: CompletedSpillReference): string {
  return reference.kind === "native-read" ? `${reference.read_key}:${reference.observation_key}` : reference.reference.record_id;
}
