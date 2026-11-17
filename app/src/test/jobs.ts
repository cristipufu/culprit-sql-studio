import type { JobEntry } from "../gen/JobEntry";
import type { JobsReport } from "../gen/JobsReport";
import type { OperationalObservation } from "../gen/OperationalObservation";
import type { TimescaleJobsSnapshot } from "../gen/TimescaleJobsSnapshot";
import { partial } from "./fixtures";

export const JOBS_SOURCE = "timescale-jobs";
export function jobPoint(index = 0): OperationalObservation {
  return partial<OperationalObservation>({ source: JOBS_SOURCE, key: `job-state-${index}`, fact: { kind: "timescale-job-state", data: {
    position: index + 1, observed_at: "2026-11-15T12:00:00.123456Z", job_id: String(1000 + index),
    procedure: { schema: "public", name: "native_procedure<script>job-canary</script>" },
    owner: "native_job_owner", scheduled: true, statistics: { reported_job_state: { kind: "running" }, total_runs: "9007199254740993" },
  } } });
}
export function jobsEvidence(rows = [jobPoint()]): TimescaleJobsSnapshot {
  return partial<TimescaleJobsSnapshot>({ context: { observed_from: "2026-11-15T12:00:00.123456Z", observed_until: "2026-11-15T12:00:01.123456Z",
    target: { scope: "database", identity: { engine: "postgres", database: "private-read-database-canary", location: { kind: "server", host: "private-read-host-canary", port: 5432, instance: null }, incarnation: null } }, partition: { kind: "target" }, database_writable: true },
    metadata: { run_window: { from: "2026-11-14T12:00:00.123456Z", until: "2026-11-15T12:00:00.123456Z" } },
    sources: [{ source: JOBS_SOURCE, state: "partial", records: rows.length, complete_scope: false, error_code: null, stopped: null }], observations: rows });
}
export function jobsReport(rows = [jobPoint()]): JobsReport {
  return { context: jobsEvidence(rows).context, sources: [{ source: JOBS_SOURCE, state: "partial", reason: "Job-owner visibility remains partial." }], read_complete: true,
    native_rows: rows.length, observed_rows: rows.length, entries_limited: false, title: "Recorded TimescaleDB jobs", summary: "Native job state and run outcomes remain independent.",
    limitations: ["Reported Running does not prove a worker or request.", "Job history retention and logging remain incomplete."],
    entries: rows.map((row): JobEntry => {
      if (row.fact.kind !== "timescale-job-state") throw new Error("Job state fixture required");
      return { source: row.source, observation_key: row.key, job_id: row.fact.data.job_id, kind: "state", title: `Native job ${row.fact.data.job_id}`,
        explanation: "The native view reports Running without an exact worker binding.", metrics: [
          { label: "Native lifetime run counter", key: "view.jobs.metric.total_runs", value: 0, unit: "text", text: "9007199254740993" },
          { label: "Current reported procedure", key: "view.jobs.metric.procedure", value: 0, unit: "text", text: row.fact.data.procedure.name },
        ], investigation: "Inspect the original job point.", verify: "Read the supported native view without executing the job." };
    }) };
}
