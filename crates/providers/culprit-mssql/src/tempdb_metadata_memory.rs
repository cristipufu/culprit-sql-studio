//! One bounded tempdb-specific XTP consumer read. The existing shared source owns its instance/process lifecycle.
//! Native reservation and applicable usage gauges are separate; neither identifies an allocating query or pressure.
use crate::{
    instance,
    read::{
        NativeRead, Parameter, ReadError, boolean, integer, invalid_data, one, optional_boolean, optional_counter,
        optional_text, signed_integer, text,
    },
    sql_time::native_time,
    value::Rec,
};
use culprit_core::{Error, ErrorCode, MeasurementScope, Result, t, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

const REQUIRED_COLUMNS: i64 = 6;

const LAYOUT: &str = r#"SELECT /* culprit */
 CONVERT(int,SERVERPROPERTY('IsTempDbMetadataMemoryOptimized')) AS [metadata_enabled],
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS [observed_at],
 DB_ID() AS [database_id],CONVERT(varchar(40),CAST(d.create_date AS datetime2),126) AS [database_created],
 CASE WHEN HAS_PERMS_BY_NAME(N'tempdb','DATABASE','VIEW DATABASE PERFORMANCE STATE')=1 THEN 1 ELSE 0 END AS [permission],
 CASE WHEN HAS_PERMS_BY_NAME(N'tempdb','DATABASE','VIEW DEFINITION')=1
  OR HAS_PERMS_BY_NAME(NULL,NULL,'VIEW ANY DEFINITION')=1 THEN 1 ELSE 0 END AS [metadata_visible],
 (SELECT COUNT(*) FROM sys.all_columns
  WHERE object_id=OBJECT_ID(N'sys.dm_db_xtp_memory_consumers')
   AND name IN(N'memory_consumer_id',N'memory_consumer_type',N'memory_consumer_type_desc',
    N'memory_consumer_desc',N'allocated_bytes',N'used_bytes')) AS [columns_present]
 FROM sys.databases d WHERE d.database_id=DB_ID() AND d.name=N'tempdb' OPTION(MAXDOP 1)"#;

const CONSUMERS: &str = r#"DECLARE /* culprit */ @observed_at varchar(40)=CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z';
 SELECT TOP (@P1) /* culprit */ @observed_at AS [observed_at],
 CONVERT(int,SERVERPROPERTY('IsTempDbMetadataMemoryOptimized')) AS [metadata_enabled],
 DB_ID() AS [database_id],CONVERT(varchar(40),CAST(d.create_date AS datetime2),126) AS [database_created],
 c.memory_consumer_id,c.memory_consumer_type,c.memory_consumer_type_desc,c.memory_consumer_desc,
 c.allocated_bytes,c.used_bytes
 FROM sys.dm_db_xtp_memory_consumers c
 CROSS JOIN sys.databases d
 WHERE d.database_id=DB_ID() AND d.name=N'tempdb' AND c.memory_consumer_type<>0 OPTION(MAXDOP 1)"#;

pub(crate) struct Read {
    pub counters: Vec<NativeCounterSet>,
    pub complete_scope: bool,
    pub error_code: Option<ErrorCode>,
}

struct Layout {
    observed_at: String,
    database_created: String,
}

/// Only VARHEAP has documented used-byte semantics in this database-level native view.
/// Native platform/owner facts have already been acquired by the shared StateSource; no second owner reader exists.
pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    owner: &instance::Context,
    limit: usize,
) -> Result<Option<Read>> {
    if !MSSQL_TEMPDB_METADATA_EDITIONS.contains(&owner.engine_edition()) {
        return Ok(None);
    }
    if owner.major < MSSQL_TEMPDB_METADATA_MIN_MAJOR {
        return Err(Error::new(ErrorCode::Unsupported, t!("collection.unsupported")));
    }
    if limit == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    let row = one(reader, &in_tempdb(LAYOUT)).await?;
    if integer(&row, "database_id")? != 2 {
        return Err(CollectionStop::SourceChanged.error());
    }
    let layout = Layout {
        observed_at: canonical_time(&text(&row, "observed_at")?)?,
        database_created: native_time(&text(&row, "database_created")?)?,
    };
    let enabled = optional_boolean(&row, "metadata_enabled")?;
    let feature = feature(&layout, owner, enabled)?;
    if enabled != Some(true) {
        return Ok(Some(Read { counters: vec![feature], complete_scope: enabled == Some(false), error_code: None }));
    }
    let error_code = if !boolean(&row, "permission")? || !boolean(&row, "metadata_visible")? {
        Some(ErrorCode::PermissionDenied)
    } else if integer(&row, "columns_present")? != REQUIRED_COLUMNS {
        Some(ErrorCode::Unsupported)
    } else {
        None
    };
    if error_code.is_some() || limit == 1 {
        return Ok(Some(Read { counters: vec![feature], complete_scope: false, error_code }));
    }
    let limit = (limit - 1).min(MAX_MSSQL_TEMPDB_METADATA_CONSUMERS);
    let query = format!("{}, N'@P1 int', @P1=@P1", in_tempdb(CONSUMERS));
    let mut rows = reader
        .query(&query, vec![Parameter::Integer((limit + 1) as i64)], limit + 1)
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > limit + 1 {
        return Err(CollectionStop::RowLimit.error());
    }
    let complete_scope = rows.len() <= limit;
    rows.truncate(limit);
    let mut identities = BTreeSet::new();
    let mut observed_at = None;
    let mut counters = rows
        .iter()
        .map(|row| {
            let sample = decode(row, &layout, owner)?;
            let at = sample.properties.get("observed_at").ok_or_else(invalid_data)?;
            if observed_at.as_ref().is_some_and(|previous| previous != at)
                || !identities.insert(sample.identity.clone())
            {
                return Err(invalid_data());
            }
            observed_at = Some(at.clone());
            Ok(sample)
        })
        .collect::<Result<Vec<_>>>()?;
    counters.insert(0, feature);
    Ok(Some(Read { counters, complete_scope, error_code: None }))
}

