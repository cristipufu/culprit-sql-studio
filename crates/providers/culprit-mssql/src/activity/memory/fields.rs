//! One native field table drives optional-column selection, SQL projection, decoding and NULL metadata.
use super::*;
use crate::read::optional_counter;

const OBJECT: &str = "sys.dm_exec_query_memory_grants";
struct Field {
    name: &'static str,
    optional: bool,
    assign: fn(&mut ExecutionGrantObservation, Option<String>),
}
const FIELDS: &[Field] = &[
    Field { name: "requested_memory_kb", optional: false, assign: |row, value| row.requested_kibibytes = value },
    Field { name: "required_memory_kb", optional: false, assign: |row, value| row.required_kibibytes = value },
    Field { name: "granted_memory_kb", optional: false, assign: |row, value| row.granted_kibibytes = value },
    Field { name: "used_memory_kb", optional: false, assign: |row, value| row.used_kibibytes = value },
    Field { name: "max_used_memory_kb", optional: false, assign: |row, value| row.maximum_used_kibibytes = value },
    Field { name: "ideal_memory_kb", optional: false, assign: |row, value| row.ideal_kibibytes = value },
    Field { name: "wait_time_ms", optional: false, assign: |row, value| row.wait_ms = value },
    Field { name: "timeout_sec", optional: false, assign: |row, value| row.timeout_seconds = value },
    Field { name: "dop", optional: false, assign: |row, value| row.degree_of_parallelism = value },
    Field { name: "reserved_worker_count", optional: true, assign: |row, value| row.reserved_workers = value },
    Field { name: "used_worker_count", optional: true, assign: |row, value| row.used_workers = value },
    Field { name: "max_used_worker_count", optional: true, assign: |row, value| row.maximum_used_workers = value },
];
struct Property {
    name: &'static str,
    read: fn(&Rec, &str) -> Result<Option<String>>,
}
const PROPERTIES: &[Property] = &[
    Property { name: "queue_id", read: optional_counter },
    Property { name: "wait_order", read: optional_counter },
    Property { name: "is_next_candidate", read: read_boolean },
    Property { name: "is_small", read: read_boolean },
    Property { name: "query_cost", read: read_cost },
];
fn names() -> impl Iterator<Item = &'static str> {
    FIELDS.iter().map(|field| field.name).chain(PROPERTIES.iter().map(|field| field.name))
}
fn read_boolean(row: &Rec, name: &str) -> Result<Option<String>> {
    Ok(crate::read::optional_boolean(row, name)?.map(|value| u8::from(value).to_string()))
}
fn read_cost(row: &Rec, name: &str) -> Result<Option<String>> {
    Ok(crate::read::optional_nonnegative_number(row, name)?.map(|value| value.to_string()))
}

pub(super) struct Layout {
    available: BTreeSet<String>,
}
pub(super) fn inventory_sql() -> String {
    let names = names().map(|name| format!("N'{name}'")).collect::<Vec<_>>().join(",");
    format!(
        "SELECT /* culprit */ name FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{OBJECT}') AND name IN ({names}) {}",
        super::super::sql::QueryPolicy::MemoryDiagnostic.hints()
    )
}
impl Layout {
    pub async fn read<R: NativeRead>(reader: &mut R) -> Result<Self> {
        let rows = reader
            .query(&inventory_sql(), vec![], FIELDS.len() + PROPERTIES.len() + 1)
            .await
            .map_err(ReadError::into_error)?;
        let mut available = BTreeSet::new();
        for row in &rows {
            let name = crate::read::text(row, "name")?;
            if !self::names().any(|field| field == name) || !available.insert(name) {
                return Err(invalid_data());
            }
        }
        if FIELDS.iter().any(|field| !field.optional && !available.contains(field.name))
            || PROPERTIES.iter().any(|field| !available.contains(field.name))
        {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        Ok(Self { available })
    }
    pub fn projection(&self) -> String {
        names()
            .map(|name| {
                let expression =
                    if self.available.contains(name) { format!("mg.[{name}]") } else { "CAST(NULL AS bigint)".into() };
                format!("{expression} AS grant_{name}")
            })
            .collect::<Vec<_>>()
            .join(",")
    }
    pub fn decode(&self, row: &Rec, grant: &mut ExecutionGrantObservation) -> Result<()> {
        for field in FIELDS {
            if !self.available.contains(field.name) {
                continue;
            }
            grant.available_columns.push(field.name.into());
            let value = optional_counter(row, &format!("grant_{}", field.name))?;
            if value.is_none() {
                grant.native_null_columns.push(field.name.into());
            }
            (field.assign)(grant, value);
        }
        for field in PROPERTIES {
            grant.available_columns.push(field.name.into());
            match (field.read)(row, &format!("grant_{}", field.name))? {
                Some(value) => {
                    grant.native_properties.insert(field.name.into(), value);
                }
                None => grant.native_null_columns.push(field.name.into()),
            }
        }
        for (name, alias) in [
            ("grant_time", "grant_granted_without_offset"),
            ("sql_handle", "grant_sql_handle"),
            ("plan_handle", "grant_plan_handle"),
            ("pool_id", "grant_pool_id"),
            ("group_id", "grant_group_id"),
            ("resource_semaphore_id", "grant_semaphore_id"),
        ] {
            grant.available_columns.push(name.into());
            if matches!(super::super::rows::value(row, alias)?, crate::value::Val::Null) {
                grant.native_null_columns.push(name.into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn columns() -> Vec<Rec> {
    names().map(|name| Rec::of(&[("name", crate::value::Val::Text(name.into()))])).collect()
}

#[cfg(test)]
pub(super) fn observed_layout(columns: &[String]) -> Layout {
    Layout {
        available: names().filter(|name| columns.iter().any(|column| column == name)).map(str::to_owned).collect(),
    }
}
