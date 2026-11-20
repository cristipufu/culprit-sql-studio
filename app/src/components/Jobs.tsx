import type { JobsReport } from "../gen/JobsReport";
import type { JobsSnapshot } from "../gen/JobsSnapshot";
import { findingMetric } from "../lib/format";
import { t } from "../lib/i18n";
import { observationRowsBySource } from "../lib/observation-rows";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

export const MYSQL_EVENTS_SOURCE = "mysql-scheduled-events";
export const AGENT_JOB_SOURCES = ["mssql-agent-jobs", "mssql-agent-job-steps", "mssql-agent-job-history", "mssql-agent-context"] as const;
export const JOBS_SOURCES = ["timescale-jobs", "timescale-job-runs", ...AGENT_JOB_SOURCES, "timescale-continuous-aggregates", MYSQL_EVENTS_SOURCE] as const;

/** Rust words the native conditions; references locate only the exact original row, never a current worker. */
export function Jobs({ evidence, report }: { evidence: JobsSnapshot; report: JobsReport }) {
  const native = evidence.snapshot;
  const sources = observationRowsBySource(native.observations, (row) => row.key);
  const sameRead = JSON.stringify(native.context) === JSON.stringify(report.context);
  return <ObservationList collapseEntries source={evidence.kind === "mysql-events" ? MYSQL_EVENTS_SOURCE : evidence.kind === "mssql-agent" ? AGENT_JOB_SOURCES[0] : JOBS_SOURCES[0]} report={report}
    scope={evidence.kind === "mysql-events" ? t("view.mysql_events.ui.scope") : evidence.kind === "mssql-agent" ? t("view.mssql_agent_jobs.ui.scope") : t("ui.jobs.scope")} entriesLabel={t("ui.jobs.inspect")}
    time={<>{t("ui.memory_failures.read_between", { from: native.context.observed_from, until: native.context.observed_until })} {evidence.kind === "timescale" && t("ui.jobs.window", { from: evidence.snapshot.metadata.run_window.from, until: evidence.snapshot.metadata.run_window.until })}</>}
    renderEntry={(entry) => {
      const row = sources.get(entry.source)?.get(entry.observation_key);
      const matched = sameRead && row && ((entry.kind === "state" && ["timescale-job-state", "mssql-agent-job", "mssql-agent-step", "mysql-scheduled-event"].includes(row.fact.kind))
        || (entry.kind === "retained-run" && ["timescale-job-run", "mssql-agent-history"].includes(row.fact.kind))
        || (entry.kind === "agent-context" && row.fact.kind === "mssql-agent-context")
        || (entry.kind === "continuous-aggregate" && row.fact.kind === "timescale-continuous-aggregate"))
        && ("job_id" in row.fact.data ? row.fact.data.job_id === entry.job_id : entry.job_id === null);
      return matched ? <article className="operational-card" key={`${entry.source}:${entry.observation_key}`}>
        <h4>{entry.title}</h4><p>{entry.explanation}</p>
        <dl>{entry.metrics.map((metric, index) => <NativeValue key={`${metric.key}:${index}`} label={metric.label} value={findingMetric(metric)} />)}</dl>
        <details><summary>{t("ui.memory_failures.investigation")}</summary><p>{entry.investigation}</p><p>{entry.verify}</p></details>
      </article> : <div className="operational-card" key={`${entry.source}:${entry.observation_key}`}><p>{t("ui.memory_failures.missing_row")}</p></div>;
    }} />;
}
