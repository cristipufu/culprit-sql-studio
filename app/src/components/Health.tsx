import { evidencePresentation } from "../lib/engines";
import { useState, type MouseEvent } from "react";
import type { Engine } from "../gen/Engine";
import type { OpenTransaction } from "../gen/OpenTransaction";
import type { Pressure } from "../gen/Pressure";
import type { Report } from "../gen/Report";
import type { SignalStatus } from "../gen/SignalStatus";
import type { ResourceSample } from "../gen/ResourceSample";
import type { Signal } from "../gen/Signal";
import { dateFormat, duration, percent, percentValue } from "../lib/format";
import { findingStatus, transactionState, worstFinding } from "../lib/health";
import { t } from "../lib/i18n";
import { tx } from "../lib/message-parts";
import { SIGNAL_SCOPE, isHistoricalSignalScope, signalValue } from "../lib/waits";
import { Replication, StorageLatency } from "./BeyondTime";
import { OperationalHealth, hasOperationalHealth } from "./OperationalHealth";
import { MemoryFailureSourceIssue, RetainedMemoryFailures, memoryFailureReadIssue } from "./RetainedMemoryFailures";
import { NativeFailureSourceIssue, RetainedNativeFailures, nativeFailureReadIssue } from "./RetainedNativeFailures";
import { RequestMemoryParticipants, SessionMemoryParticipants } from "./MemoryParticipants";
import { ObservationExplanationIssue, ObservationSourceIssue, observationReadIssue } from "./ObservationList";
import { PageWaitTargets } from "./PageWaitTargets";
import { TaskSpace } from "./TaskSpace";
import { QueryCompilation } from "./QueryCompilation";
import { CompletedSpills } from "./CompletedSpills";
import { CompilerAdmission } from "./CompilerAdmission";
import { CleanupEvidence } from "./CleanupEvidence";
import { StatementWork } from "./StatementWork";
import { PerformanceCounters } from "./PerformanceCounters";
import { CounterRatios } from "./CounterRatios";
import { MemoryClerks } from "./MemoryClerks";
import { TempdbMetadataMemory } from "./TempdbMetadataMemory";
import { HyperscaleLogComponents } from "./HyperscaleLogComponents";
import { DatabaseProperties } from "./DatabaseProperties";
import { StorageEvidence } from "./StorageEvidence";
import { InstanceResources } from "./InstanceResources";
import { ConfiguredLogRates } from "./ConfiguredLogRates";
import { WaitCounters } from "./WaitCounters";
import { ConnectionOutcomes } from "./ConnectionOutcomes";
import { PageServerReads, pageServerReadIssue } from "./PageServerReads";
import { Jobs, JOBS_SOURCES, AGENT_JOB_SOURCES, MYSQL_EVENTS_SOURCE } from "./Jobs";

const RANK: Record<Signal["status"], number> = { bad: 0, watch: 1, good: 2, info: 3 };

/** A signal: label, value, when it was measured, and what healthy looks like, in words. */
export function SignalTile({ signal }: { signal: Signal }) {
  return (
    <div className={`signal-tile ${signal.status}`}>
      <div className="signal-head">
        <span className="signal-label" title={signal.label}>{signal.label}</span>
        {signal.status !== "info" && <i className={`sig-dot ${signal.status}`} title={signal.status === "good" ? t("ui.health.status.good") : signal.status === "watch" ? t("ui.health.status.watch") : t("ui.health.status.bad")} />}
      </div>
      <div className="signal-value">{signalValue(signal)}</div>
      <div className="signal-meta">
        <span className="scope">{SIGNAL_SCOPE[signal.scope]}</span>
        {signal.healthy && <span className="healthy">{signal.healthy}</span>}
      </div>
    </div>
  );
}

