//! Database-scoped recommendations and optional query associations share strict native decoding.
use crate::read::{integer, invalid_data, optional_text, required_nonnegative_number, text};
use culprit_core::{Error, MissingIndex, Result, telemetry::*};

use crate::sql_time::utc_local_iso;
use crate::{
    read::{NativeRead, Parameter, ReadError, optional_counter, rows as query},
    value::Rec,
};

fn missing_indexes_sql() -> String {
    format!(
        r#"
SELECT TOP (@P1) DB_ID() AS database_id,t.object_id,
 CONVERT(varchar(40),t.create_date,126) AS created_without_offset,
 mid.index_handle,s.name AS schema_name, t.name AS table_name,
  mid.equality_columns, mid.inequality_columns, mid.included_columns,
  migs.user_seeks, migs.user_scans, migs.avg_total_user_cost, migs.avg_user_impact,
  {last_used} AS last_used,
 CONVERT(varchar(40),migs.last_user_seek,126) AS last_seek,
 CONVERT(varchar(40),migs.last_user_scan,126) AS last_scan,
  migs.group_handle
FROM sys.dm_db_missing_index_details mid
JOIN sys.dm_db_missing_index_groups mig ON mig.index_handle = mid.index_handle
JOIN sys.dm_db_missing_index_group_stats migs ON migs.group_handle = mig.index_group_handle
JOIN sys.tables t ON t.object_id = mid.object_id
JOIN sys.schemas s ON s.schema_id = t.schema_id
WHERE mid.database_id = DB_ID() AND (migs.user_seeks > 0 OR migs.user_scans > 0)
OPTION(MAXDOP 1)
"#,
        last_used = utc_local_iso("(SELECT MAX(v) FROM (VALUES (migs.last_user_seek), (migs.last_user_scan)) AS x(v))")
    )
}

const MISSING_INDEX_QUERIES_SQL: &str = r#"
SELECT DISTINCT q.group_handle, CONVERT(varchar(20), q.query_hash, 1) AS query_hash
FROM sys.dm_db_missing_index_group_stats_query q
JOIN sys.dm_db_missing_index_groups mig ON mig.index_group_handle=q.group_handle
JOIN sys.dm_db_missing_index_details mid ON mid.index_handle=mig.index_handle
WHERE mid.database_id=DB_ID()
OPTION(MAXDOP 1)
"#;

/// The DMV returns delimited identifiers. The shared lexer owns bracket escaping and quote boundaries.
fn bracket_list(value: Option<String>) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let mut remaining = value.trim();
    let mut names = Vec::new();
    loop {
        if !remaining.starts_with('[') {
            return Err(invalid_data());
        }
        let end = culprit_core::sql::quoted_end(remaining.as_bytes(), 0, b']', false).map_err(|_| invalid_data())?;
        let name = culprit_core::sql::unquote_identifier(&remaining[..end]);
        if name.is_empty() {
            return Err(invalid_data());
        }
        names.push(name);
        remaining = remaining[end..].trim_start();
        if remaining.is_empty() {
            return Ok(names);
        }
        remaining = remaining.strip_prefix(',').ok_or_else(invalid_data)?.trim_start();
    }
}

