//! Current catalog context is independent of an original module declaration or compilation schema.
use super::super::{CachedStatementContext, MssqlCatalogIndexColumn, MssqlModuleParameters, MssqlObjectIdentity};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{SeqAccess, Visitor},
};
use std::{collections::BTreeSet, fmt, marker::PhantomData};
use ts_rs::TS;

pub const MAX_MSSQL_CURRENT_TYPE_PARAMETERS: usize = 16;
pub const MAX_MSSQL_CURRENT_TYPES: usize = 8;
pub const MAX_MSSQL_CURRENT_TYPE_COLUMNS: usize = 64;
pub const MAX_MSSQL_CURRENT_TYPE_INDEXES: usize = 8;
pub const MAX_MSSQL_CURRENT_TYPE_INDEX_COLUMNS: usize = 64;
/// Native JSON has a separate UTF-16 byte ceiling; this reserves its worst-case JSON representation.
pub const MAX_MSSQL_CURRENT_TYPE_SCHEMA_BYTES: usize = 96 * 1024;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlCurrentTypeSchemaStatus {
    Available,
    Restricted,
    NotCollected,
    Oversized,
    Changed,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCurrentTypeSchema {
    /// Native UTC point at which this current catalog context was read, not the plan's compilation time.
    pub observed_at: String,
    pub status: MssqlCurrentTypeSchemaStatus,
    pub module: Option<MssqlCurrentTypeModule>,
    #[serde(deserialize_with = "bounded::<_, _, MAX_MSSQL_CURRENT_TYPE_PARAMETERS>")]
    pub parameters: Vec<MssqlCurrentTypeParameter>,
    #[serde(deserialize_with = "bounded::<_, _, MAX_MSSQL_CURRENT_TYPES>")]
    pub types: Vec<MssqlCurrentTableType>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCurrentTypeModule {
    pub identity: MssqlObjectIdentity,
    pub schema: String,
    pub name: String,
    pub modified_without_offset: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCurrentTypeParameter {
    pub parameter_id: u32,
    pub name: String,
    pub user_type_id: u32,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCurrentTableType {
    pub schema_id: u32,
    pub user_type_id: u32,
    pub schema: String,
    pub name: String,
    /// sys.table_types.type_table_object_id and the current backing object's native generation.
    pub backing_object: MssqlObjectIdentity,
    pub modified_without_offset: String,
    pub memory_optimized: bool,
    #[serde(deserialize_with = "bounded::<_, _, MAX_MSSQL_CURRENT_TYPE_COLUMNS>")]
    pub columns: Vec<MssqlCurrentTypeColumn>,
    #[serde(deserialize_with = "bounded::<_, _, MAX_MSSQL_CURRENT_TYPE_INDEXES>")]
    pub indexes: Vec<MssqlCurrentTypeIndex>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCurrentTypeColumn {
    pub column_id: u32,
    pub name: String,
    pub user_type_id: u32,
    pub system_type_id: u32,
    pub type_schema: String,
    pub type_name: String,
    /// Native bytes, including -1; never a character count inferred from a plan conversion target.
    pub max_length: i32,
    pub precision: u8,
    pub scale: u8,
    pub nullable: bool,
    pub identity: bool,
    pub computed: bool,
    pub collation: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCurrentTypeIndex {
    pub index_id: u32,
    pub name: String,
    pub type_code: u32,
    pub native_type: String,
    pub unique: bool,
    pub primary_key: bool,
    pub unique_constraint: bool,
    pub disabled: bool,
    pub hypothetical: bool,
    #[serde(deserialize_with = "bounded::<_, _, MAX_MSSQL_CURRENT_TYPE_INDEX_COLUMNS>")]
    pub columns: Vec<MssqlCatalogIndexColumn>,
}

impl MssqlCurrentTypeSchema {
    pub fn unavailable(observed_at: String, status: MssqlCurrentTypeSchemaStatus) -> Self {
        Self { observed_at, status, module: None, parameters: Vec::new(), types: Vec::new() }
    }

    pub fn is_valid(&self) -> bool {
        if !super::super::identity::valid_utc_instant(&self.observed_at)
            || crate::serialization::json_size(self, MAX_MSSQL_CURRENT_TYPE_SCHEMA_BYTES).is_err()
        {
            return false;
        }
        if self.status != MssqlCurrentTypeSchemaStatus::Available {
            return self.module.is_none() && self.parameters.is_empty() && self.types.is_empty();
        }
        let Some(module) = &self.module else { return false };
        if !module.is_valid()
            || !(1..=MAX_MSSQL_CURRENT_TYPE_PARAMETERS).contains(&self.parameters.len())
            || !(1..=MAX_MSSQL_CURRENT_TYPES).contains(&self.types.len())
        {
            return false;
        }
        let mut parameter_ids = BTreeSet::new();
        let mut parameter_names = BTreeSet::new();
        let mut type_ids = BTreeSet::new();
        let mut object_ids = BTreeSet::new();
        self.types.iter().all(|data_type| {
            data_type.is_valid(module.identity.database_id)
                && type_ids.insert(data_type.user_type_id)
                && object_ids.insert(data_type.backing_object.object_id)
                && self.parameters.iter().any(|parameter| parameter.user_type_id == data_type.user_type_id)
        }) && self.parameters.iter().all(|parameter| {
            parameter.parameter_id > 0
                && name(&parameter.name)
                && parameter.name.starts_with('@')
                && !parameter.name.starts_with("@@")
                && parameter.name.len() > 1
                && parameter_ids.insert(parameter.parameter_id)
                && parameter_names.insert(&parameter.name)
                && type_ids.contains(&parameter.user_type_id)
        })
    }

    pub fn is_valid_for(&self, context: &CachedStatementContext, original: Option<&MssqlModuleParameters>) -> bool {
        if !self.is_valid() || !context.is_valid() {
            return false;
        }
        if self.status != MssqlCurrentTypeSchemaStatus::Available {
            return true;
        }
        let Some(original) = original.filter(|original| original.is_valid_for(context)) else { return false };
        let Some(module) = &self.module else { return false };
        module.identity.database_id == original.database_id
            && module.identity.object_id == original.object_id as u32
            && self.parameters.len() == original.table_parameters.len()
            && self.parameters.iter().all(|parameter| {
                let Some(declared) = original.table_parameters.get(&parameter.name) else { return false };
                let Some(data_type) = self.types.iter().find(|value| value.user_type_id == parameter.user_type_id)
                else {
                    return false;
                };
                matches!(declared.as_slice(), [schema, name] if schema == &data_type.schema && name == &data_type.name)
            })
    }
}

impl MssqlCurrentTypeModule {
    fn is_valid(&self) -> bool {
        self.identity.is_valid()
            && name(&self.schema)
            && name(&self.name)
            && super::super::catalog::local_time(&self.modified_without_offset)
    }
}

impl MssqlCurrentTableType {
    fn is_valid(&self, database_id: u32) -> bool {
        if self.schema_id == 0
            || self.user_type_id == 0
            || !name(&self.schema)
            || !name(&self.name)
            || !self.backing_object.is_valid()
            || self.backing_object.database_id != database_id
            || !super::super::catalog::local_time(&self.modified_without_offset)
            || !(1..=MAX_MSSQL_CURRENT_TYPE_COLUMNS).contains(&self.columns.len())
            || self.indexes.len() > MAX_MSSQL_CURRENT_TYPE_INDEXES
            || self.indexes.iter().map(|index| index.columns.len()).sum::<usize>()
                > MAX_MSSQL_CURRENT_TYPE_INDEX_COLUMNS
        {
            return false;
        }
        let mut columns = BTreeSet::new();
        let mut column_names = BTreeSet::new();
        let mut indexes = BTreeSet::new();
        self.columns
            .iter()
            .all(|column| column.is_valid() && columns.insert(column.column_id) && column_names.insert(&column.name))
            && self.indexes.iter().all(|index| index.is_valid(&self.columns) && indexes.insert(index.index_id))
    }
}

impl MssqlCurrentTypeColumn {
    fn is_valid(&self) -> bool {
        self.column_id > 0
            && self.user_type_id > 0
            && self.system_type_id > 0
            && name(&self.name)
            && name(&self.type_schema)
            && name(&self.type_name)
            && (-1..=i16::MAX as i32).contains(&self.max_length)
            && self.collation.as_deref().is_none_or(name)
    }
}

impl MssqlCurrentTypeIndex {
    fn is_valid(&self, columns: &[MssqlCurrentTypeColumn]) -> bool {
        if self.index_id == 0
            || self.type_code == 0
            || self.type_code > u8::MAX as u32
            || !name(&self.name)
            || !name(&self.native_type)
            || (self.primary_key || self.unique_constraint) && !self.unique
            || self.columns.is_empty()
            || self.columns.len() > MAX_MSSQL_CURRENT_TYPE_INDEX_COLUMNS
        {
            return false;
        }
        let mut ordinals = BTreeSet::new();
        let mut column_ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        self.columns.iter().all(|column| {
            column.index_column_id > 0
                && ordinals.insert(column.index_column_id)
                && column_ids.insert(column.column_id)
                && columns.iter().any(|known| known.column_id == column.column_id && known.name == column.name)
                && (column.key_ordinal == 0 || !column.included && keys.insert(column.key_ordinal))
        })
    }
}

fn name(value: &str) -> bool {
    !value.is_empty() && value.encode_utf16().count() <= 128
}

fn bounded<'de, D, T, const LIMIT: usize>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Values<T, const LIMIT: usize>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const LIMIT: usize> Visitor<'de> for Values<T, LIMIT> {
        type Value = Vec<T>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("bounded current SQL Server type metadata")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = sequence.next_element()? {
                if values.len() == LIMIT {
                    return Err(serde::de::Error::custom("current SQL Server type metadata limit exceeded"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Values::<T, LIMIT>(PhantomData))
}

#[cfg(test)]
pub(crate) mod tests;