function OpenTransactionCard({ txn, status }: { txn: OpenTransaction; status: SignalStatus }) {
  // A replication slot has no start time: how far it holds the horizon back is what tells.
  const slot = txn.holder === "replication-slot";
  const behind = txn.xid_age ?? 0;
  const session = <b>{txn.session}</b>;
  const long = status === "bad" || status === "watch";
  return (
    <div className={`open-txn ${long ? "long" : ""}`}>
      <i className={`sig-dot ${status}`} />
      <div>
        <div className="open-txn-title">
          {slot
            ? tx("ui.health.slot_holds", { cleanup: txn.catalog_only ? "catalog" : "all", count: Math.round(behind) }, { slot: session })
            : txn.holder === "prepared-transaction"
              ? tx("ui.health.oldest_prepared", { duration: duration(txn.seconds * 1000) }, { session })
              : tx("ui.health.oldest_open", { duration: duration(txn.seconds * 1000) }, { session })}
          {transactionState(txn) ? ` · ${transactionState(txn)}` : ""}
        </div>
        <div className="open-txn-meta">
          {txn.program ? `${t("ui.health.from_program", { program: txn.program })} ` : ""}
          {txn.held_bytes ? `${t("ui.health.version_store", { mb: String(Math.round(txn.held_bytes / 1024 / 1024)) })} ` : ""}
          {long ? t("ui.health.see_finding") : t("ui.health.recorded")}
        </div>
      </div>
    </div>
  );
}

