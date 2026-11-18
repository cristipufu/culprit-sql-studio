//! A native scalar counter carries its meaning separately from its name and current value.
use super::*;
mod ratios;
pub use ratios::{CounterRatioGap, CounterRatioIssue, CounterRatioKind, CounterRatioSupport, NativeCounterRatio};

/// The existing generic set ceiling also bounds each native performance-counter acquisition.
pub const MAX_NATIVE_COUNTERS: usize = 128;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CounterNature {
    Cumulative,
    Gauge,
    /// The documented 64-bit native raw-fraction format, never a cooked ratio or independent rate.
    FractionNumerator,
    FractionBase,
    Unknown,
}
impl CounterNature {
    /// Native Windows counter types establish formatting semantics; a '/sec' suffix establishes none.
    pub fn from_windows_type(code: i32) -> Self {
        match code {
            272_696_320 | 272_696_576 => Self::Cumulative,
            65_792 => Self::Gauge,
            537_003_264 => Self::FractionNumerator,
            1_073_939_712 => Self::FractionBase,
            _ => Self::Unknown,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum CounterUnit {
    Count,
    Ratio,
    Bytes,
    Pages { bytes_per_page: u32 },
    Picoseconds,
    Seconds,
    Milliseconds,
    Kibibytes,
    Unknown,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct NativeCounterSample {
    pub key: String,
    pub value: MeasurementValue,
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
            value: MeasurementValue::Integer(value.to_string()),
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
            && self.value.is_valid()
            && !matches!(self.unit, CounterUnit::Pages { bytes_per_page: 0 })
            && valid_reset(&self.reset)
            && [&self.native_object, &self.native_name].into_iter().all(|value| valid_key(value))
            && [&self.native_instance, &self.native_type]
                .into_iter()
                .all(|value| value.as_deref().is_none_or(valid_key))
            && (self.nature != CounterNature::Unknown || self.unit == CounterUnit::Unknown)
            && match self.nature {
                // These codes are the native Windows raw-fraction/base format, independent of engine identity.
                CounterNature::FractionNumerator => {
                    self.unit == CounterUnit::Count
                        && self.value.integer().is_some()
                        && self.native_type.as_deref() == Some("537003264")
                }
                CounterNature::FractionBase => {
                    self.unit == CounterUnit::Count
                        && self.value.integer().is_some()
                        && self.native_type.as_deref() == Some("1073939712")
                }
                _ => true,
            }
    }
}

/// One native object can expose many scalar measurements without multiplying observation identity or source rows.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct NativeCounterSet {
    pub identity: NativeIdentity,
    pub scope: crate::MeasurementScope,
    pub counters: Vec<NativeCounterSample>,
    /// Source-declared pairs from this one native read, never joined across objects or captures.
    pub ratios: Vec<NativeCounterRatio>,
    /// Exact rejected native fraction values remain evidence without becoming valid measurements.
    pub ratio_gaps: Vec<CounterRatioGap>,
    /// Bounded native labels and state codes, never SQL text or connection secrets.
    pub properties: std::collections::BTreeMap<String, String>,
}
impl NativeCounterSet {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&self.identity).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        let mut keys = std::collections::BTreeSet::new();
        self.identity.is_valid()
            && self.counters.len() <= MAX_NATIVE_COUNTERS
            && self
                .counters
                .iter()
                .all(|counter| counter.is_valid() && counter.scope == self.scope && keys.insert(&counter.key))
            && self.properties.len() <= MAX_NATIVE_QUALIFIERS
            && self.properties.iter().all(|(name, value)| valid_key(name) && value.len() <= 2048)
            && ratios::valid(self)
            && (!self.counters.is_empty() || !self.properties.is_empty() || !self.ratio_gaps.is_empty())
    }
}
