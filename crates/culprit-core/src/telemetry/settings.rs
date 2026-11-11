//! Native configuration observations keep scope, pending values and default provenance separate from display.
use super::*;
const MAX_SETTING_VALUE_BYTES: usize = 16 * 1024;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SettingNature {
    Configuration,
    State,
    Measurement,
    Unknown,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SettingDefaultSource {
    NativeFlag,
    NativeValue,
    EngineSpec,
    Unknown,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SettingObservation {
    pub source: String,
    pub namespace: String,
    pub name: String,
    pub scope: crate::MeasurementScope,
    pub nature: SettingNature,
    /// The source's native value. Absence is not an empty string or zero.
    pub value: Option<String>,
    /// Separate native effective value, where the source exposes configured and active values.
    pub value_in_use: Option<String>,
    pub value_for_secondary: Option<String>,
    /// Native unit and configuration provenance, independent of display formatting.
    pub unit: Option<String>,
    pub native_context: Option<String>,
    pub native_source: Option<String>,
    pub pending_restart: Option<bool>,
    pub native_is_default: Option<bool>,
    pub default_value: Option<String>,
    pub default_source: SettingDefaultSource,
}
impl SettingObservation {
    pub fn is_valid(&self) -> bool {
        [&self.source, &self.namespace, &self.name].into_iter().all(|value| valid_key(value))
            && [
                &self.value,
                &self.value_in_use,
                &self.value_for_secondary,
                &self.default_value,
                &self.unit,
                &self.native_context,
                &self.native_source,
            ]
            .into_iter()
            .flatten()
            .all(|value| value.len() <= MAX_SETTING_VALUE_BYTES)
            && match self.default_source {
                SettingDefaultSource::Unknown => self.default_value.is_none(),
                SettingDefaultSource::NativeFlag => {
                    self.default_value.is_some()
                        && self.native_is_default == Some(true)
                        && self.default_value == self.value
                }
                SettingDefaultSource::EngineSpec | SettingDefaultSource::NativeValue => self.default_value.is_some(),
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> SettingObservation {
        SettingObservation {
            source: "scoped settings".into(),
            namespace: "sys.database_scoped_configurations".into(),
            name: "MAXDOP".into(),
            scope: crate::MeasurementScope::Database,
            nature: SettingNature::Configuration,
            value: None,
            value_in_use: None,
            value_for_secondary: None,
            unit: None,
            native_context: None,
            native_source: None,
            pending_restart: None,
            native_is_default: None,
            default_value: None,
            default_source: SettingDefaultSource::Unknown,
        }
    }
    #[test]
    fn unknown_setting_values_and_defaults_are_valid_without_invented_empty_strings() {
        assert!(observation().is_valid());
        let mut value = observation();
        value.default_source = SettingDefaultSource::NativeFlag;
        value.native_is_default = Some(true);
        assert!(!value.is_valid(), "a NULL value cannot establish a default value");
        value.value = Some("0".into());
        value.default_value = value.value.clone();
        assert!(value.is_valid());
        value.native_is_default = Some(false);
        assert!(!value.is_valid(), "the native flag must support the recorded provenance");
        value.default_source = SettingDefaultSource::Unknown;
        assert!(!value.is_valid(), "a default value needs an established source");
        value.default_source = SettingDefaultSource::EngineSpec;
        assert!(value.is_valid());
    }
    #[test]
    fn setting_identity_and_each_value_are_bounded_before_export() {
        for field in ["source", "namespace", "name"] {
            let mut value = observation();
            match field {
                "source" => value.source.clear(),
                "namespace" => value.namespace.clear(),
                _ => value.name.clear(),
            }
            assert!(!value.is_valid());
        }
        let mut value = observation();
        value.value_for_secondary = Some("x".repeat(MAX_SETTING_VALUE_BYTES));
        assert!(value.is_valid());
        value.value_for_secondary.as_mut().unwrap().push('x');
        assert!(!value.is_valid());
    }
}