export function HealthTab({
  pressure,
  resources,
  report,
  engineLabel,
  engine,
}: {
  pressure: Pressure | null;
  resources: ResourceSample[];
  report: Report;
  engineLabel: string;
  engine?: Engine;
}) {
  const signals = [...(pressure?.signals ?? [])].sort((a, b) => RANK[a.status] - RANK[b.status]);
  const currentSignals = signals.filter((s) => !isHistoricalSignalScope(s.scope));
  const historicalSignals = signals.filter((s) => isHistoricalSignalScope(s.scope));
  const memoryFailures = report.snapshot.operations.memory_failures;
  const memoryReport = report.memory_failures;
  const memoryIssue = memoryFailureReadIssue(memoryFailures?.source, report.snapshot.sources ?? [], report.snapshot.options?.collect_memory_failures === true);
  const nativeFailures = report.snapshot.operations.native_failures;
  const nativeReport = report.native_failures;
  const jobs = report.snapshot.operations.jobs;
  const jobsReport = report.jobs;
  const agentJobs = jobs?.kind === "mssql-agent" || (report.snapshot.sources ?? []).some((outcome) => AGENT_JOB_SOURCES.some((source) => source === outcome.source));
  const mysqlEvents = jobs?.kind === "mysql-events" || (report.snapshot.sources ?? []).some((outcome) => outcome.source === MYSQL_EVENTS_SOURCE);
  const jobsTitle = t(mysqlEvents ? "view.mysql_events.title" : agentJobs ? "view.mssql_agent_jobs.title" : "view.jobs.title");
  const jobsAssessed = Boolean(jobs) || (report.snapshot.sources ?? []).some((outcome) => JOBS_SOURCES.some((source) => source === outcome.source));
  const explicitNativeFailureSource = (report.snapshot.sources ?? []).some((outcome) => outcome.source === (nativeFailures?.source ?? "native failures"));
  const nativeIssue = nativeFailureReadIssue(nativeFailures?.source, report.snapshot.sources ?? [], report.snapshot.options?.collect_native_failures === true && (!jobsAssessed || explicitNativeFailureSource));
  const requests = report.snapshot.pressure?.request_memory;
  const requestReport = report.request_memory;
  const requestIssue = observationReadIssue(requests?.source ?? "current request memory", report.snapshot.sources ?? [], report.snapshot.options?.collect_request_memory === true);
  const sessions = report.snapshot.operations.session_memory;
  const sessionReport = report.session_memory;
  const sessionIssue = observationReadIssue(sessions?.source ?? "session memory", report.snapshot.sources ?? [], report.snapshot.options?.collect_session_memory === true);
  const pageWaits = report.snapshot.operations.page_waits;
  const pageReport = report.page_waits;
  const pageIssue = observationReadIssue(pageWaits?.source ?? "page wait targets", report.snapshot.sources ?? [], report.snapshot.options?.collect_page_wait_targets === true);
  const tasks = report.snapshot.operations.task_space;
  const taskReport = report.task_space;
  const taskIssue = observationReadIssue(tasks?.source ?? "task space", report.snapshot.sources ?? [], report.snapshot.options?.collect_task_space === true);
  const compilation = report.snapshot.operations.query_compilation;
  const compilationReport = report.query_compilation;
  const compilationIssue = observationReadIssue(compilation?.source ?? "query compilation", report.snapshot.sources ?? []);
  const completedSpillRead = report.snapshot.operations.cached_runtime;
  const completedSpillReport = report.completed_spills;
  const completedSpillIssue = observationReadIssue("statement-runtime-cache", report.snapshot.sources ?? []);
  const completedSpillUnexplained = Boolean((completedSpillRead || completedSpillReport) && !(completedSpillRead && completedSpillReport));
  const compilerAdmission = report.snapshot.operations.compiler_admission;
  const compilerAdmissionReport = report.compiler_admission;
  const compilerAdmissionIssue = observationReadIssue(compilerAdmission?.source ?? "compilation gateways", report.snapshot.sources ?? []);
  const jobsSources = (report.snapshot.sources ?? []).filter((outcome) => JOBS_SOURCES.some((source) => source === outcome.source));
  const jobsIssues = jobsSources.map((source) => observationReadIssue(source.source, jobsSources, true)).filter((source) => source !== undefined);
  const statementWork = report.snapshot.operations.statement_work;
  const statementWorkReport = report.statement_work;
  const statementWorkUnexplained = Boolean(statementWork && !statementWorkReport);
  const waitCounters = report.wait_counters;
  const waitCounterRead = pressure?.wait_counters;
  const waitCounterIssue = observationReadIssue("waits", report.snapshot.sources ?? []);
  const performanceCounters = report.performance_counters;
  const hasSharedPerformanceCounters = performanceCounters.some((counters) => counters.population === "shared-server");
  const performanceRecorded = (report.snapshot.sources ?? []).some((source) => source.source === "performance counters");
  const performanceIssue = observationReadIssue("performance counters", report.snapshot.sources ?? [], performanceRecorded);
  const counterRatios = report.counter_ratios;
  const counterRatioRead = pressure?.resource_counters?.some((sample) => sample.ratios.length > 0) ?? false;
  const counterRatioIssue = observationReadIssue("performance counters", report.snapshot.sources ?? []);
  const instanceResources = report.instance_resources;
  const instanceResourceRead = pressure?.instance_resources;
  const instanceResourceRecorded = (report.snapshot.sources ?? []).some((source) => source.source === "instance-resources");
  const instanceResourceIssue = observationReadIssue("instance-resources", report.snapshot.sources ?? [], Boolean(instanceResourceRead));
  const storage = report.storage;
  const storageRead = report.snapshot.operations.storage;
  const storageIssue = observationReadIssue("file_capacity", report.snapshot.sources ?? [], Boolean(storageRead));
  const databaseProperties = report.database_properties;
  const databasePropertiesRead = report.snapshot.operations.database_properties;
  const databasePropertiesRecorded = (report.snapshot.sources ?? []).some((source) => source.source === "database-properties");
  const databasePropertiesIssue = observationReadIssue("database-properties", report.snapshot.sources ?? [], Boolean(databasePropertiesRead || databasePropertiesRecorded));
  const tempdbMetadataMemory = report.tempdb_metadata_memory;
  const tempdbMetadataRead = report.snapshot.operations.tempdb_metadata_memory;
  const tempdbMetadataRecorded = Boolean(tempdbMetadataRead) || (report.snapshot.sources ?? []).some((source) => source.source === "tempdb metadata memory");
  const tempdbMetadataIssue = observationReadIssue("tempdb metadata memory", report.snapshot.sources ?? [], tempdbMetadataRecorded);
  const memoryClerks = report.memory_clerks;
  const memoryClerkRecorded = (report.snapshot.sources ?? []).some((source) => source.source === "memory clerks");
  const memoryClerkIssue = observationReadIssue("memory clerks", report.snapshot.sources ?? [], memoryClerkRecorded);
  const hyperscaleLogComponents = report.hyperscale_log_components;
  const hyperscaleLogRead = report.snapshot.operations.hyperscale_log_components;
  const hyperscaleLogRecorded = (report.snapshot.sources ?? []).some((source) => source.source === "hyperscale-log-components");
  const hyperscaleLogIssue = observationReadIssue("hyperscale-log-components", report.snapshot.sources ?? [], hyperscaleLogRecorded);
  const configuredLogRates = report.configured_log_rates;
  const configuredLogRateRead = report.snapshot.pressure?.configured_log_rates;
  const configuredLogRateIssue = observationReadIssue("configured-log-rates", report.snapshot.sources ?? []);
  const connectionOutcomes = report.connection_outcomes;
  const connectionOutcomesRead = report.snapshot.pressure?.connection_outcomes;
  const connectionOutcomesIssue = observationReadIssue("sessions", report.snapshot.sources ?? []);
  const pageServerReads = report.page_server_reads;
  const pageServerCarrier = report.snapshot.workload?.queries.some((query) => query.plans?.some((plan) => Boolean(plan.page_server_reads))) ?? false;
  const pageServerIssue = pageServerReadIssue(report.snapshot.sources ?? []);
  const cleanup = report.cleanup;
  const cleanupReads = report.snapshot.operations.cleanup ?? [];
  const retention = pressure?.retention ?? null;
  const cleanupSources = new Set(["persistent version store", "open transactions", ...cleanupReads.map((read) => read.source), ...(retention?.sources.map((source) => source.source) ?? [])]);
  const cleanupIssues = [...cleanupSources].map((source) => observationReadIssue(source, report.snapshot.sources ?? [])).filter((source) => source !== undefined);
  const cleanupUnexplained = !cleanup && (cleanupReads.length > 0 || retention !== null);
  const hasDetailedEvidence = [memoryFailures && memoryReport, nativeFailures && nativeReport, requests && requestReport,
    sessions && sessionReport, pageWaits && pageReport, tasks && taskReport, compilation && compilationReport, completedSpillRead, completedSpillReport, compilerAdmission && compilerAdmissionReport, statementWork, waitCounters, waitCounterRead, performanceCounters.length > 0, performanceRecorded, counterRatios, counterRatioRead, instanceResources, instanceResourceRead, instanceResourceRecorded, storage, storageRead, databaseProperties, databasePropertiesRead, databasePropertiesRecorded, memoryClerks, memoryClerkRecorded, tempdbMetadataMemory, tempdbMetadataRead, tempdbMetadataRecorded, hyperscaleLogComponents, hyperscaleLogRead, hyperscaleLogRecorded, configuredLogRates, configuredLogRateRead, connectionOutcomes, connectionOutcomesRead, pageServerReads, pageServerCarrier, jobs, jobsSources.length > 0, cleanup, cleanupUnexplained, report.replication].some(Boolean);
  const hasReadIssue = [memoryIssue, nativeIssue, requestIssue, sessionIssue, pageIssue, taskIssue, compilationIssue, completedSpillIssue, compilerAdmissionIssue, waitCounterIssue, performanceIssue, counterRatioIssue, instanceResourceIssue, storageIssue, databasePropertiesIssue, memoryClerkIssue, tempdbMetadataIssue, hyperscaleLogIssue, configuredLogRateIssue, connectionOutcomesIssue, pageServerIssue].some(Boolean) || cleanupIssues.length > 0;
  if (!signals.length && !resources.length && !hasOperationalHealth(report) && !hasDetailedEvidence && !hasReadIssue) {
    return (
      <div className="waits-empty">
        <h3>{t("ui.health.empty")}</h3>
        <p>
          {t(evidencePresentation(engine ?? report.snapshot.server.engine).health_empty)}
        </p>
      </div>
    );
  }
  const uptime = pressure?.uptime_hours;
  const showUptime = uptime != null && signals.some((s) => s.scope === "since-startup") && !signals.some((s) => s.scope === "cumulative");
  return (
    <div className="health-tab">
      {signals.length > 0 && (
        <>
          <div className="health-intro">
            {t("ui.health.intro")} {showUptime ? `${t("ui.health.since_start", { duration: duration(uptime * 3_600_000), engine: engineLabel })} ` : ""}
            {t("ui.health.coverage")}
          </div>
          <div className="signal-grid">
            {currentSignals.map((s) => <SignalTile key={s.key} signal={s} />)}
          </div>
          {historicalSignals.length > 0 && (
            <details className="historical-signals">
              <summary>{t("ui.health.historical")}</summary>
              <div className="signal-grid">
                {historicalSignals.map((s) => <SignalTile key={s.key} signal={s} />)}
              </div>
            </details>
          )}
        </>
      )}
      {report.snapshot.server.platform === "azure-sql-database" && !signals.some((s) => s.key === "cpu-use") && (
        <p className="muted">{t("ui.health.cpu_unmeasured")}</p>
      )}
      {pressure?.oldest_transaction && <OpenTransactionCard txn={pressure.oldest_transaction} status={findingStatus(worstFinding(report, (f) => f.check === "held-cleanup"))} />}
      {engine && <StorageLatency files={pressure?.files ?? []} engine={engine} report={report} />}
      <Replication replicas={pressure?.replicas ?? []} report={report} />
      <OperationalHealth report={report} />
      {memoryFailures && memoryReport && <RetainedMemoryFailures key={memoryFailures.observed_until} evidence={memoryFailures} report={memoryReport} />}
      {!(memoryFailures && memoryReport) && memoryIssue && <MemoryFailureSourceIssue source={memoryIssue} />}
      {nativeFailures && nativeReport && <RetainedNativeFailures key={nativeFailures.observed_until} evidence={nativeFailures} report={nativeReport} />}
      {!(nativeFailures && nativeReport) && nativeIssue && <NativeFailureSourceIssue source={nativeIssue} />}
      {requests && requestReport && <RequestMemoryParticipants key={requests.observed_at} evidence={requests} report={requestReport} />}
      {!(requests && requestReport) && requestIssue && <ObservationSourceIssue source={requestIssue} title={t("view.request_memory.title")} />}
      {sessions && sessionReport && <SessionMemoryParticipants key={sessions.observed_until} evidence={sessions} report={sessionReport} />}
      {!(sessions && sessionReport) && sessionIssue && <ObservationSourceIssue source={sessionIssue} title={t("view.session_memory.title")} />}
      {pageWaits && pageReport && <PageWaitTargets key={pageWaits.observed_until} evidence={pageWaits} report={pageReport} />}
      {!(pageWaits && pageReport) && pageIssue && <ObservationSourceIssue source={pageIssue} title={t("view.page_waits.title")} />}
      {tasks && taskReport && <TaskSpace key={tasks.observed_until} evidence={tasks} report={taskReport} />}
      {!(tasks && taskReport) && taskIssue && <ObservationSourceIssue source={taskIssue} title={t("view.task_space.title")} />}
      {compilation && compilationReport && <QueryCompilation key={compilation.observed_until} evidence={compilation} report={compilationReport} />}
      {!(compilation && compilationReport) && compilationIssue && <ObservationSourceIssue source={compilationIssue} title={t("view.compilation.title")} />}
      {completedSpillRead && completedSpillReport && <CompletedSpills key={report.snapshot.id} report={completedSpillReport} />}
      {!(completedSpillRead && completedSpillReport) && completedSpillIssue && <ObservationSourceIssue source={completedSpillIssue} title={t("view.completed_spills.title")} />}
      {completedSpillUnexplained && !completedSpillIssue && <ObservationExplanationIssue title={t("view.completed_spills.title")} reason={t("view.completed_spills.invalid")} />}
      {compilerAdmission && compilerAdmissionReport && <CompilerAdmission key={report.snapshot.id} evidence={compilerAdmission} report={compilerAdmissionReport} />}
      {!(compilerAdmission && compilerAdmissionReport) && compilerAdmissionIssue && <ObservationSourceIssue source={compilerAdmissionIssue} title={t("view.compiler_admission.title")} />}
      {jobs && jobsReport && <Jobs key={report.snapshot.id} evidence={jobs} report={jobsReport} />}
      {!(jobs && jobsReport) && jobsIssues.length > 0 && <ObservationSourceIssue sources={jobsIssues} title={jobsTitle} />}
      {!(jobs && jobsReport) && (jobs || jobsSources.length > 0) && jobsIssues.length === 0 && <ObservationExplanationIssue title={jobsTitle} reason={t("view.jobs.invalid")} />}
      {statementWork && statementWorkReport && <StatementWork key={report.snapshot.id} evidence={statementWork} report={statementWorkReport} />}
      {statementWorkUnexplained && <ObservationExplanationIssue title={t("view.statement_work.title")} reason={t("view.statement_work.invalid")} />}
      {hyperscaleLogComponents && <HyperscaleLogComponents key={report.snapshot.id} report={hyperscaleLogComponents} />}
      {!hyperscaleLogComponents && hyperscaleLogIssue && <ObservationSourceIssue source={hyperscaleLogIssue} title={t("view.hyperscale_log_components.title")} />}
      {!hyperscaleLogComponents && (hyperscaleLogRead || hyperscaleLogRecorded) && !hyperscaleLogIssue && <ObservationExplanationIssue title={t("view.hyperscale_log_components.title")} reason={t("view.hyperscale_log_components.invalid")} />}
      {configuredLogRates && <ConfiguredLogRates key={report.snapshot.id} report={configuredLogRates} />}
      {!configuredLogRates && configuredLogRateIssue && <ObservationSourceIssue source={configuredLogRateIssue} title={t("view.configured_log_rates.title")} />}
      {!configuredLogRates && configuredLogRateRead && !configuredLogRateIssue && <ObservationExplanationIssue title={t("view.configured_log_rates.title")} reason={t("view.configured_log_rates.invalid")} />}
      {waitCounters && <WaitCounters key={report.snapshot.id} report={waitCounters} />}
      {!waitCounters && waitCounterIssue && <ObservationSourceIssue source={waitCounterIssue} title={t("view.wait_counters.title")} />}
      {!waitCounters && waitCounterRead && !waitCounterIssue && <ObservationExplanationIssue title={t("view.wait_counters.title")} reason={t("view.wait_counters.invalid")} />}
      {performanceCounters.map((counters) => <PerformanceCounters key={`${report.snapshot.id}:${counters.population}`} report={counters} />)}
      {!hasSharedPerformanceCounters && performanceIssue && <ObservationSourceIssue source={performanceIssue} title={t("view.performance_counters.title")} />}
      {!hasSharedPerformanceCounters && performanceRecorded && !performanceIssue && <ObservationExplanationIssue title={t("view.performance_counters.title")} reason={t("view.performance_counters.invalid")} />}
      {!performanceCounters.some((counters) => counters.ratios) && counterRatios && <CounterRatios key={report.snapshot.id} report={counterRatios} />}
      {!hasSharedPerformanceCounters && !counterRatios && counterRatioIssue && <ObservationSourceIssue source={counterRatioIssue} title={t("view.counter_ratios.title")} />}
      {!hasSharedPerformanceCounters && !counterRatios && counterRatioRead && !counterRatioIssue && <ObservationExplanationIssue title={t("view.counter_ratios.title")} reason={t("view.counter_ratios.invalid")} />}
      {instanceResources && <InstanceResources key={report.snapshot.id} report={instanceResources} />}
      {!instanceResources && instanceResourceIssue && <ObservationSourceIssue source={instanceResourceIssue} title={t("view.instance_resources.title")} />}
      {!instanceResources && (instanceResourceRead || instanceResourceRecorded) && !instanceResourceIssue && <ObservationExplanationIssue title={t("view.instance_resources.title")} reason={t("view.instance_resources.invalid")} />}
      {storage && <StorageEvidence key={report.snapshot.id} report={storage} />}
      {!storage && storageIssue && <ObservationSourceIssue source={storageIssue} title={t("view.storage.title")} />}
      {!storage && storageRead && !storageIssue && <ObservationExplanationIssue title={t("view.storage.title")} reason={t("view.storage.invalid")} />}
      {databaseProperties && <DatabaseProperties key={report.snapshot.id} report={databaseProperties} />}
      {!databaseProperties && databasePropertiesIssue && <ObservationSourceIssue source={databasePropertiesIssue} title={t("view.database_properties.title")} />}
      {!databaseProperties && (databasePropertiesRead || databasePropertiesRecorded) && !databasePropertiesIssue && <ObservationExplanationIssue title={t("view.database_properties.title")} reason={t("view.database_properties.invalid")} />}
      {tempdbMetadataMemory && <TempdbMetadataMemory key={report.snapshot.id} report={tempdbMetadataMemory} />}
      {!tempdbMetadataMemory && tempdbMetadataIssue && <ObservationSourceIssue source={tempdbMetadataIssue} title={t("view.tempdb_metadata_memory.title")} />}
      {!tempdbMetadataMemory && tempdbMetadataRecorded && !tempdbMetadataIssue && <ObservationExplanationIssue title={t("view.tempdb_metadata_memory.title")} reason={t("view.tempdb_metadata_memory.invalid")} />}
      {memoryClerks && <MemoryClerks key={report.snapshot.id} report={memoryClerks} />}
      {!memoryClerks && memoryClerkIssue && <ObservationSourceIssue source={memoryClerkIssue} title={t("collected.memory_clerks.title")} />}
      {!memoryClerks && memoryClerkRecorded && !memoryClerkIssue && <ObservationExplanationIssue title={t("collected.memory_clerks.title")} reason={t("collected.memory_clerks.invalid")} />}
      {connectionOutcomes && <ConnectionOutcomes key={report.snapshot.id} report={connectionOutcomes} />}
      {pageServerReads && <PageServerReads key={report.snapshot.id} report={pageServerReads} />}
      {!pageServerReads && pageServerIssue && <ObservationSourceIssue source={pageServerIssue} title={t("view.page_server_reads.title")} />}
      {!pageServerReads && !pageServerIssue && pageServerCarrier && <ObservationExplanationIssue title={t("view.page_server_reads.title")} reason={t("view.page_server_reads.invalid")} />}
      {!connectionOutcomes && connectionOutcomesIssue && <ObservationSourceIssue source={connectionOutcomesIssue} title={t("view.connection_outcomes.title")} />}
      {connectionOutcomesRead && !connectionOutcomes && !connectionOutcomesIssue && <ObservationExplanationIssue title={t("view.connection_outcomes.title")} reason={t("view.connection_outcomes.invalid")} />}
      {cleanup && <CleanupEvidence key={report.snapshot.id} reads={cleanupReads} retention={retention} report={cleanup} />}
      {!cleanup && cleanupIssues.length > 0 && <ObservationSourceIssue sources={cleanupIssues} title={t("view.cleanup.title")} />}
      {cleanupUnexplained && cleanupIssues.length === 0 && <ObservationExplanationIssue title={t("view.cleanup.title")} reason={t("view.cleanup.invalid")} />}
      {resources.length >= 4 && <ResourceCharts samples={resources} signals={signals} />}
    </div>
  );
}

