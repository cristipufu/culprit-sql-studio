use culprit_core::{Report, telemetry::INSTANCE_RESOURCES_SOURCE};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 5;

pub(super) fn attach(response: &mut Value, report: &Report) {
    if let Some(point) = report.instance_resources.as_deref() {
        response["instance_resources"] = value(point);
    } else if report.snapshot.pressure.as_ref().is_some_and(|pressure| pressure.instance_resources.is_some())
        || report.snapshot.sources.iter().any(|source| source.source == INSTANCE_RESOURCES_SOURCE)
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
        "process_cpu":report.process_cpu.as_ref().map(|cpu|json!({
            "title":cpu.title,"summary":cpu.summary,"source_state":cpu.source_state,"native_source":cpu.native_source,
            "read_complete":cpu.read_complete,"observed_at":cpu.observed_at,"limitations":cpu.limitations,
            "fields":cpu.fields.iter().take(ENTRY_LIMIT).map(|field|json!({
                "key":field.key,"label":field.label,"value":field.value,
            })).collect::<Vec<_>>(),
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
