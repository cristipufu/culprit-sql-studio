//! Exact checked index/partition points; NULL/zero wildcard arguments and correlated APPLY are never used.
//! This reuses the streamed source's native projection and decoder, without visiting every database index.
use super::{Layout, decode, layout};
use crate::{
    read::{
        NativeRead, Parameter, ReadError, integer, invalid_data, optional_counter, optional_integer, optional_text,
        text,
    },
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

const MAX_TARGETS: usize = 8;
const PARAMETERS_PER_TARGET: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Target {
    pub database_id: u32,
    pub object_id: u32,
    pub index_id: u32,
    pub partition_number: u32,
    pub partition_id: i64,
}
impl Target {
    pub(crate) fn is_valid(&self) -> bool {
        self.database_id > 0
            && self.database_id <= i16::MAX as u32
            && self.object_id > 0
            && self.object_id <= i32::MAX as u32
            && self.index_id <= i32::MAX as u32
            && self.partition_number > 0
            && self.partition_number <= i32::MAX as u32
            && self.partition_id > 0
    }
    fn matches(&self, identity: &MssqlIndexIdentity) -> bool {
        self.database_id == identity.database_id
            && self.object_id == identity.object_id
            && self.index_id == identity.index_id
            && self.partition_number == identity.partition_number
    }
    pub(crate) fn parameters(&self) -> [Parameter; PARAMETERS_PER_TARGET] {
        [
            i64::from(self.database_id),
            i64::from(self.object_id),
            i64::from(self.index_id),
            i64::from(self.partition_number),
            self.partition_id,
        ]
        .map(Parameter::Integer)
    }
}

pub(crate) struct Point {
    pub observed_at: String,
    pub counters: MssqlIndexOperations,
    pub partition_id: Option<String>,
    pub object: Option<NativeIdentity>,
}

pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    layout: &Layout,
    targets: &BTreeSet<Target>,
) -> Result<BTreeMap<Target, Point>> {
    if targets.is_empty() || targets.len() > MAX_TARGETS || targets.iter().any(|target| !target.is_valid()) {
        return Err(invalid_data());
    }
    let selected = targets.iter().copied().collect::<Vec<_>>();
    let parameters = selected.iter().flat_map(Target::parameters).collect();
    let rows = reader
        .query(&query(layout, selected.len()), parameters, selected.len() + 1)
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > selected.len() {
        return Err(invalid_data());
    }
    let mut points = BTreeMap::new();
    for row in &rows {
        let ordinal = usize::try_from(integer(row, "target_ordinal")?).map_err(|_| invalid_data())?;
        let target = selected.get(ordinal).ok_or_else(invalid_data)?;
        let point = decode_point(row, layout)?;
        if !target.matches(&point.counters.identity)
            || point.partition_id.as_deref().is_some_and(|id| id.parse::<i64>() != Ok(target.partition_id))
            || points.insert(*target, point).is_some()
        {
            return Err(invalid_data());
        }
    }
    Ok(points)
}

fn decode_point(row: &Rec, layout: &Layout) -> Result<Point> {
    let counters = decode(row, layout)?;
    let created = optional_text(row, "object_created_without_offset")?
        .map(|time| crate::sql_time::native_time(&time))
        .transpose()?;
    let modified = optional_text(row, "object_modified_without_offset")?
        .map(|time| crate::sql_time::native_time(&time))
        .transpose()?;
    let object = object_identity(
        counters.identity.object_id,
        Some(counters.identity.index_id),
        optional_integer(row, "catalog_index_type")?,
        created.as_deref(),
        modified.as_deref(),
        optional_counter(row, "catalog_hobt_id")?.as_deref(),
    );
    let partition_id = optional_counter(row, "catalog_partition_id")?;
    if partition_id.as_deref().is_some_and(|id| id == "0") {
        return Err(invalid_data());
    }
    Ok(Point { observed_at: canonical_time(&text(row, "observed_at")?)?, counters, partition_id, object })
}

/// Catalog identity used by the page and the counter point; native names are display context only.
pub(crate) fn object_identity(
    id: u32,
    index: Option<u32>,
    index_type: Option<i64>,
    created: Option<&str>,
    modified: Option<&str>,
    hobt: Option<&str>,
) -> Option<NativeIdentity> {
    let mut native = NativeIdentity::new(id.to_string())
        .qualified("native_object", "sys.all_objects")
        .qualified("created_without_offset", created?)
        .qualified("modified_without_offset", modified?);
    if let Some(index) = index {
        native = native.qualified("index_id", index.to_string());
    }
    if let Some(kind) = index_type {
        native = native.qualified("index_type", kind.to_string());
    }
    if let Some(hobt) = hobt {
        native = native.qualified("hobt_id", hobt);
    }
    native.is_valid().then_some(native)
}

pub(crate) fn query(layout: &Layout, count: usize) -> String {
    let branches = (0..count)
        .map(|ordinal| {
            let first = ordinal * PARAMETERS_PER_TARGET + 1;
            let [database, object, index, partition, partition_id] = [first, first + 1, first + 2, first + 3, first + 4];
            format!(
                r#"SELECT {ordinal} AS target_ordinal,CONVERT(varchar(40),@at,126)+'Z' AS observed_at,
 {projection},part.partition_id AS catalog_partition_id,part.hobt_id AS catalog_hobt_id,i.type AS catalog_index_type,
 CONVERT(varchar(40),CAST(o.create_date AS datetime2),126) AS object_created_without_offset,
 CONVERT(varchar(40),CAST(o.modify_date AS datetime2),126) AS object_modified_without_offset
 FROM sys.dm_db_index_operational_stats(@P{database},@P{object},@P{index},@P{partition}) s
 {joins}
 LEFT JOIN sys.partitions part ON part.partition_id=@P{partition_id} AND part.object_id=s.object_id AND part.index_id=s.index_id
  AND part.partition_number=s.partition_number
 WHERE s.database_id=DB_ID() AND s.database_id=@P{database} AND s.object_id=@P{object}
  AND s.index_id=@P{index} AND s.partition_number=@P{partition}"#,
                projection = layout::projection(layout),
                joins = layout::catalog_joins(),
            )
        })
        .collect::<Vec<_>>()
        .join("\nUNION ALL\n");
    format!("DECLARE /* culprit */ @at datetime2=SYSUTCDATETIME();\n{branches}\nOPTION(MAXDOP 1,LOOP JOIN)")
}

#[cfg(test)]
pub(crate) mod tests;