// ── resource limits (Azure SQL Database) ───────────────────────────────────────────────────────────

const VW = 600;
const VH = 120;
const LIMIT = 95;

type Pick = (s: ResourceSample) => number | null;

/** The tier's limits charted, each with the signal that grades it. */
function dimensions(samples: ResourceSample[]): { key: string; signalKey: string; label: string; qualification?: string; pick: Pick }[] {
  const ioLabels: Record<ResourceSample["data_io_accounting"], { label: string; qualification: string }> = {
    "data-files": { label: t("core.resource_io.data_files.label"), qualification: t("core.resource_io.data_files.meaning") },
    "compute-local-ssd": { label: t("core.resource_io.compute_local_ssd.label"), qualification: t("core.resource_io.compute_local_ssd.meaning") },
    unknown: { label: t("core.resource_io.unknown.label"), qualification: t("core.resource_io.unknown.meaning") },
  };
  // Group only the already typed source facts. The browser neither identifies a deployment nor pools different
  // accounting bases. Unknown remains its own series, including pool samples without native pool qualification.
  const bases = [...new Set(samples.filter((sample) => sample.data_io_pct !== null).map((sample) => sample.data_io_accounting))];
  const io = bases.map((basis) => ({ key: `data-io:${basis}`, signalKey: bases.length === 1 ? "data-io" : "", ...ioLabels[basis], pick: (sample: ResourceSample) => sample.data_io_accounting === basis ? sample.data_io_pct : null }));
  return [
    { key: "log-write-use", signalKey: "log-write-use", label: t("ui.health.dimension.log_writes"), pick: (s) => s.log_write_pct },
    { key: "cpu-use", signalKey: "cpu-use", label: t("ui.health.dimension.cpu"), pick: (s) => s.cpu_pct },
    ...io,
    { key: "workers-use", signalKey: "workers-use", label: t("ui.health.dimension.workers"), pick: (s) => s.workers_pct },
  ];
}

