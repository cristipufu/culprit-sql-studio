//! Persisted sys.tables configuration, not an observation of memory use, restart or lost rows.
use super::*;

/// Native catalog values from the same original table row. NULLs remain independently unknown.
/// https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-tables-transact-sql
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlTableStorageConfiguration {
    #[serde(deserialize_with = "required_nullable")]
    pub memory_optimized: Option<bool>,
    /// Native tinyint: 0 = SCHEMA_AND_DATA, 1 = SCHEMA_ONLY. This is table configuration, not a backup guarantee.
    #[serde(deserialize_with = "required_nullable")]
    pub durability: Option<u8>,
    #[serde(deserialize_with = "required_nullable")]
    pub durability_desc: Option<String>,
}
impl MssqlTableStorageConfiguration {
    pub fn is_valid(&self) -> bool {
        let code_description = match self.durability {
            Some(0) => Some("SCHEMA_AND_DATA"),
            Some(1) => Some("SCHEMA_ONLY"),
            Some(_) => return false,
            None => None,
        };
        self.durability_desc.as_deref().is_none_or(|description| {
            matches!(description, "SCHEMA_AND_DATA" | "SCHEMA_ONLY")
                && code_description.is_none_or(|expected| expected == description)
        }) && !(self.memory_optimized == Some(false)
            && (self.durability == Some(1) || self.durability_desc.as_deref() == Some("SCHEMA_ONLY")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn original_durability_codes_and_independent_nulls_round_trip_without_inferred_defaults() {
        for (memory_optimized, durability, description) in [
            (Some(false), Some(0), Some("SCHEMA_AND_DATA")),
            (Some(true), Some(0), Some("SCHEMA_AND_DATA")),
            (Some(true), Some(1), Some("SCHEMA_ONLY")),
            (None, None, None),
            (Some(true), None, None),
            (None, Some(1), None),
            (None, None, Some("SCHEMA_ONLY")),
        ] {
            let original = MssqlTableStorageConfiguration {
                memory_optimized,
                durability,
                durability_desc: description.map(str::to_owned),
            };
            assert!(original.is_valid());
            let saved = serde_json::to_value(&original).unwrap();
            let restored: MssqlTableStorageConfiguration = serde_json::from_value(saved).unwrap();
            assert_eq!(restored, original);
        }
    }

    #[test]
    fn contradictory_native_codes_and_descriptions_cannot_become_valid_configuration() {
        for value in [
            json!({"memory_optimized":false,"durability":1,"durability_desc":"SCHEMA_ONLY"}),
            json!({"memory_optimized":false,"durability":null,"durability_desc":"SCHEMA_ONLY"}),
            json!({"memory_optimized":true,"durability":0,"durability_desc":"SCHEMA_ONLY"}),
            json!({"memory_optimized":true,"durability":1,"durability_desc":"SCHEMA_AND_DATA"}),
            json!({"memory_optimized":true,"durability":2,"durability_desc":null}),
            json!({"memory_optimized":null,"durability":null,"durability_desc":"unknown"}),
        ] {
            let state: MssqlTableStorageConfiguration = serde_json::from_value(value).unwrap();
            assert!(!state.is_valid());
        }
    }

    #[test]
    fn omitted_fields_and_non_native_scalar_types_are_refused_instead_of_backfilled() {
        let original = json!({"memory_optimized":null,"durability":null,"durability_desc":null});
        for field in ["memory_optimized", "durability", "durability_desc"] {
            let mut missing = original.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<MssqlTableStorageConfiguration>(missing).is_err());
        }
        for (field, value) in [
            ("memory_optimized", json!(1)),
            ("durability", json!("1")),
            ("durability", json!(-1)),
            ("durability", json!(256)),
            ("durability_desc", json!(false)),
        ] {
            let mut malformed = original.clone();
            malformed[field] = value;
            assert!(serde_json::from_value::<MssqlTableStorageConfiguration>(malformed).is_err());
        }
    }
}
