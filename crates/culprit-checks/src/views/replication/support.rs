use culprit_core::{
    MeasurementScope, ReplicationEntry, ReplicationField, ReplicationReference, format, t, telemetry::*,
};
use std::fmt::Display;

pub(super) fn entry(
    row: &ReplicationObservation,
    scope: MeasurementScope,
    title: String,
    explanation: String,
) -> ReplicationEntry {
    ReplicationEntry {
        reference: ReplicationReference { source: row.source.clone(), observation_key: row.fact.key() },
        scope,
        conditions: vec![],
        title,
        explanation,
        fields: vec![],
        groups: vec![],
        session: None,
        unresolved_session_id: None,
        investigation: t!("view.replication.investigate"),
        verify: t!("view.replication.verify"),
    }
}

/// Native identifiers, codes and original date spellings are data; they do not borrow numeric measurement units.
pub(super) fn native<T: Display>(key: &str, label: String, value: Option<T>) -> ReplicationField {
    ReplicationField { key: key.into(), label, value: value.map(|value| value.to_string()) }
}

/// Exact integer and decimal measurements are formatted directly from native text, without an f64 round trip.
pub(super) fn number(key: &str, label: String, value: Option<&str>) -> ReplicationField {
    ReplicationField { key: key.into(), label, value: value.map(|value| format::digits(value.into())) }
}

pub(super) fn counter(key: &str, label: String, value: Option<&NativeCounterSample>) -> ReplicationField {
    let value = value.map(|value| match &value.value {
        MeasurementValue::Integer(value) => format::digits(value.clone()),
        MeasurementValue::Real(value) => format::digits(value.to_string()),
    });
    ReplicationField { key: key.into(), label, value }
}

pub(super) fn property<'a>(sample: &'a NativeReplicationSample, key: &str) -> Option<&'a str> {
    sample.measurements.properties.get(key).map(String::as_str)
}

pub(super) fn measurement<'a>(sample: &'a NativeReplicationSample, key: &str) -> Option<&'a NativeCounterSample> {
    sample.measurements.counters.iter().find(|counter| counter.key == key)
}
