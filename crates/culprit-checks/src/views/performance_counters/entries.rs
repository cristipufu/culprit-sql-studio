use super::*;
use culprit_core::{PerformanceCounterAvailability, PerformanceCounterEntry, i18n};

pub(super) fn append(
    profile: &PerformanceCounterProfile,
    sample: &NativeCounterSet,
    entries: &mut Vec<PerformanceCounterEntry>,
) -> Result<()> {
    let observation_key = sample.key()?;
    let at = profile.observed_at(sample)?.map(str::to_owned);
    for row in &sample.counters {
        if entries.len() >= MAX_PERFORMANCE_COUNTER_ENTRIES {
            break;
        }
        let field = profile
            .field_in_sample(sample, &row.native_object, &row.native_name, row.native_instance.as_deref())
            .ok_or_else(invalid)?;
        let raw = match &row.value {
            MeasurementValue::Integer(value) => value,
            _ => return Err(invalid()),
        };
        entries.push(PerformanceCounterEntry {
            observation_key: observation_key.clone(),
            counter_key: row.key.clone(),
            field: field.id.into(),
            family: field.family,
            family_label: family_label(field.family),
            availability: if row.nature == CounterNature::Unknown {
                PerformanceCounterAvailability::UnknownNativeType
            } else {
                PerformanceCounterAvailability::Observed
            },
            native_object: row.native_object.clone(),
            native_name: row.native_name.clone(),
            native_instance: row.native_instance.clone(),
            native_type: row.native_type.clone(),
            observed_at: at.clone(),
            scope: row.scope,
            nature: row.nature,
            unit: row.unit,
            reset: row.reset.clone(),
            raw_value: raw.clone(),
            metric: Metric::said_text(field.label_key, value(raw, row.unit)),
            explanation: if row.nature == CounterNature::Unknown {
                t!("view.performance_counters.limit.native_type")
            } else {
                explanation(field.family)
            },
        });
    }
    for gap in &sample.ratio_gaps {
        if entries.len() >= MAX_PERFORMANCE_COUNTER_ENTRIES {
            break;
        }
        let field =
            profile.field(&gap.native_object, &gap.native_name, gap.native_instance.as_deref()).ok_or_else(invalid)?;
        entries.push(PerformanceCounterEntry {
            observation_key: observation_key.clone(),
            counter_key: gap.key.clone(),
            field: field.id.into(),
            family: field.family,
            family_label: family_label(field.family),
            availability: PerformanceCounterAvailability::RejectedNativeValue,
            native_object: gap.native_object.clone(),
            native_name: gap.native_name.clone(),
            native_instance: gap.native_instance.clone(),
            native_type: Some(gap.native_type.clone()),
            observed_at: at.clone(),
            scope: sample.scope,
            nature: CounterNature::Unknown,
            unit: CounterUnit::Unknown,
            reset: culprit_core::ResetMarker::Unknown,
            raw_value: gap.raw_value.clone(),
            metric: Metric::said_text(field.label_key, t!("view.performance_counters.unknown")),
            explanation: t!("view.performance_counters.limit.rejected"),
        });
    }
    Ok(())
}

fn value(raw: &str, unit: CounterUnit) -> String {
    let value = format::digits(raw.to_owned());
    match unit {
        CounterUnit::Count => value,
        CounterUnit::Kibibytes => t!("view.performance_counters.value.kibibytes", value = value),
        CounterUnit::Bytes => t!("view.performance_counters.value.bytes", value = value),
        CounterUnit::Milliseconds => t!("view.performance_counters.value.milliseconds", value = value),
        CounterUnit::Seconds => t!("view.performance_counters.value.seconds", value = value),
        CounterUnit::Pages { bytes_per_page } => t!(
            "view.performance_counters.value.pages",
            value = value,
            bytes = format::digits(bytes_per_page.to_string())
        ),
        _ => t!("view.performance_counters.value.raw", value = value),
    }
}

