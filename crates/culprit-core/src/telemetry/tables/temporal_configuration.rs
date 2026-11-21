//! Same-row persisted table configuration. It establishes neither history cleanup nor a parent incarnation.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlTableTemporalConfiguration {
    pub temporal_type_availability: MssqlCatalogAvailability,
    #[serde(deserialize_with = "required_nullable")]
    pub temporal_type: Option<u8>,
    pub temporal_type_desc_availability: MssqlCatalogAvailability,
    #[serde(deserialize_with = "required_nullable")]
    pub temporal_type_desc: Option<String>,
    pub history_table_id_availability: MssqlCatalogAvailability,
    /// Native database-local reference only; ledger tables can also supply it. No independent history owner is acquired.
    #[serde(deserialize_with = "required_nullable")]
    pub history_table_id: Option<u32>,
    pub history_retention_period_availability: MssqlCatalogAvailability,
    /// Native signed int configuration value, never a measured duration or a count of retained/deleted rows.
    #[serde(deserialize_with = "required_nullable")]
    pub history_retention_period: Option<i32>,
    pub history_retention_period_unit_availability: MssqlCatalogAvailability,
    #[serde(deserialize_with = "required_nullable")]
    pub history_retention_period_unit: Option<i32>,
    pub history_retention_period_unit_desc_availability: MssqlCatalogAvailability,
    #[serde(deserialize_with = "required_nullable")]
    pub history_retention_period_unit_desc: Option<String>,
}
impl MssqlTableTemporalConfiguration {
    pub fn is_valid(&self) -> bool {
        available(self.temporal_type_availability, self.temporal_type.as_ref())
            && available(self.temporal_type_desc_availability, self.temporal_type_desc.as_ref())
            && available(self.history_table_id_availability, self.history_table_id.as_ref())
            && available(self.history_retention_period_availability, self.history_retention_period.as_ref())
            && available(self.history_retention_period_unit_availability, self.history_retention_period_unit.as_ref())
            && available(
                self.history_retention_period_unit_desc_availability,
                self.history_retention_period_unit_desc.as_ref(),
            )
            && self.temporal_type.is_none_or(|value| value <= 2)
            && description(
                self.temporal_type.map(i32::from),
                self.temporal_type_desc.as_deref(),
                &[(0, "NON_TEMPORAL_TABLE"), (1, "HISTORY_TABLE"), (2, "SYSTEM_VERSIONED_TEMPORAL_TABLE")],
            )
            && self.history_table_id.is_none_or(|id| id > 0 && id <= i32::MAX as u32)
            && self.history_retention_period_unit.is_none_or(|value| (-1..=6).contains(&value))
            && description(
                self.history_retention_period_unit,
                self.history_retention_period_unit_desc.as_deref(),
                &[
                    (-1, "INFINITE"),
                    (0, "SECOND"),
                    (1, "MINUTE"),
                    (2, "HOUR"),
                    (3, "DAY"),
                    (4, "WEEK"),
                    (5, "MONTH"),
                    (6, "YEAR"),
                ],
            )
    }
}
fn available<T>(availability: MssqlCatalogAvailability, value: Option<&T>) -> bool {
    availability == MssqlCatalogAvailability::Available || value.is_none()
}
fn description(code: Option<i32>, text: Option<&str>, known: &[(i32, &str)]) -> bool {
    text.is_none_or(|text| known.iter().any(|(value, name)| *name == text && code.is_none_or(|code| code == *value)))
}

#[cfg(test)]
mod tests;
