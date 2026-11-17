import type { JobsReport } from "../gen/JobsReport";
import type { TimescaleJobsSnapshot } from "../gen/TimescaleJobsSnapshot";
import { findingMetric } from "../lib/format";
import { t } from "../lib/i18n";
import { observationRowsBySource } from "../lib/observation-rows";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

export const JOBS_SOURCES = ["timescale-jobs", "timescale-job-runs"] as const;

/** Rust words the native conditions; references locate only the exact original row, never a current worker. */
export function Jobs({ evidence, report }: { evidence: TimescaleJobsSnapshot; report: JobsReport }) {
  const sources = observationRowsBySource(evidence.observations, (row) => row.key);
  const sameRead = JSON.stringify(evidence.context) === JSON.stringify(report.context);
  return <ObservationList collapseEntries source={JOBS_SOURCES[0]} report={report}
    scope={t("ui.jobs.scope")} entriesLabel={t("ui.jobs.inspect")}
    time={<>{t("ui.memory_failures.read_between", { from: evidence.context.observed_from, until: evidence.context.observed_until })} {t("ui.jobs.window", { from: evidence.metadata.run_window.from, until: evidence.metadata.run_window.until })}</>}
    renderEntry={(entry) => {
      const row = sources.get(entry.source)?.get(entry.observation_key);
      const matched = sameRead && row && ((entry.kind === "state" && row.fact.kind === "timescale-job-state")
        || (entry.kind === "retained-run" && row.fact.kind === "timescale-job-run"))
        && "job_id" in row.fact.data && row.fact.data.job_id === entry.job_id;
      return matched ? <article className="operational-card" key={`${entry.source}:${entry.observation_key}`}>
        <h4>{entry.title}</h4><p>{entry.explanation}</p>
        <dl>{entry.metrics.map((metric, index) => <NativeValue key={metric.key || index} label={metric.label} value={findingMetric(metric)} />)}</dl>
        <details><summary>{t("ui.memory_failures.investigation")}</summary><p>{entry.investigation}</p><p>{entry.verify}</p></details>
      </article> : <div className="operational-card" key={`${entry.source}:${entry.observation_key}`}><p>{t("ui.memory_failures.missing_row")}</p></div>;
    }} />;
}