const timeFmt = () => dateFormat({ timeZone: "UTC", hour: "2-digit", minute: "2-digit", hour12: false });

export function ResourceCharts({ samples, signals }: { samples: ResourceSample[]; signals: Signal[] }) {
  const times = samples.map((s) => Date.parse(s.at));
  return (
    <section className="resource-charts">
      <div className="resource-head">
        <h4>{t("ui.health.limits")}</h4>
        <span className="muted">
          {t("ui.health.limits_about", { from: timeFmt().format(times[0]!), to: timeFmt().format(times[times.length - 1]!) })}
        </span>
      </div>
      <div className="resource-grid">
        {dimensions(samples).map((d) => (
          <ResourceChart key={d.key} label={d.label} qualification={d.qualification} status={signals.find((s) => s.key === d.signalKey)?.status ?? "info"} times={times} values={samples.map(d.pick)} />
        ))}
      </div>
    </section>
  );
}

function ResourceChart({ label, qualification, status, times, values }: { label: string; qualification?: string; status: SignalStatus; times: number[]; values: (number | null)[] }) {
  const [hover, setHover] = useState<number | null>(null);
  const known = values.filter((v): v is number => v != null);
  if (known.length < 4) return null;
  const from = times[0]!;
  const to = times[times.length - 1]!;
  const x = (time: number) => ((time - from) / Math.max(1, to - from)) * VW;
  const y = (v: number) => VH - (Math.min(100, Math.max(0, v)) / 100) * VH;
  const points = values.map((v, i) => (v == null ? null : [x(times[i]!), y(v)] as const)).filter((p): p is readonly [number, number] => p != null);
  const line = points.map(([px, py], i) => `${i ? "L" : "M"}${px.toFixed(1)},${py.toFixed(1)}`).join("");
  const area = `${line}L${points[points.length - 1]![0]},${VH}L${points[0]![0]},${VH}Z`;
  const atLimit = known.filter((v) => v >= LIMIT).length / known.length;
  const peakIndex = values.reduce<number>((best, v, i) => (v != null && v > (values[best] ?? -1) ? i : best), 0);
  const peak = values[peakIndex] ?? 0;
  const ticks = [0, 0.25, 0.5, 0.75, 1].map((f) => from + f * (to - from));
  const onMove = (e: MouseEvent<HTMLDivElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    const at = from + ((e.clientX - box.left) / box.width) * (to - from);
    let best = 0;
    times.forEach((time, i) => {
      if (Math.abs(time - at) < Math.abs(times[best]! - at)) best = i;
    });
    setHover(best);
  };
  const hv = hover != null ? values[hover] : null;
  return (
    <div className="resource-chart">
      <div className="resource-chart-head">
        <span className="resource-label">
          <i className={`sig-dot ${status}`} /> {label}
        </span>
        <span className="resource-sentence">
          {atLimit >= 0.01
            ? tx("ui.health.at_limit", {}, { share: <b>{percent(atLimit)}</b> })
            : t("ui.health.recorded_peak", { peak: percentValue(peak) })}
        </span>
      </div>
      {qualification && <p className="muted">{qualification}</p>}
      <div className="resource-plot" onMouseMove={onMove} onMouseLeave={() => setHover(null)}>
        <div className="resource-yaxis" aria-hidden>
          <span style={{ top: "0%" }}>100%</span>
          <span style={{ top: "50%" }}>50%</span>
          <span style={{ top: "100%" }}>0</span>
        </div>
        <div className="resource-canvas">
          <svg viewBox={`0 0 ${VW} ${VH}`} preserveAspectRatio="none" role="img" aria-label={t("ui.health.chart_label", { dimension: label, share: percent(atLimit), peak: percentValue(peak) })}>
            <line className="grid" x1="0" x2={VW} y1={y(50)} y2={y(50)} />
            <line className="grid" x1="0" x2={VW} y1={y(100)} y2={y(100)} />
            <line className="limit" x1="0" x2={VW} y1={y(LIMIT)} y2={y(LIMIT)} />
            <path className="area" d={area} />
            <path className="line" d={line} vectorEffect="non-scaling-stroke" />
            {hover != null && <line className="cursor" x1={x(times[hover]!)} x2={x(times[hover]!)} y1="0" y2={VH} vectorEffect="non-scaling-stroke" />}
          </svg>
          <span className="peak" style={{ left: `${(x(times[peakIndex]!) / VW) * 100}%`, top: `${(y(peak) / VH) * 100}%` }} title={t("ui.health.peak", { peak: percentValue(peak), time: timeFmt().format(times[peakIndex]!) })} />
          <span className="limit-label">95%</span>
          {hover != null && hv != null && (
            <span className="readout" style={{ left: `${(x(times[hover]!) / VW) * 100}%` }}>
              <b>{percentValue(hv)}</b> {timeFmt().format(times[hover]!)}
            </span>
          )}
        </div>
      </div>
      <div className="resource-xaxis" aria-hidden>
        {ticks.map((tick, i) => (
          <span key={i} style={{ left: `${i * 25}%` }}>
            {timeFmt().format(tick)}
          </span>
        ))}
      </div>
    </div>
  );
}
