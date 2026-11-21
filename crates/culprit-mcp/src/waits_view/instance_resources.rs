use culprit_core::{Report, telemetry::INSTANCE_RESOURCES_SOURCE};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 5;
const NUMA_FIELD_LIMIT: usize = 13;
const SCHEDULER_FIELD_LIMIT: usize = culprit_core::telemetry::MSSQL_SCHEDULER_FIELDS.len() - 1;
const NATIVE_TEXT_LIMIT: usize = 512;
const LIMITATION_LIMIT: usize = 8;

pub(super) fn attach(response: &mut Value, report: &Report) {
    if let Some(originals) = &report.numa_nodes {
        response["numa_nodes"] = json!(originals.iter().map(value).collect::<Vec<_>>());
    }
    if let Some(point) = report.instance_resources.as_deref() {
        response["instance_resources"] = value(point);
    } else if !report.snapshot.operations.process_cpu_originals.is_empty()
        || report.snapshot.pressure.as_ref().is_some_and(|pressure| pressure.instance_resources.is_some())
        || report.snapshot.sources.iter().any(|source| {
            matches!(source.source.as_str(), INSTANCE_RESOURCES_SOURCE | culprit_core::source_id::SCHEDULERS)
        })
    {
        response["instance_resources"] = json!({
            "available":false,"reason":culprit_core::t!("view.instance_resources.invalid"),
        });
    }
}

pub(crate) fn value(report: &culprit_core::InstanceResourceReport) -> Value {
    json!({
        "title":report.title,"summary":report.summary,"source_state":report.source_state,
        "read_complete":report.read_complete,"observed_at":report.observed_at,
        "native_populations_complete":report.native_populations_complete,"limitations":report.limitations,
        "schedulers":report.schedulers.as_deref().map(scheduler_value),
        "numa_nodes":report.numa_nodes.as_deref().map(numa_value),
        "process_cpu":report.process_cpu.as_deref().map(cpu_value),
        "process_cpu_originals":report.process_cpu_originals.as_ref().map(|originals| originals.iter().take(ENTRY_LIMIT).map(cpu_value).collect::<Vec<_>>()),
        "process_cpu_originals_available":report.process_cpu_originals.as_ref().map(Vec::len),
        "process_cpu_interval":report.process_cpu_interval.as_deref().map(|interval|json!({
            "available":interval.available,"title":bounded(&interval.title),"summary":bounded(&interval.summary),
            "first_observation":interval.first.as_ref().map(|original|bounded(&original.capture.observation_id)),
            "second_observation":interval.second.as_ref().map(|original|bounded(&original.capture.observation_id)),
            "first_read_bounds":interval.first_context.as_ref().map(|context|json!({"from":context.observed_from,"until":context.observed_until})),
            "second_read_bounds":interval.second_context.as_ref().map(|context|json!({"from":context.observed_from,"until":context.observed_until})),
            "fields":interval.fields.iter().take(CPU_FIELD_LIMIT).map(cpu_field).collect::<Vec<_>>(),
            "limitations":interval.limitations.iter().take(LIMITATION_LIMIT).map(|text|bounded(text)).collect::<Vec<_>>()
        })),
        "groups":report.groups.iter().take(ENTRY_LIMIT).map(|group|json!({
            "key":group.key,"title":group.title,
            "fields":group.fields.iter().take(ENTRY_LIMIT).map(|field|json!({
                "key":field.key,"label":field.label,"value":field.value,
            })).collect::<Vec<_>>(),
            "fields_available":group.fields.len(),"fields_presented":group.fields.len().min(ENTRY_LIMIT),
            "fields_omitted":group.fields.len().saturating_sub(ENTRY_LIMIT),
        })).collect::<Vec<_>>(),
        "groups_available":report.groups.len(),"groups_presented":report.groups.len().min(ENTRY_LIMIT),
        "groups_omitted":report.groups.len().saturating_sub(ENTRY_LIMIT),
    })
}

fn numa_value(nodes: &culprit_core::NumaNodeReport) -> Value {
    json!({
        "scope":nodes.scope,"read_complete":nodes.read_complete,"title":nodes.title,"summary":nodes.summary,"limitations":nodes.limitations,
        "original_read_bounds":nodes.context.as_ref().map(|context|{let(from,until)=context.bounds();json!({"from":from,"until":until})}),
        "populations":nodes.populations.iter().take(2).map(|population|json!({"kind":population.kind,"title":population.title,"summary":population.summary,"native_source":population.native_source,"source_state":population.source_state,"native_rows":population.native_rows,"observed_rows":population.observed_rows,"entries_limited":population.entries_limited,
            "entries_available":population.entries.len(),"entries_presented":population.entries.len().min(ENTRY_LIMIT),
            "entries":population.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({"node_id":bounded(&entry.node_id),"title":bounded(&entry.title),"fields":entry.fields.iter().take(NUMA_FIELD_LIMIT).map(|field|json!({"native_name":bounded(&field.native_name),"label":bounded(&field.label),"value":field.value.as_deref().map(bounded),"availability":field.availability,"unit":field.unit,"nature":field.nature,"reset":field.reset})).collect::<Vec<_>>()})).collect::<Vec<_>>()
        })).collect::<Vec<_>>()
    })
}

