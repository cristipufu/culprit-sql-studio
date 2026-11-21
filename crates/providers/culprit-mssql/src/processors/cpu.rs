//! CPU totals use the topology query's same native row, not another sample or process owner.
use crate::{
    instance,
    read::{integer, invalid_data, optional_counter, optional_text, text},
    value::Rec,
};
use culprit_core::{MeasurementScope, ResetMarker, Result, telemetry::*};

pub(super) fn columns() -> String {
    format!(
        ",\n i.process_user_time_ms,i.process_kernel_time_ms,{}",
        instance::point_columns(instance::PointClock::CurrentUtc)
    )
}

pub(super) fn decode(row: &Rec) -> Result<Option<NativeCounterSet>> {
    for column in ["major", "server_name", "machine_name", "observed_at"] {
        if optional_text(row, column)?.is_none() {
            return Ok(None);
        }
    }
    for column in ["edition", "process_id", "started_at_ticks", "ms_ticks"] {
        if optional_counter(row, column)?.is_none() {
            return Ok(None);
        }
    }
    let observation = instance::observation(row)?;
    let Some(owner) = observation.context_from_epoch(row)? else { return Ok(None) };
    let epoch = owner.capture_epoch()?;
    let process_id = integer(row, "process_id")?.to_string();
    let mut counters = Vec::new();
    for name in MSSQL_PROCESS_CPU_COUNTERS {
        let Some(value) = optional_counter(row, name)? else { continue };
        counters.push(NativeCounterSample {
            key: name.into(),
            value: MeasurementValue::Integer(value),
            unit: CounterUnit::Milliseconds,
            nature: CounterNature::Cumulative,
            scope: MeasurementScope::Server,
            reset: ResetMarker::Generation(epoch.clone()),
            native_object: MSSQL_PROCESS_CPU_OBJECT.into(),
            native_name: name.into(),
            native_instance: None,
            native_type: None,
        });
    }
    let sample = NativeCounterSet {
        identity: NativeIdentity::new(process_id)
            .qualified("kind", MSSQL_PROCESS_CPU_KIND)
            .qualified("native_object", MSSQL_PROCESS_CPU_OBJECT)
            .qualified("instance_id", owner.identity.telemetry_id()?)
            .qualified("instance_epoch", epoch),
        scope: MeasurementScope::Server,
        counters,
        ratios: Vec::new(),
        ratio_gaps: Vec::new(),
        properties: [
            ("observed_at".into(), canonical_time(&text(row, "observed_at")?)?),
            ("started_at_ticks".into(), integer(row, "started_at_ticks")?.to_string()),
            ("sample_ms_ticks".into(), integer(row, "ms_ticks")?.to_string()),
        ]
        .into(),
    };
    MssqlProcessCpuPoint::from_sample(&sample).map_err(|_| invalid_data())?;
    Ok(Some(sample))
}
