import type { JobEntry } from "../gen/JobEntry";
import type { JobsReport } from "../gen/JobsReport";
import type { OperationalObservation } from "../gen/OperationalObservation";
import type { JobsSnapshot } from "../gen/JobsSnapshot";
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
export function jobsEvidence(rows = [jobPoint()]): JobsSnapshot {
  return { kind: "timescale", read_complete: true, snapshot: timescaleEvidence(rows) };
}
function timescaleEvidence(rows: OperationalObservation[]): TimescaleJobsSnapshot {
  return partial<TimescaleJobsSnapshot>({ context: { observed_from: "2026-11-15T12:00:00.123456Z", observed_until: "2026-11-15T12:00:01.123456Z",
    target: { scope: "database", identity: { engine: "postgres", database: "private-read-database-canary", location: { kind: "server", host: "private-read-host-canary", port: 5432, instance: null }, incarnation: null } }, partition: { kind: "target" }, database_writable: true },
    metadata: { run_window: { from: "2026-11-14T12:00:00.123456Z", until: "2026-11-15T12:00:00.123456Z" } },
    sources: [{ source: JOBS_SOURCE, state: "partial", records: rows.length, complete_scope: false, error_code: null, stopped: null }], observations: rows });
}
export function jobsReport(rows = [jobPoint()]): JobsReport {
  return { context: jobsEvidence(rows).snapshot.context, sources: [{ source: JOBS_SOURCE, state: "partial", reason: "Job-owner visibility remains partial." }], read_complete: true,
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

export function agentJobEvidence(): JobsSnapshot {
  const member={engine:"mssql" as const,kind:"instance" as const,namespace:{id:"private-agent-machine",qualifiers:{}},native:{id:"private-agent-instance",qualifiers:{}}};
  return partial<JobsSnapshot>({kind:"mssql-agent",read_complete:true,snapshot:{context:{...timescaleEvidence([]).context,target:{scope:"shared",identity:member},partition:{kind:"instance-local",member,epoch:"original-agent-process"},database_writable:null},
    observations:[{source:"mssql-agent-job-history",key:"original-agent-history",fact:{kind:"mssql-agent-history",data:{observed_at:"2026-11-15T12:00:01.123456Z",job_id:"aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb",step_id:"1",run_status:0,instance_id:"2147483647",run_date:"20261115",run_time:"123456",run_duration:"250101",sql_message_id:"0",sql_severity:"0",retries_attempted:"0",reported_server:null,stored_step_name:"stored <script>agent-canary</script>"}}}],
    sources:[{source:"mssql-agent-job-history",state:"partial",records:1,complete_scope:false,error_code:null,stopped:null}]}});
}
export function agentJobsReport(evidence=agentJobEvidence()): JobsReport {
  return {context:evidence.snapshot.context,sources:[{source:"mssql-agent-job-history",state:"partial",reason:"Retained history is partial."}],read_complete:true,native_rows:1,observed_rows:1,entries_limited:false,title:"Recorded SQL Server Agent jobs",summary:"Retained failure is independent from the current definition.",limitations:["Job local times have no recorded offset.","An old failure does not establish a current failing job."],entries:[{source:"mssql-agent-job-history",observation_key:"original-agent-history",job_id:"aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb",kind:"retained-run",title:"Retained execution record",explanation:"This retained history row records failure, not a current failure.",metrics:[{key:"view.mssql_agent_jobs.field.history_id",label:"Native history row ID",value:0,unit:"text",text:"2147483647"},{key:"view.mssql_agent_jobs.field.duration",label:"Native duration (HHmmss)",value:0,unit:"text",text:"250101"},{key:"view.mssql_agent_jobs.field.stored_step",label:"Stored historical label",value:0,unit:"text",text:"stored <script>agent-canary</script>"}],investigation:"Inspect current definition and retained outcomes separately.",verify:"Take another bounded native read without executing jobs."}]};
}


export function mysqlEventEvidence(): JobsSnapshot {
  const context = jobsEvidence([]).snapshot.context;
  context.target = { scope: "database", identity: { engine: "mysql", database: "shop", location: { kind: "server", host: "private-native-event-host", port: 3306, instance: null }, incarnation: null } };
  context.partition = { kind: "instance-local", member: { engine: "mysql", kind: "instance", namespace: { id: "mysql-server", qualifiers: {} }, native: { id: "original-member", qualifiers: {} } }, epoch: "original-process" };
  const row = partial<OperationalObservation>({ source: "mysql-scheduled-events", key: "original-event", fact: { kind: "mysql-scheduled-event", data: {
    observed_at: context.observed_from, schema: "shop", name: "nightly <script>event-canary</script>", status: "disabled", event_type: "recurring", time_zone: "SYSTEM", last_started_without_offset: null,
  } } });
  return { kind: "mysql-events", read_complete: true, snapshot: { context, metadata: { scheduler_setting: "off", scheduler_observed_at: context.observed_from, database_event_visibility: true, layout: "available", session_time_zone: "+00:00", database_writable: true }, sources: [{ source: "mysql-scheduled-events", records: 1, state: "collected", complete_scope: true, error_code: null, stopped: null }], observations: [row] } };
}
export function mysqlEventsReport(evidence = mysqlEventEvidence()): JobsReport {
  const row = evidence.snapshot.observations[0]!;
  if (row.fact.kind !== "mysql-scheduled-event") throw new Error("original event point required");
  return { context: evidence.snapshot.context, sources: [{ source: row.source, state: "collected", reason: null }], read_complete: true, native_rows: 1, observed_rows: 1, entries_limited: false,
    title: "Scheduled events", summary: "Event Scheduler setting: OFF. This is configuration, not a runtime result.", limitations: ["Last start is not a success or failure outcome."],
    entries: [{ source: row.source, observation_key: row.key, job_id: null, kind: "state", title: "Recorded event", explanation: "The event is recorded as disabled.", metrics: [{ label: "Event", key: "view.mysql_events.field.name", value: 0, unit: "text", text: row.fact.data.name }], investigation: "Review the original event schedule.", verify: "Verify a specific run independently." }] };
}