fn scheduler_value(report: &culprit_core::SchedulerReport) -> Value {
    json!({
        "scope":report.scope, "title":bounded(&report.title), "summary":bounded(&report.summary), "source_state":report.source_state,
        "source_reason":report.source_reason.as_deref().map(bounded), "native_source":report.native_source,
        "read_complete":report.read_complete, "native_rows":report.native_rows, "observed_rows":report.observed_rows,
        "entries_limited":report.entries_limited, "population":bounded(&report.population),
        "original_read_bounds":report.context.as_ref().map(|context| { let (from,until)=context.bounds(); json!({"from":from,"until":until}) }),
        "limitations":report.limitations.iter().take(LIMITATION_LIMIT).map(|value| bounded(value)).collect::<Vec<_>>(),
        "entries":report.entries.iter().take(ENTRY_LIMIT).map(|entry| json!({
            "observation_key":bounded(&entry.observation_key), "scheduler_id":bounded(&entry.scheduler_id), "title":bounded(&entry.title),
            "fields":entry.fields.iter().take(SCHEDULER_FIELD_LIMIT).map(|field| json!({
                "native_name":bounded(&field.native_name), "label":bounded(&field.label), "value":field.value.as_deref().map(bounded),
                "unqualified":field.unqualified.as_ref().map(|raw|json!({"native_value":bounded(&raw.native_value),"required_global_trace_flag":raw.required_global_trace_flag})), "availability":field.availability, "unit":field.unit, "nature":field.nature, "reset":field.reset,
                "explanation":field.explanation.as_deref().map(bounded),
            })).collect::<Vec<_>>(),
            "fields_available":entry.fields.len(), "fields_presented":entry.fields.len().min(SCHEDULER_FIELD_LIMIT),
            "fields_omitted":entry.fields.len().saturating_sub(SCHEDULER_FIELD_LIMIT),
        })).collect::<Vec<_>>(),
        "entries_available":report.entries.len(), "entries_presented":report.entries.len().min(ENTRY_LIMIT),
        "entries_omitted":report.entries.len().saturating_sub(ENTRY_LIMIT),
    })
}
fn bounded(value: &str) -> String {
    value.chars().take(NATIVE_TEXT_LIMIT).collect()
}

#[cfg(test)]
mod tests;

const CPU_FIELD_LIMIT: usize = 8;
fn cpu_field(field: &culprit_core::InstanceResourceField) -> Value {
    json!({"key":bounded(&field.key),"label":bounded(&field.label),"value":field.value.as_deref().map(bounded)})
}
fn cpu_value(cpu: &culprit_core::ProcessCpuReport) -> Value {
    json!({"title":bounded(&cpu.title),"summary":bounded(&cpu.summary),"source_state":cpu.source_state,"native_source":cpu.native_source,
        "read_complete":cpu.read_complete,"observed_at":cpu.observed_at,"original_observation":cpu.capture.as_ref().map(|capture|bounded(&capture.capture.observation_id)),
        "original_read_bounds":cpu.context.as_ref().map(|context|{let(from,until)=context.bounds();json!({"from":from,"until":until})}),
        "limitations":cpu.limitations.iter().take(LIMITATION_LIMIT).map(|text|bounded(text)).collect::<Vec<_>>(),
        "fields":cpu.fields.iter().take(CPU_FIELD_LIMIT).map(cpu_field).collect::<Vec<_>>(),
        "populations":cpu.populations.iter().take(2).map(|population|json!({"title":bounded(&population.title),"summary":bounded(&population.summary),"native_source":population.native_source,"source_state":population.source_state,"observed_at":population.observed_at,"read_complete":population.read_complete,"native_rows":population.native_rows,"observed_rows":population.observed_rows,
            "limitations":population.limitations.iter().take(LIMITATION_LIMIT).map(|text|bounded(text)).collect::<Vec<_>>(),
            "entries_available":population.entries.len(),"entries_presented":population.entries.len().min(ENTRY_LIMIT),
            "entries":population.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({"key":bounded(&entry.key),"title":bounded(&entry.title),"fields":entry.fields.iter().take(CPU_FIELD_LIMIT).map(cpu_field).collect::<Vec<_>>()})).collect::<Vec<_>>()
        })).collect::<Vec<_>>()})
}