// This fixed context change belongs only to the nested statement. It cannot leave a pooled reader in tempdb.
// DB_ID() is returned by that same native batch, rather than inferred from a three-part DMV/catalog name.
// https://learn.microsoft.com/sql/relational-databases/system-stored-procedures/sp-executesql-transact-sql
fn in_tempdb(sql: &'static str) -> String {
    format!("EXEC /* culprit */ sys.sp_executesql N'USE /* culprit */ [tempdb]; {}'", sql.replace('\'', "''"))
}

fn feature(layout: &Layout, owner: &instance::Context, enabled: Option<bool>) -> Result<NativeCounterSet> {
    let mut properties = BTreeMap::from([
        ("native_major".into(), owner.major.to_string()),
        ("native_edition".into(), owner.engine_edition().to_string()),
        ("observed_at".into(), layout.observed_at.clone()),
    ]);
    match enabled {
        Some(enabled) => {
            properties.insert("metadata_enabled".into(), i32::from(enabled).to_string());
        }
        None => {
            properties.insert("metadata_enabled_state".into(), "native-null".into());
        }
    }
    let sample = NativeCounterSet {
        identity: NativeIdentity::new("feature")
            .qualified("kind", MSSQL_TEMPDB_METADATA_FEATURE_KIND)
            .qualified("native_object", "SERVERPROPERTY")
            .qualified("native_property", "IsTempDbMetadataMemoryOptimized")
            .qualified("database_id", "2")
            .qualified("database_created", &layout.database_created),
        scope: MeasurementScope::Server,
        counters: vec![],
        ratios: vec![],
        ratio_gaps: vec![],
        properties,
    };
    if !sample.is_valid() {
        return Err(invalid_data());
    }
    Ok(sample)
}

fn decode(row: &Rec, layout: &Layout, owner: &instance::Context) -> Result<NativeCounterSet> {
    let observed_at = canonical_time(&text(row, "observed_at")?)?;
    let database_created = native_time(&text(row, "database_created")?)?;
    if integer(row, "database_id")? != 2
        || database_created != layout.database_created
        || !boolean(row, "metadata_enabled")?
        || ObservationBounds::new(&layout.observed_at, &observed_at).is_none()
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    let consumer = signed_integer(row, "memory_consumer_id")?.to_string();
    let kind = i32::try_from(integer(row, "memory_consumer_type")?).map_err(|_| invalid_data())?;
    if kind <= 0 {
        return Err(invalid_data());
    }
    let kind_description = text(row, "memory_consumer_type_desc")?;
    let expected = match kind {
        2 => Some("VARHEAP"),
        3 => Some("HASH"),
        4 => Some("PGPOOL"),
        _ => None,
    };
    if kind_description.encode_utf16().count() > 64 || expected.is_some_and(|expected| kind_description != expected) {
        return Err(invalid_data());
    }
    let description = optional_text(row, "memory_consumer_desc")?;
    if description.as_ref().is_some_and(|value| value.encode_utf16().count() > 64) {
        return Err(invalid_data());
    }
    let allocated = optional_counter(row, "allocated_bytes")?;
    let used = optional_counter(row, "used_bytes")?;
    let mut properties = BTreeMap::from([
        ("metadata_enabled".into(), "1".into()),
        ("native_major".into(), owner.major.to_string()),
        ("native_edition".into(), owner.engine_edition().to_string()),
        ("observed_at".into(), observed_at),
        ("memory_consumer_type".into(), kind.to_string()),
        ("memory_consumer_type_desc".into(), kind_description),
    ]);
    if let Some(description) = description {
        properties.insert("memory_consumer_desc".into(), description);
    }
    let mut counters = Vec::new();
    if let Some(allocated) = allocated {
        counters.push(gauge("allocated_bytes", &allocated)?);
    }
    if kind == 2 {
        properties.insert("used_bytes_state".into(), if used.is_some() { "measured" } else { "native-null" }.into());
        if let Some(used) = used {
            counters.push(gauge("used_bytes", &used)?);
        }
    } else {
        properties.insert("used_bytes_state".into(), "not-applicable".into());
        if let Some(used) = used {
            // Preserve the returned value without promoting undocumented usage semantics to a gauge.
            properties.insert("native_used_bytes".into(), used);
        }
    }
    let sample = NativeCounterSet {
        identity: NativeIdentity::new(consumer)
            .qualified("kind", MSSQL_TEMPDB_METADATA_CONSUMER_KIND)
            .qualified("native_object", MSSQL_TEMPDB_METADATA_CONSUMERS)
            .qualified("database_id", "2")
            .qualified("database_created", database_created),
        scope: MeasurementScope::Server,
        counters,
        ratios: vec![],
        ratio_gaps: vec![],
        properties,
    };
    if !sample.is_valid() {
        return Err(invalid_data());
    }
    Ok(sample)
}

fn gauge(name: &str, value: &str) -> Result<NativeCounterSample> {
    Ok(NativeCounterSample::gauge(
        name,
        value.parse::<u64>().map_err(|_| invalid_data())?,
        CounterUnit::Bytes,
        MeasurementScope::Server,
        MSSQL_TEMPDB_METADATA_CONSUMERS,
        name,
    ))
}

#[cfg(test)]
pub(crate) mod tests;
