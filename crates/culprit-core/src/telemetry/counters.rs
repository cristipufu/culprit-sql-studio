//! A native scalar counter carries its meaning separately from its name and current value.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CounterNature {
    Cumulative,
    Gauge,
    Unknown,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CounterUnit {
    Count,
    Bytes,
    Pages,
    Seconds,
    Milliseconds,
    Kibibytes,
    Unknown,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct NativeCounterSample {
    pub key: String,
    pub value: String,
    pub unit: CounterUnit,
    pub nature: CounterNature,
    pub scope: crate::MeasurementScope,
    pub reset: crate::ResetMarker,
    pub native_object: String,
    pub native_name: String,
    /// None is a confirmed uninstanced counter; an unavailable identity cannot establish a row.
    pub native_instance: Option<String>,
    pub native_type: Option<String>,
}
impl NativeCounterSample {
    pub fn gauge(
        key: &str,
        value: u64,
        unit: CounterUnit,
        scope: crate::MeasurementScope,
        object: &str,
        name: &str,
    ) -> Self {
        Self {
            key: key.into(),
            value: value.to_string(),
            unit,
            nature: CounterNature::Gauge,
            scope,
            reset: crate::ResetMarker::Unknown,
            native_object: object.into(),
            native_name: name.into(),
            native_instance: None,
            native_type: None,
        }
    }
    pub fn is_valid(&self) -> bool {
        valid_key(&self.key)
            && valid_counter(&self.value)
            && valid_reset(&self.reset)
            && [&self.native_object, &self.native_name].into_iter().all(|value| valid_key(value))
            && [&self.native_instance, &self.native_type]
                .into_iter()
                .all(|value| value.as_deref().is_none_or(valid_key))
            && (self.nature != CounterNature::Unknown || self.unit == CounterUnit::Unknown)
    }
}
