//! Shared native-field admission for closed SQL Server descriptor populations; no discovery or rate inference.
use super::*;
use MssqlNativeFieldKind::{BooleanProperty, Identity, IntegerProperty, TextProperty};
use std::collections::BTreeSet;
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MssqlNativeFieldKind {
    Identity,
    IntegerProperty,
    TextProperty,
    BooleanProperty,
    Counter {
        unit: CounterUnit,
        nature: CounterNature,
    },
    /// Read-only native data whose collection prerequisite was not observed. Never a scalar measurement.
    UnqualifiedCounter {
        unit: CounterUnit,
        nature: CounterNature,
        required_global_trace_flag: u32,
    },
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MssqlNativeField {
    pub name: &'static str,
    pub kind: MssqlNativeFieldKind,
    pub required_column: bool,
    /// Documentation only: acquisition uses actual original catalog-column visibility, never version guessing.
    pub introduced_in: Option<u32>,
}
impl MssqlNativeField {
    pub const fn counter(name: &'static str, unit: CounterUnit, nature: CounterNature) -> Self {
        Self { name, kind: MssqlNativeFieldKind::Counter { unit, nature }, required_column: false, introduced_in: None }
    }
    pub const fn counter_since(name: &'static str, unit: CounterUnit, nature: CounterNature, major: u32) -> Self {
        Self { introduced_in: Some(major), ..Self::counter(name, unit, nature) }
    }
}
/// Original field absence is explicit; native NULL and an unavailable catalog column are different facts.
pub enum MssqlNativeFieldValue<'a> {
    Observed(&'a str),
    Unqualified { native_value: &'a str, required_global_trace_flag: u32 },
    NativeNull,
    ColumnUnavailable,
}

/// A complete original native roster; names and current values never replace units, nulls or scope proof.
pub struct MssqlNativeFieldRoster<'a> {
    sample: &'a NativeCounterSet,
    unavailable: BTreeSet<&'static str>,
    native_null: BTreeSet<&'static str>,
    object: &'static str,
    fields: &'static [MssqlNativeField],
}
impl<'a> MssqlNativeFieldRoster<'a> {
    pub fn from_sample(
        sample: &'a NativeCounterSet,
        object: &'static str,
        fields: &'static [MssqlNativeField],
    ) -> Result<Self> {
        if !sample.is_valid()
            || sample.scope != crate::MeasurementScope::Server
            || sample.identity.qualifiers.len() != 1
            || sample.identity.qualifiers.get("native_object").map(String::as_str) != Some(object)
            || !signed_native(&sample.identity.id)
            || !sample.ratios.is_empty()
            || !sample.ratio_gaps.is_empty()
        {
            return Err(invalid());
        }
        let point = Self {
            sample,
            object,
            fields,
            unavailable: missing_columns(sample, "unavailable_columns", fields)?,
            native_null: missing_columns(sample, "native_null_columns", fields)?,
        };
        if !point.unavailable.is_disjoint(&point.native_null)
            || sample.properties.keys().any(|name| {
                !matches!(name.as_str(), "unavailable_columns" | "native_null_columns")
                    && !fields.iter().any(|field| {
                        field.name == name
                            && matches!(
                                field.kind,
                                IntegerProperty
                                    | TextProperty
                                    | BooleanProperty
                                    | MssqlNativeFieldKind::UnqualifiedCounter { .. }
                            )
                    })
            })
            || sample.counters.iter().any(|counter| {
                !fields.iter().any(|field| {
                    field.name == counter.key && matches!(field.kind, MssqlNativeFieldKind::Counter { .. })
                })
            })
        {
            return Err(invalid());
        }
        for field in fields {
            point.validate_field(field)?;
        }
        Ok(point)
    }
    pub fn native_id(&self) -> &'a str {
        &self.sample.identity.id
    }
    pub fn sample(&self) -> &'a NativeCounterSet {
        self.sample
    }
    pub fn field(&self, field: &MssqlNativeField) -> Result<MssqlNativeFieldValue<'a>> {
        if !self.fields.contains(field) {
            return Err(invalid());
        }
        if self.unavailable.contains(field.name) {
            Ok(MssqlNativeFieldValue::ColumnUnavailable)
        } else if self.native_null.contains(field.name) {
            Ok(MssqlNativeFieldValue::NativeNull)
        } else {
            // from_sample established exactly one original representation for every field.
            let value = self.observed(field).ok_or_else(invalid)?;
            Ok(match field.kind {
                MssqlNativeFieldKind::UnqualifiedCounter { required_global_trace_flag, .. } => {
                    MssqlNativeFieldValue::Unqualified { native_value: value, required_global_trace_flag }
                }
                _ => MssqlNativeFieldValue::Observed(value),
            })
        }
    }
    fn observed(&self, field: &MssqlNativeField) -> Option<&'a str> {
        match field.kind {
            Identity => Some(self.native_id()),
            MssqlNativeFieldKind::Counter { .. } => {
                let counter = self.sample.counters.iter().find(|counter| counter.key == field.name)?;
                let MeasurementValue::Integer(value) = &counter.value else { return None };
                Some(value.as_str())
            }
            _ => self.sample.properties.get(field.name).map(String::as_str),
        }
    }
    fn has_representation(&self, field: &MssqlNativeField) -> bool {
        match field.kind {
            Identity => true,
            MssqlNativeFieldKind::Counter { .. } => {
                self.sample.counters.iter().any(|counter| counter.key == field.name)
            }
            _ => self.sample.properties.contains_key(field.name),
        }
    }
    fn validate_field(&self, field: &MssqlNativeField) -> Result<()> {
        if self.unavailable.contains(field.name) || self.native_null.contains(field.name) {
            if matches!(field.kind, Identity)
                || self.unavailable.contains(field.name) && field.required_column
                || self.has_representation(field)
            {
                return Err(invalid());
            }
            return Ok(());
        }
        let value = self.observed(field).ok_or_else(invalid)?;
        let valid = match field.kind {
            Identity | IntegerProperty => signed_native(value),
            TextProperty => !value.is_empty() && !value.chars().any(char::is_control),
            BooleanProperty => matches!(value, "0" | "1"),
            MssqlNativeFieldKind::UnqualifiedCounter { .. } => native_count(value),
            MssqlNativeFieldKind::Counter { unit, nature } => {
                let counter =
                    self.sample.counters.iter().find(|counter| counter.key == field.name).ok_or_else(invalid)?;
                counter.native_object == self.object
                    && counter.native_name == field.name
                    && counter.native_instance.is_none()
                    && counter.native_type.is_none()
                    && counter.unit == unit
                    && counter.nature == nature
                    && counter.reset == crate::ResetMarker::Unknown
                    && native_count(value)
            }
        };
        valid.then_some(()).ok_or_else(invalid)
    }
}
fn missing_columns(
    sample: &NativeCounterSet,
    key: &str,
    fields: &'static [MssqlNativeField],
) -> Result<BTreeSet<&'static str>> {
    let Some(raw) = sample.properties.get(key) else { return Ok(BTreeSet::new()) };
    let names: Vec<String> = serde_json::from_str(raw).map_err(|_| invalid())?;
    if names.is_empty() || names.len() > fields.len() {
        return Err(invalid());
    }
    let mut columns = BTreeSet::new();
    for name in names {
        let field = fields.iter().find(|field| field.name == name).ok_or_else(invalid)?;
        if !columns.insert(field.name) {
            return Err(invalid());
        }
    }
    Ok(columns)
}
fn native_count(value: &str) -> bool {
    value.parse::<i64>().is_ok_and(|number| number >= 0 && number.to_string() == value)
}
fn signed_native(value: &str) -> bool {
    value.parse::<i64>().is_ok_and(|number| number.to_string() == value)
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
