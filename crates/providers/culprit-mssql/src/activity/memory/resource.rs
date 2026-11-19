//! Optional shared semaphore counters use this grant acquisition, never an independent capacity scan.
use super::*;
use crate::read::{optional_counter, optional_signed_integer};

const OBJECT: &str = "sys.dm_exec_query_resource_semaphores";
const KEYS: &[&str] = &["pool_id", "resource_semaphore_id"];

struct Field {
    name: &'static str,
    read: fn(&Rec, &str) -> Result<Option<String>>,
    assign: fn(&mut ExecutionGrantResourceObservation, Option<String>),
}

const FIELDS: &[Field] = &[
    Field { name: "available_memory_kb", read: signed, assign: |row, value| row.available_kibibytes = value },
    Field { name: "granted_memory_kb", read: optional_counter, assign: |row, value| row.granted_kibibytes = value },
    Field { name: "grantee_count", read: optional_counter, assign: |row, value| row.grantee_count = value },
    Field { name: "waiter_count", read: optional_counter, assign: |row, value| row.waiter_count = value },
];

pub(super) const COLUMN_COUNT: usize = KEYS.len() + FIELDS.len();

fn names() -> impl Iterator<Item = &'static str> {
    KEYS.iter().copied().chain(FIELDS.iter().map(|field| field.name))
}

fn inventory_name(name: &str) -> String {
    format!("resource.{name}")
}

fn alias(name: &str) -> String {
    format!("resource_{name}")
}

fn signed(row: &Rec, name: &str) -> Result<Option<String>> {
    Ok(optional_signed_integer(row, name)?.map(|value| value.to_string()))
}

pub(super) fn qualified_scope(owner: &crate::local_source::Context) -> bool {
    owner.is_qualified()
        && owner.source_instance().is_some_and(|resource| resource.kind == SharedResourceKind::Instance)
}

pub(super) fn inventory_sql(shared_scope: bool) -> String {
    let selected = names().map(|name| format!("N'{name}'")).collect::<Vec<_>>().join(",");
    let shared_scope = u8::from(shared_scope);
    format!(
        "SELECT N'resource.'+name AS name FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{OBJECT}') AND name IN ({selected}) \
         AND {shared_scope}=1 \
         AND (HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE')=1 \
         OR (TRY_CONVERT(int,SERVERPROPERTY('ProductMajorVersion'))<16 AND HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE')=1))"
    )
}

pub(super) fn is_inventory_name(name: &str) -> bool {
    names().any(|field| inventory_name(field) == name)
}

pub(super) struct Layout {
    available: bool,
}

impl Layout {
    pub(super) fn from_inventory(columns: &BTreeSet<String>, shared_scope: bool) -> Self {
        Self { available: shared_scope && names().all(|name| columns.contains(&inventory_name(name))) }
    }

    pub(super) fn join(&self) -> &'static str {
        if self.available {
            "LEFT JOIN sys.dm_exec_query_resource_semaphores rs ON rs.pool_id=mg.pool_id AND rs.resource_semaphore_id=mg.resource_semaphore_id"
        } else {
            ""
        }
    }

    pub(super) fn projection(&self) -> String {
        names()
            .map(|name| {
                let value = if self.available { format!("rs.[{name}]") } else { "CAST(NULL AS bigint)".into() };
                format!("{value} AS {}", alias(name))
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    pub(super) fn decode(&self, row: &Rec, grant: &mut ExecutionGrantObservation) -> Result<()> {
        if !self.available {
            return Ok(());
        }
        let pool = optional_counter(row, &alias("pool_id"))?;
        let semaphore = optional_counter(row, &alias("resource_semaphore_id"))?;
        let mut observed = ExecutionGrantResourceObservation {
            available_kibibytes: None,
            granted_kibibytes: None,
            grantee_count: None,
            waiter_count: None,
        };
        let mut has_value = false;
        for field in FIELDS {
            let value = (field.read)(row, &alias(field.name))?;
            has_value |= value.is_some();
            record_column(grant, field.name, value.is_none());
            (field.assign)(&mut observed, value);
        }
        for (name, value) in [("pool_id", &pool), ("resource_semaphore_id", &semaphore)] {
            record_column(grant, name, value.is_none());
        }
        match (pool, semaphore) {
            (None, None) if !has_value => Ok(()),
            (Some(pool), Some(semaphore))
                if grant.resource.as_ref().is_some_and(|resource| {
                    resource.id == pool && resource.qualifiers.get("resource_semaphore_id") == Some(&semaphore)
                }) && observed.is_valid() =>
            {
                grant.resource_observation = Some(observed);
                Ok(())
            }
            _ => Err(invalid_data()),
        }
    }
}

fn record_column(grant: &mut ExecutionGrantObservation, name: &str, null: bool) {
    fields::record_column(grant, inventory_name(name), null);
}

#[cfg(test)]
pub(super) fn columns() -> Vec<Rec> {
    names().map(|name| Rec::of(&[("name", crate::value::Val::Text(inventory_name(name)))])).collect()
}
