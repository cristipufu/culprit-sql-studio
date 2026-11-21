import type { InstanceResourceGroup } from "../gen/InstanceResourceGroup";
import type { InstanceResourceReport } from "../gen/InstanceResourceReport";
import type { NumaNodeEntry } from "../gen/NumaNodeEntry";
import type { NumaNodeReport } from "../gen/NumaNodeReport";
import type { SchedulerEntry } from "../gen/SchedulerEntry";
import type { SchedulerField } from "../gen/SchedulerField";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

const MAX_SCHEDULER_ENTRIES = 256;
// The one native descriptor contains 30 fields, including the separately rendered scheduler ID.
const MAX_SCHEDULER_FIELDS = 29;

/** Shared Rust facts own units, native populations and qualifications; this disclosure calculates no pressure. */
export function InstanceResources({ report }: { report: InstanceResourceReport }) {
  const bounds = report.context?.kind === "native" ? report.context.context : report.context;
  const cpu = report.process_cpu;
  return (
    <>
      {(!report.numa_nodes || report.numa_nodes.populations.length === 0) && <ObservationList
        collapseEntries
        source={report.numa_nodes ? report.title : t("view.instance_resources.source")}
        scope={t("ui.beyond_time.scope.server")}
        time={
          <>
            {report.observed_at != null && (
              <>{t("view.instance_resources.observed_at", { at: report.observed_at })}<br /></>
            )}
            {bounds && (
              <span className="muted">
                {t("ui.memory_failures.read_between", { from: bounds.observed_from, until: bounds.observed_until })}
              </span>
            )}
          </>
        }
        report={{ ...report, entries: report.groups, source_reason: null }}
        renderEntry={renderResourceGroup}
      />}
      {report.schedulers && <SchedulerDetails report={report.schedulers} />}
      {report.numa_nodes && <NumaDetails report={report.numa_nodes} />}
      {cpu && (
        <ObservationList
          collapseEntries
          source={cpu.title}
          scope={t("ui.beyond_time.scope.server")}
          time={cpu.observed_at && t("view.instance_resources.observed_at", { at: cpu.observed_at })}
          report={{
            ...cpu,
            entries: cpu.fields.length ? [{ key: "process-cpu", title: cpu.title, fields: cpu.fields }] : [],
            source_reason: null,
          }}
          renderEntry={renderResourceGroup}
        />
      )}
    </>
  );
}

function renderResourceGroup(group: InstanceResourceGroup) {
  return (
    <article className="operational-card" key={group.key}>
      <h4>{group.title}</h4>
      <dl>
        {group.fields.map((field) => (
          <NativeValue key={field.key} label={field.label} value={field.value} />
        ))}
      </dl>
    </article>
  );
}

function SchedulerDetails({ report }: { report: NonNullable<InstanceResourceReport["schedulers"]> }) {
  const bounds = report.context?.kind === "native" ? report.context.context : report.context;
  return <ObservationList
    collapseEntries
    source={t("view.schedulers.source")}
    scope={t("ui.beyond_time.scope.server")}
    time={bounds && t("ui.memory_failures.read_between", { from: bounds.observed_from, until: bounds.observed_until })}
    report={{ ...report, entries: report.entries.slice(0, MAX_SCHEDULER_ENTRIES) }}
    renderEntry={renderScheduler}
  >
    <p className="muted">{report.population}</p>
    {(report.entries_limited || report.entries.length > MAX_SCHEDULER_ENTRIES) && <p className="muted">{t("view.schedulers.limit.presentation")}</p>}
  </ObservationList>;
}
function renderScheduler(entry: SchedulerEntry) {
  return <article className="operational-card" key={entry.observation_key}>
    <h4>{entry.title}</h4>
    <dl>{entry.fields.slice(0, MAX_SCHEDULER_FIELDS).map((field) => <div key={field.native_name}>
      <dt>{field.label}<br /><code>{field.native_name}</code></dt>
      <dd>{schedulerValue(field)}{field.explanation && <p className="muted">{field.explanation}</p>}</dd>
    </div>)}</dl>
  </article>;
}
function schedulerValue(field: SchedulerField) {
  switch (field.availability) {
    case "observed": return field.value == null ? t("ui.memory_failures.not_reported") : field.unit ? digits(field.value) : field.value;
    case "native-null": return t("view.schedulers.value.null");
    case "column-unavailable": return t("view.schedulers.value.unavailable");
    case "instrumentation-unqualified": return field.unqualified ? t("view.schedulers.value.unqualified", { value: digits(field.unqualified.native_value) }) : t("view.schedulers.value.unavailable");
  }
}

const MAX_NUMA_FIELDS = 13;
/** A read interval is not a per-row timestamp or a physical CPU/memory-node continuity claim. */
function NumaDetails({ report }: { report: NumaNodeReport }) {
  const bounds = report.context?.kind === "native" ? report.context.context : report.context;
  return <>{report.populations.map((population) => <ObservationList key={population.kind} collapseEntries
    source={population.native_source.source} scope={t("ui.beyond_time.scope.server")}
    time={bounds && t("ui.memory_failures.read_between", { from: bounds.observed_from, until: bounds.observed_until })}
    report={{ title: population.title, summary: population.summary, source_state: population.source_state, source_reason: null, limitations: report.limitations, entries: population.entries }}
    renderEntry={renderNumaNode}>
    {population.entries_limited && <p className="muted">{t("view.schedulers.limit.presentation")}</p>}
  </ObservationList>)}</>;
}

function renderNumaNode(entry: NumaNodeEntry) {
  return <article className="operational-card" key={entry.observation_key}>
    <h4>{entry.title}</h4>
    <dl>{entry.fields.slice(0, MAX_NUMA_FIELDS).map((field) => <NativeValue key={field.native_name} label={field.label}
      value={field.availability === "native-null" ? t("view.schedulers.value.null") : field.availability === "column-unavailable" ? t("view.schedulers.value.unavailable") : field.value} />)}</dl>
  </article>;
}