const APPLICATION_RECOMMENDATIONS: usize = 10_000;
pub(crate) struct NativeRecommendation {
    pub sample: MssqlMissingIndexSnapshot,
    display_last_used: Option<String>,
}
fn decode(row: &Rec) -> Result<NativeRecommendation> {
    let narrow = |field| integer(row, field)?.try_into().map_err(|_| invalid_data());
    let sample = MssqlMissingIndexSnapshot {
        table: MssqlObjectIdentity {
            database_id: narrow("database_id")?,
            object_id: narrow("object_id")?,
            created_without_offset: text(row, "created_without_offset")?,
        },
        schema: text(row, "schema_name")?,
        table_name: text(row, "table_name")?,
        index_handle: narrow("index_handle")?,
        group_handle: narrow("group_handle")?,
        equality: bracket_list(optional_text(row, "equality_columns")?)?,
        inequality: bracket_list(optional_text(row, "inequality_columns")?)?,
        include: bracket_list(optional_text(row, "included_columns")?)?,
        seeks: optional_counter(row, "user_seeks")?.ok_or_else(invalid_data)?,
        scans: optional_counter(row, "user_scans")?.ok_or_else(invalid_data)?,
        average_query_cost: required_nonnegative_number(row, "avg_total_user_cost")?,
        estimated_impact_percent: required_nonnegative_number(row, "avg_user_impact")?,
        last_seek_without_offset: optional_text(row, "last_seek")?,
        last_scan_without_offset: optional_text(row, "last_scan")?,
        reset: culprit_core::ResetMarker::Unknown,
        query_hashes: None,
    };
    if !sample.is_valid() {
        return Err(invalid_data());
    }
    Ok(NativeRecommendation { sample, display_last_used: optional_text(row, "last_used")? })
}
pub(crate) struct NativeRecommendations {
    pub indexes: Vec<NativeRecommendation>,
    pub query_link_failure: Option<Error>,
}
pub(crate) async fn read(reader: &mut impl NativeRead, limit: usize) -> Result<NativeRecommendations> {
    let rows = reader
        .query(&missing_indexes_sql(), vec![Parameter::Integer((limit + 1) as i64)], limit + 1)
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut indexes = Vec::with_capacity(rows.len());
    let mut groups = std::collections::BTreeMap::new();
    for row in &rows {
        let index = decode(row)?;
        if groups.insert(index.sample.group_handle, indexes.len()).is_some() {
            return Err(invalid_data());
        }
        indexes.push(index);
    }
    if indexes.is_empty() {
        return Ok(NativeRecommendations { indexes, query_link_failure: None });
    }
    let links = match query(reader, MISSING_INDEX_QUERIES_SQL).await {
        Ok(links) => links,
        Err(error) if crate::read::stops_remaining_reads(&error) => return Err(error),
        Err(error) => return Ok(NativeRecommendations { indexes, query_link_failure: Some(error) }),
    };
    let mut hashes = vec![Vec::new(); indexes.len()];
    for row in links {
        let group: u32 = integer(&row, "group_handle")?.try_into().map_err(|_| invalid_data())?;
        let hash = text(&row, "query_hash")?;
        if !hash
            .strip_prefix("0x")
            .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(invalid_data());
        }
        if let Some(index) = groups.get(&group) {
            hashes[*index].push(hash);
        }
    }
    for (index, mut hashes) in indexes.iter_mut().zip(hashes) {
        hashes.sort();
        hashes.dedup();
        index.sample.query_hashes = Some(hashes);
    }
    Ok(NativeRecommendations { indexes, query_link_failure: None })
}
pub(crate) struct Recommendations {
    pub indexes: Vec<MissingIndex>,
    pub query_link_failure: Option<Error>,
}
pub(crate) async fn application(reader: &mut impl NativeRead) -> Result<Recommendations> {
    let read = read(reader, APPLICATION_RECOMMENDATIONS).await?;
    let indexes = read
        .indexes
        .into_iter()
        .map(|native| {
            let sample = native.sample;
            let display = |counter: &str| counter.parse::<u64>().map(|value| value as f64).map_err(|_| invalid_data());
            Ok(MissingIndex {
                seeks: display(&sample.seeks)?,
                scans: display(&sample.scans)?,
                schema: sample.schema,
                table: sample.table_name,
                equality: sample.equality,
                inequality: sample.inequality,
                include: sample.include,
                avg_query_cost: sample.average_query_cost,
                avg_impact_pct: sample.estimated_impact_percent,
                last_used: native.display_last_used,
                query_hashes: sample.query_hashes.unwrap_or_default(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Recommendations { indexes, query_link_failure: read.query_link_failure })
}
#[cfg(test)]
pub(crate) mod tests;
