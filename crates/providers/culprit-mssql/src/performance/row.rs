//! One decoder preserves native Windows counter types for both standard and explicitly requested populations.
use super::*;
use culprit_core::telemetry::{CounterRatioGap, MeasurementValue};

pub(super) enum CounterMeasurement {
    Value(NativeCounterSample),
    Rejected(CounterRatioGap),
}
pub(super) struct CounterInput {
    object: String,
    name: String,
    instance: Option<String>,
    identity: Vec<u8>,
    key: String,
    native_type: i32,
    value: i64,
}
impl CounterInput {
    pub(super) fn decode(row: &crate::value::Rec) -> Result<Self> {
        let object = text(row, "native_object")?;
        let name = text(row, "native_name")?;
        let instance = optional_text(row, "native_instance")?.ok_or_else(invalid_data)?;
        let instance = (!instance.is_empty()).then_some(instance);
        let identity = serde_json::to_vec(&(&object, &name, &instance)).map_err(|_| invalid_data())?;
        Ok(Self {
            object,
            name,
            instance,
            key: digest(&identity),
            identity,
            native_type: i32::try_from(signed_integer(row, "native_type")?).map_err(|_| invalid_data())?,
            value: signed_integer(row, "value")?,
        })
    }
    pub(super) fn object(&self) -> &str {
        &self.object
    }
    pub(super) fn name(&self) -> &str {
        &self.name
    }
    pub(super) fn instance(&self) -> Option<&str> {
        self.instance.as_deref()
    }
    pub(super) fn identity(&self) -> &[u8] {
        &self.identity
    }
    pub(super) fn measurement(
        self,
        spec: &PerformanceCounterSpec,
        scope: MeasurementScope,
    ) -> Result<CounterMeasurement> {
        if self.value < 0 {
            if spec.ratio.is_none() {
                return Err(invalid_data());
            }
            return Ok(CounterMeasurement::Rejected(CounterRatioGap {
                key: self.key,
                raw_value: self.value.to_string(),
                native_object: self.object,
                native_name: self.name,
                native_instance: self.instance,
                native_type: self.native_type.to_string(),
            }));
        }
        let (nature, unit) = spec.meaning(self.native_type);
        let sample = NativeCounterSample {
            key: self.key,
            value: MeasurementValue::Integer(self.value.to_string()),
            unit,
            nature,
            scope,
            reset: ResetMarker::Unknown,
            native_object: self.object,
            native_name: self.name,
            native_instance: self.instance,
            native_type: Some(self.native_type.to_string()),
        };
        if !sample.is_valid() {
            return Err(invalid_data());
        }
        Ok(CounterMeasurement::Value(sample))
    }
}
