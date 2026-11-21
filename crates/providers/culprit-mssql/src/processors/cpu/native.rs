//! Explicit CPU details use the original native reader; no polling, external metrics or ring XML is retained.
use super::*;
use crate::read::{NativeRead, Parameter, ReadError, optional_text};
use std::collections::BTreeMap;
const MAX_RING_RECORD_BYTES: usize = 16 * 1024;

fn pool_sql() -> String {
    format!(
        r#"SELECT /* culprit */ {},i.cpu_count AS host_processors,
 ISNULL((SELECT TOP (@P1) p.pool_id,
 CONVERT(varchar(40),CAST(p.statistics_start_time AS datetime2),126) AS statistics_started_without_offset,
 CONVERT(varchar(40),p.total_cpu_usage_ms) AS total_cpu_usage_ms
 FROM sys.dm_resource_governor_resource_pools p ORDER BY p.pool_id
 FOR JSON PATH,INCLUDE_NULL_VALUES),N'[]') AS pool_cpu_rows
 FROM sys.dm_os_sys_info i OPTION(MAXDOP 1)"#,
        instance::point_columns(instance::PointClock::CurrentUtc)
    )
}
fn history_sql() -> String {
    // Microsoft documents these scheduler-monitor fields. It explicitly says Linux idle is always zero.
    // https://techcommunity.microsoft.com/blog/azuresqlblog/monitor-cpu-usage-on-sql-server-and-azure-sql/680777/
    // https://techcommunity.microsoft.com/blog/sqlserver/sql-server-cpu-usage-available-in-sys-dm-os-ring-buffers-dmv-starting-sql-server/825361
    format!(
        r#"SELECT /* culprit */ {},h.host_platform,b.[timestamp] AS record_ms_ticks,
 DATALENGTH(b.record) AS record_bytes,
 CASE WHEN DATALENGTH(b.record)<=@P2 THEN b.record END AS cpu_record
 FROM sys.dm_os_sys_info i CROSS JOIN sys.dm_os_host_info h
 OUTER APPLY(SELECT TOP (@P1) [timestamp],record FROM sys.dm_os_ring_buffers
 WHERE ring_buffer_type=N'RING_BUFFER_SCHEDULER_MONITOR' AND record LIKE N'%<SystemHealth>%'
 ORDER BY [timestamp] DESC) b OPTION(MAXDOP 1)"#,
        instance::point_columns(instance::PointClock::CurrentUtc)
    )
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PoolRow {
    pool_id: i32,
    statistics_started_without_offset: String,
    #[serde(deserialize_with = "required_nullable_counter")]
    total_cpu_usage_ms: Option<String>,
}
fn required_nullable_counter<'de, D: serde::Deserializer<'de>>(
    value: D,
) -> std::result::Result<Option<String>, D::Error> {
    serde::Deserialize::deserialize(value)
}
pub(crate) async fn read_pools(
    reader: &mut impl NativeRead,
    expected: &instance::Context,
    limit: usize,
) -> Result<Vec<NativeCounterSet>> {
    if limit < MAX_MSSQL_CPU_POOLS as usize {
        return Err(CollectionStop::RowLimit.error());
    }
    let rows = reader
        .query(&pool_sql(), vec![Parameter::Integer(i64::from(MAX_MSSQL_CPU_POOLS) + 1)], 2)
        .await
        .map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let (owner, properties) = point(row, expected)?;
    let host = crate::read::optional_counter(row, "host_processors")?.ok_or_else(invalid_data)?;
    if host == "0" {
        return Err(invalid_data());
    }
    let pools: Vec<PoolRow> = serde_json::from_str(&text(row, "pool_cpu_rows")?).map_err(|_| invalid_data())?;
    if pools.is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    if pools.len() > MAX_MSSQL_CPU_POOLS as usize {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut seen = std::collections::BTreeSet::new();
    pools
        .into_iter()
        .map(|pool| {
            if pool.pool_id < 0 || !seen.insert(pool.pool_id) {
                return Err(invalid_data());
            }
            let mut properties = properties.clone();
            properties.insert("host_logical_processors".into(), host.clone());
            properties.insert(
                "statistics_started_without_offset".into(),
                crate::sql_time::native_time(&pool.statistics_started_without_offset)?,
            );
            let mut sample =
                header(&owner, MSSQL_CPU_POOL_KIND, MSSQL_CPU_POOL_OBJECT, pool.pool_id.to_string(), properties)?;
            if let Some(value) = pool.total_cpu_usage_ms {
                sample.counters.push(NativeCounterSample {
                    key: "total_cpu_usage_ms".into(),
                    value: MeasurementValue::Integer(value),
                    unit: CounterUnit::Milliseconds,
                    nature: CounterNature::Cumulative,
                    scope: MeasurementScope::Server,
                    reset: ResetMarker::Unknown,
                    native_object: MSSQL_CPU_POOL_OBJECT.into(),
                    native_name: "total_cpu_usage_ms".into(),
                    native_instance: Some(pool.pool_id.to_string()),
                    native_type: None,
                });
            }
            MssqlCpuPoolPoint::from_sample(&sample)?;
            Ok(sample)
        })
        .collect()
}

pub(crate) async fn read_history(
    reader: &mut impl NativeRead,
    expected: &instance::Context,
    limit: usize,
) -> Result<Vec<NativeCounterSet>> {
    if expected.major < 14 {
        return Err(culprit_core::Error::new(
            culprit_core::ErrorCode::Unsupported,
            culprit_core::t!("collection.unsupported"),
        ));
    }
    if limit < 1 {
        return Err(CollectionStop::RowLimit.error());
    }
    let rows = reader
        .query(&history_sql(), vec![Parameter::Integer(2), Parameter::Integer(MAX_RING_RECORD_BYTES as i64)], 2)
        .await
        .map_err(ReadError::into_error)?;
    let Some(row) = rows.first() else { return Err(invalid_data()) };
    let (owner, mut properties) = point(row, expected)?;
    let record_ticks = crate::read::optional_counter(row, "record_ms_ticks")?;
    let Some(record_ticks) = record_ticks else {
        if rows.len() != 1
            || optional_text(row, "cpu_record")?.is_some()
            || crate::read::optional_counter(row, "record_bytes")?.is_some()
        {
            return Err(invalid_data());
        }
        return Ok(Vec::new());
    };
    if rows.len() > 2 {
        return Err(invalid_data());
    }
    for next in rows.iter().skip(1) {
        point(next, expected)?;
        let next_ticks = crate::read::optional_counter(next, "record_ms_ticks")?.ok_or_else(invalid_data)?;
        if next_ticks.parse::<u64>().map_err(|_| invalid_data())?
            >= record_ticks.parse::<u64>().map_err(|_| invalid_data())?
        {
            return Err(CollectionStop::SourceUnavailable.error());
        }
    }
    let bytes = usize::try_from(integer(row, "record_bytes")?).map_err(|_| invalid_data())?;
    if bytes == 0 || bytes > MAX_RING_RECORD_BYTES {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let record = text(row, "cpu_record")?;
    if record.encode_utf16().count().checked_mul(2) != Some(bytes) {
        return Err(invalid_data());
    }
    let parsed = roxmltree::Document::parse(&record).map_err(|_| invalid_data())?;
    let root = parsed.root_element();
    if root.tag_name().name() != "Record" {
        return Err(invalid_data());
    }
    let id = root.attribute("id").ok_or_else(invalid_data)?.parse::<i32>().map_err(|_| invalid_data())?;
    if id < 0 {
        return Err(invalid_data());
    }
    let event = one_child(root, "SchedulerMonitorEvent")?.ok_or_else(invalid_data)?;
    let health = one_child(event, "SystemHealth")?.ok_or_else(invalid_data)?;
    for (element, name) in [("ProcessUtilization", "process_utilization"), ("SystemIdle", "system_idle")] {
        if let Some(child) = one_child(health, element)? {
            if child.children().any(|node| node.is_element()) {
                return Err(invalid_data());
            }
            let raw = child.text().ok_or_else(invalid_data)?;
            let value = raw.parse::<i32>().map_err(|_| invalid_data())?;
            if value.to_string() != raw {
                return Err(invalid_data());
            }
            properties.insert(name.into(), raw.into());
        }
    }
    if let Some(platform) = optional_text(row, "host_platform")? {
        properties.insert("host_platform".into(), platform);
    }
    properties.insert("record_ms_ticks".into(), record_ticks);
    let sample = header(&owner, MSSQL_CPU_HISTORY_KIND, MSSQL_CPU_HISTORY_OBJECT, id.to_string(), properties)?;
    MssqlCpuHistoryPoint::from_sample(&sample)?;
    Ok(vec![sample])
}
fn one_child<'a, 'input>(node: roxmltree::Node<'a, 'input>, name: &str) -> Result<Option<roxmltree::Node<'a, 'input>>> {
    let mut children = node.children().filter(|child| child.is_element() && child.tag_name().name() == name);
    let first = children.next();
    if children.next().is_some() {
        return Err(invalid_data());
    }
    Ok(first)
}
fn point(row: &Rec, expected: &instance::Context) -> Result<(instance::Context, BTreeMap<String, String>)> {
    let owner = instance::observation(row)?.context_from_epoch(row)?.ok_or_else(invalid_data)?;
    if &owner != expected || ![2, 3, 4].contains(&owner.engine_edition()) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let properties = BTreeMap::from([
        ("observed_at".into(), canonical_time(&text(row, "observed_at")?)?),
        ("started_at_ticks".into(), integer(row, "started_at_ticks")?.to_string()),
        ("sample_ms_ticks".into(), integer(row, "ms_ticks")?.to_string()),
        ("engine_edition".into(), integer(row, "edition")?.to_string()),
        ("major".into(), text(row, "major")?),
    ]);
    Ok((owner, properties))
}
fn header(
    owner: &instance::Context,
    kind: &str,
    object: &str,
    id: String,
    properties: BTreeMap<String, String>,
) -> Result<NativeCounterSet> {
    Ok(NativeCounterSet {
        identity: NativeIdentity::new(id)
            .qualified("kind", kind)
            .qualified("native_object", object)
            .qualified("instance_id", owner.identity.telemetry_id()?)
            .qualified("instance_epoch", owner.capture_epoch()?),
        scope: MeasurementScope::Server,
        counters: Vec::new(),
        ratios: Vec::new(),
        ratio_gaps: Vec::new(),
        properties,
    })
}

#[cfg(test)]
pub(crate) mod tests;
