//! Original native module declarations, without SQL bodies, defaults or parameter values.

use crate::{
    plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance},
    telemetry::CachedStatementContext,
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor},
};
use std::{collections::BTreeMap, fmt};
use ts_rs::TS;

const MAX_TABLE_PARAMETERS: usize = 128;
const MAX_IDENTIFIER_UTF16: usize = 128;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlModuleParameters {
    pub database_id: u32,
    pub object_id: i32,
    pub module_number: i32,
    /// Original batch-text token. It is not a cache-counter generation or an event timestamp.
    pub sql_handle: String,
    /// Exact native spellings; no universal case folding or inference about unlisted variables.
    #[serde(deserialize_with = "parameters")]
    pub table_parameters: BTreeMap<String, Vec<String>>,
}

impl MssqlModuleParameters {
    pub fn is_valid_for(&self, context: &CachedStatementContext) -> bool {
        context.is_valid()
            && context.identity.has_plan_handle()
            && context.identity.has_sql_handle()
            && self.database_id == context.database_id
            && self.object_id > 0
            && self.module_number == 1
            && self.sql_handle.eq_ignore_ascii_case(&context.identity.sql_handle)
            && self.table_parameters.len() <= MAX_TABLE_PARAMETERS
            && self.table_parameters.iter().all(|(name, data_type)| {
                identifier(name)
                    && name.starts_with('@')
                    && !name.starts_with("@@")
                    && name.len() > 1
                    && (1..=2).contains(&data_type.len())
                    && data_type.iter().all(|part| identifier(part))
            })
    }

    pub fn input(&self, context: &CachedStatementContext, native_name: &str) -> Option<RelationalInput> {
        (self.is_valid_for(context) && self.table_parameters.contains_key(native_name)).then_some(RelationalInput {
            kind: RelationalInputKind::TableValuedParameter,
            provenance: RelationalInputProvenance::NativeParameterMetadata,
        })
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.encode_utf16().count() <= MAX_IDENTIFIER_UTF16 && !value.chars().any(char::is_control)
}

fn parameters<'de, D: Deserializer<'de>>(deserializer: D) -> Result<BTreeMap<String, Vec<String>>, D::Error> {
    struct Parameters;
    impl<'de> Visitor<'de> for Parameters {
        type Value = BTreeMap<String, Vec<String>>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("bounded native table parameter metadata")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((name, value)) = access.next_entry()? {
                if values.len() >= MAX_TABLE_PARAMETERS || values.insert(name, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate or oversized native parameter metadata"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Parameters)
}

#[cfg(test)]
mod tests;