pub(super) fn missing(
    profile: &PerformanceCounterProfile,
    samples: &[&NativeCounterSet],
    population: PerformanceCounterPopulation,
) -> Vec<Metric> {
    if samples.is_empty() || samples.iter().any(|sample| !profile.declares_selection(sample)) {
        return Vec::new();
    }
    let mut missing = Vec::new();
    for field in profile.fields {
        if population == PerformanceCounterPopulation::SelectedDatabase
            && !profile.database_field_ids.contains(&field.id)
        {
            continue;
        }
        let observed = |instance: Option<&str>| {
            samples.iter().any(|sample| {
                sample.counters.iter().any(|row| {
                    profile
                        .field_in_sample(sample, &row.native_object, &row.native_name, row.native_instance.as_deref())
                        .is_some_and(|observed| observed.id == field.id)
                        && instance.is_none_or(|instance| row.native_instance.as_deref() == Some(instance))
                }) || sample.ratio_gaps.iter().any(|row| {
                    profile
                        .field_in_sample(sample, &row.native_object, &row.native_name, row.native_instance.as_deref())
                        .is_some_and(|observed| observed.id == field.id)
                        && instance.is_none_or(|instance| row.native_instance.as_deref() == Some(instance))
                })
            })
        };
        if population == PerformanceCounterPopulation::SelectedDatabase {
            if !observed(None) {
                missing.push(Metric::said_text(field.label_key, t!("view.performance_counters.unknown")));
            }
            continue;
        }
        match field.instances {
            PerformanceCounterInstances::Named(instances) => {
                for instance in instances {
                    if !observed(Some(instance)) {
                        missing.push(
                            Metric::text(
                                t!(
                                    "view.performance_counters.field_instance",
                                    field = i18n::t(field.label_key, &[]),
                                    instance = *instance
                                ),
                                t!("view.performance_counters.unknown"),
                            )
                            .keyed(field.label_key),
                        );
                    }
                }
            }
            _ => {
                if !observed(None) {
                    missing.push(Metric::said_text(field.label_key, t!("view.performance_counters.unknown")));
                }
            }
        }
    }
    missing
}

fn family_label(family: PerformanceCounterFamily) -> String {
    match family {
        PerformanceCounterFamily::Compilation => t!("view.performance_counters.family.compilation"),
        PerformanceCounterFamily::Connections => t!("view.performance_counters.family.connections"),
        PerformanceCounterFamily::BufferCache => t!("view.performance_counters.family.buffer_cache"),
        PerformanceCounterFamily::WorkspaceMemory => t!("view.performance_counters.family.workspace_memory"),
        PerformanceCounterFamily::PlanCache => t!("view.performance_counters.family.plan_cache"),
        PerformanceCounterFamily::TemporaryTables => t!("view.performance_counters.family.temporary_tables"),
        PerformanceCounterFamily::TransactionLog => t!("view.performance_counters.family.transaction_log"),
        PerformanceCounterFamily::LocalSsdCache => t!("view.performance_counters.family.local_ssd_cache"),
    }
}
fn explanation(family: PerformanceCounterFamily) -> String {
    match family {
        PerformanceCounterFamily::Compilation => t!("view.performance_counters.explain.compilation"),
        PerformanceCounterFamily::Connections => t!("view.performance_counters.explain.connections"),
        PerformanceCounterFamily::BufferCache => t!("view.performance_counters.explain.buffer_cache"),
        PerformanceCounterFamily::WorkspaceMemory => t!("view.performance_counters.explain.workspace_memory"),
        PerformanceCounterFamily::PlanCache => t!("view.performance_counters.explain.plan_cache"),
        PerformanceCounterFamily::TemporaryTables => t!("view.performance_counters.explain.temporary_tables"),
        PerformanceCounterFamily::TransactionLog => t!("view.performance_counters.explain.transaction_log"),
        PerformanceCounterFamily::LocalSsdCache => t!("view.performance_counters.explain.local_ssd_cache"),
    }
}
