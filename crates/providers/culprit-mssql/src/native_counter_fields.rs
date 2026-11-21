//! Descriptor-driven native layout and scalar decoding, borrowed by the existing source/session owners.
use crate::read::{NativeRead, Parameter, ReadError, invalid_data};
use crate::{
    read::{optional_boolean, optional_integer, optional_signed_integer, signed_integer, text},
    value::{Rec, Val},
};
use culprit_core::{MeasurementScope, ResetMarker};
use culprit_core::{Result, telemetry::*};
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use culprit_core::telemetry::{MssqlNativeField as Field, MssqlNativeFieldKind as Kind};

fn sql_type(kind: Kind) -> &'static str {
    match kind {
        Kind::BooleanProperty => "bit",
        Kind::TextProperty => "nvarchar(60)",
        _ => "bigint",
    }
}
fn append_native_value(field: &Field, row: &Rec, sample: &mut NativeCounterSet, object: &'static str) -> Result<()> {
    match field.kind {
        Kind::Identity => {
            signed_integer(row, field.name)?;
        }
        Kind::IntegerProperty => {
            sample.properties.insert(
                field.name.into(),
                optional_signed_integer(row, field.name)?.ok_or_else(invalid_data)?.to_string(),
            );
        }
        Kind::TextProperty => {
            let value = crate::read::optional_text(row, field.name)?.ok_or_else(invalid_data)?;
            if value.is_empty() || value.chars().any(char::is_control) {
                return Err(invalid_data());
            }
            sample.properties.insert(field.name.into(), value);
        }
        Kind::BooleanProperty => {
            sample.properties.insert(
                field.name.into(),
                u8::from(optional_boolean(row, field.name)?.ok_or_else(invalid_data)?).to_string(),
            );
        }
        Kind::UnqualifiedCounter { .. } => {
            // Preserve the original scalar without admitting it to generic counter arithmetic. We have no
            // original TF 8134 state witness and must not interpret disabled/unknown instrumentation zeros.
            sample
                .properties
                .insert(field.name.into(), optional_integer(row, field.name)?.ok_or_else(invalid_data)?.to_string());
        }
        Kind::Counter { unit, nature } => {
            let count = optional_integer(row, field.name)?.ok_or_else(invalid_data)?;
            sample.counters.push(NativeCounterSample {
                key: field.name.into(),
                value: MeasurementValue::Integer(count.to_string()),
                unit,
                nature,
                scope: MeasurementScope::Server,
                reset: ResetMarker::Unknown,
                native_object: object.into(),
                native_name: field.name.into(),
                native_instance: None,
                native_type: None,
            });
        }
    }
    Ok(())
}

pub(crate) struct Columns {
    object: &'static str,
    fields: &'static [Field],
    available: BTreeSet<String>,
}
impl Columns {
    pub(crate) async fn read<R: NativeRead>(
        reader: &mut R,
        object: &'static str,
        fields: &'static [Field],
    ) -> Result<Self> {
        let names = fields.iter().map(|field| format!("N'{}'", field.name)).collect::<Vec<_>>().join(",");
        let query = format!(
            "SELECT /* culprit */ TOP (@P1) c.name FROM sys.all_columns c WHERE c.object_id=OBJECT_ID(N'{object}') AND c.name IN ({names}) ORDER BY c.name OPTION(MAXDOP 1)"
        );
        let rows = reader
            .query(&query, vec![Parameter::Integer((fields.len() + 1) as i64)], fields.len() + 1)
            .await
            .map_err(ReadError::into_error)?;
        if rows.len() > fields.len() {
            return Err(CollectionStop::RowLimit.error());
        }
        let mut available = BTreeSet::new();
        for row in rows {
            let name = text(&row, "name")?;
            if !fields.iter().any(|field| field.name == name) || !available.insert(name) {
                return Err(invalid_data());
            }
        }
        if fields.iter().any(|field| field.required_column && !available.contains(field.name)) {
            // Missing and hidden metadata cannot establish this native population. Do not guess a legacy layout.
            return Err(CollectionStop::SourceUnavailable.error());
        }
        Ok(Self { available, object, fields })
    }
    pub(crate) fn projection(&self) -> String {
        self.fields
            .iter()
            .map(|field| {
                if self.available.contains(field.name) {
                    format!("[{}]", field.name)
                } else {
                    format!("CAST(NULL AS {}) AS [{}]", sql_type(field.kind), field.name)
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }
    pub(crate) fn decode(&self, row: &Rec) -> Result<NativeCounterSet> {
        let mut sample = NativeCounterSet {
            ratios: Vec::new(),
            ratio_gaps: Vec::new(),
            identity: NativeIdentity::new(
                signed_integer(
                    row,
                    self.fields
                        .iter()
                        .find(|field| matches!(field.kind, Kind::Identity))
                        .ok_or_else(invalid_data)?
                        .name,
                )?
                .to_string(),
            )
            .qualified("native_object", self.object),
            scope: MeasurementScope::Server,
            counters: Vec::new(),
            properties: BTreeMap::new(),
        };
        let mut unavailable = Vec::new();
        let mut native_null = Vec::new();
        for field in self.fields {
            let value =
                row.columns().find(|(name, _)| *name == field.name).map(|(_, value)| value).ok_or_else(invalid_data)?;
            if !self.available.contains(field.name) {
                if *value != Val::Null {
                    return Err(invalid_data());
                }
                unavailable.push(field.name);
                continue;
            }
            if *value == Val::Null {
                if matches!(field.kind, Kind::Identity) {
                    return Err(invalid_data());
                }
                native_null.push(field.name);
                continue;
            }
            append_native_value(field, row, &mut sample, self.object)?;
        }
        for (key, names) in [("unavailable_columns", unavailable), ("native_null_columns", native_null)] {
            if !names.is_empty() {
                sample.properties.insert(key.into(), serde_json::to_string(&names).map_err(|_| invalid_data())?);
            }
        }
        if !sample.is_valid() {
            return Err(invalid_data());
        }
        Ok(sample)
    }
}

#[cfg(test)]
pub(crate) fn all_columns(fields: &[Field]) -> Vec<Rec> {
    fields.iter().map(|field| Rec::of(&[("name", Val::Text(field.name.into()))])).collect()
}
