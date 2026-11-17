//! The same native capability and decoder serve retained runtime fragments and scan plan summaries.
use crate::{
    read::{integer, invalid_data, optional_counter, optional_nonnegative_number},
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{NativeIdentity, PageServerReadStatistics, PageServerReadSummary},
};

pub(crate) use culprit_core::telemetry::PageServerReadAvailability as Availability;

// The exact system-view object and five fixed names bound catalog work; no application objects are enumerated.
macro_rules! availability {
    () => {
        "CASE WHEN CAST(SERVERPROPERTY('EngineEdition') AS int) IN(2,3,4,6,8,9,11,12) THEN 0 WHEN CAST(SERVERPROPERTY('EngineEdition') AS int)=5 THEN CASE WHEN DATABASEPROPERTYEX(DB_NAME(),'Edition') IS NULL THEN 2 WHEN CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),'Edition'))<>N'Hyperscale' THEN 0 WHEN (SELECT COUNT_BIG(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name IN(N'avg_page_server_io_reads',N'last_page_server_io_reads',N'min_page_server_io_reads',N'max_page_server_io_reads',N'stdev_page_server_io_reads'))=5 THEN 1 ELSE 3 END ELSE 2 END"
    };
}
pub(crate) use availability;

const CURRENT_HYPERSCALE: &str = "CAST(SERVERPROPERTY('EngineEdition') AS int)=5 AND CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),'Edition'))=N'Hyperscale'";
const FIELDS: [(&str, &str); 5] = [
    ("avg_page_server_io_reads", "float"),
    ("last_page_server_io_reads", "bigint"),
    ("min_page_server_io_reads", "bigint"),
    ("max_page_server_io_reads", "bigint"),
    ("stdev_page_server_io_reads", "float"),
];

pub(crate) fn decode_availability(row: &Rec) -> Result<Availability> {
    match integer(row, "page_server_read_availability")? {
        0 => Ok(Availability::NotApplicable),
        1 => Ok(Availability::Available),
        2 => Ok(Availability::Unknown),
        3 => Ok(Availability::Unavailable),
        _ => Err(invalid_data()),
    }
}

pub(crate) fn columns(availability: Availability, alias: &str) -> String {
    let columns = FIELDS.map(|(name, kind)| match availability {
        Availability::Available => {
            format!("CASE WHEN {CURRENT_HYPERSCALE} THEN {alias}.{name} ELSE CAST(NULL AS {kind}) END AS {name}")
        }
        _ => format!("CAST(NULL AS {kind}) AS {name}"),
    });
    format!("{} AS page_server_read_availability,{}", current_availability(availability), columns.join(","))
}

fn current_availability(availability: Availability) -> String {
    match availability {
        Availability::Available => format!("CASE WHEN {CURRENT_HYPERSCALE} THEN 1 ELSE 2 END"),
        Availability::NotApplicable => "0".into(),
        Availability::Unknown => "2".into(),
        Availability::Unavailable => "3".into(),
    }
}

pub(crate) fn summary_columns(availability: Availability) -> String {
    let statistics = if availability == Availability::Available {
        let complete =
            |name: &str| format!("COUNT_BIG(CASE WHEN rs.count_executions>0 AND rs.{name} IS NULL THEN 1 END)=0");
        format!(
            "CASE WHEN {guard} THEN CASE WHEN COUNT_BIG(*)=1 THEN MAX(rs.avg_page_server_io_reads) WHEN {average_complete} THEN SUM(CASE WHEN rs.count_executions>0 THEN rs.avg_page_server_io_reads*rs.count_executions ELSE 0 END)/NULLIF(SUM(rs.count_executions),0) END END AS avg_page_server_io_reads,\n\
             CASE WHEN {guard} AND COUNT_BIG(*)=1 THEN MAX(rs.last_page_server_io_reads) END AS last_page_server_io_reads,\n\
             CASE WHEN {guard} THEN CASE WHEN COUNT_BIG(*)=1 THEN MAX(rs.min_page_server_io_reads) WHEN {minimum_complete} THEN MIN(CASE WHEN rs.count_executions>0 THEN rs.min_page_server_io_reads END) END END AS min_page_server_io_reads,\n\
             CASE WHEN {guard} THEN CASE WHEN COUNT_BIG(*)=1 THEN MAX(rs.max_page_server_io_reads) WHEN {maximum_complete} THEN MAX(CASE WHEN rs.count_executions>0 THEN rs.max_page_server_io_reads END) END END AS max_page_server_io_reads,\n\
             CASE WHEN {guard} AND COUNT_BIG(*)=1 THEN MAX(rs.stdev_page_server_io_reads) END AS stdev_page_server_io_reads",
            guard = format!("{CURRENT_HYPERSCALE} AND COUNT(DISTINCT rs.execution_type)=1"),
            average_complete = complete("avg_page_server_io_reads"),
            minimum_complete = complete("min_page_server_io_reads"),
            maximum_complete = complete("max_page_server_io_reads"),
        )
    } else {
        FIELDS.map(|(name, kind)| format!("CAST(NULL AS {kind}) AS {name}")).join(",")
    };
    format!(
        "{} AS page_server_read_availability,{statistics},\n\
         COUNT_BIG(*) AS page_server_native_fragments,\n\
         CASE WHEN COUNT_BIG(*)=1 THEN MIN(rs.runtime_stats_id) END AS page_server_native_row,\n\
         CASE WHEN COUNT_BIG(*)=1 THEN MIN(rs.runtime_stats_interval_id) END AS page_server_native_interval,\n\
         CASE WHEN COUNT(DISTINCT rs.execution_type)=1 THEN MIN(rs.execution_type) END AS page_server_native_outcome",
        current_availability(availability),
    )
}

pub(crate) fn decode(row: &Rec) -> Result<PageServerReadStatistics> {
    let statistics = PageServerReadStatistics {
        availability: decode_availability(row)?,
        average_pages: optional_nonnegative_number(row, "avg_page_server_io_reads")?,
        last_pages: optional_counter(row, "last_page_server_io_reads")?,
        minimum_pages: optional_counter(row, "min_page_server_io_reads")?,
        maximum_pages: optional_counter(row, "max_page_server_io_reads")?,
        standard_deviation_pages: optional_nonnegative_number(row, "stdev_page_server_io_reads")?,
    };
    statistics.is_valid().then_some(statistics).ok_or_else(invalid_data)
}

pub(crate) fn decode_summary(
    row: &Rec,
    plan_id: &str,
    replica_group_id: Option<&str>,
    executions: u64,
) -> Result<PageServerReadSummary> {
    let statistics = Box::new(decode(row)?);
    let native_fragments = integer(row, "page_server_native_fragments")?;
    let summary = match native_fragments {
        1 => {
            let id = integer(row, "page_server_native_row")?;
            let interval = integer(row, "page_server_native_interval")?;
            let outcome = integer(row, "page_server_native_outcome")?;
            if id < 0 || interval < 0 || !(0..=u8::MAX as i64).contains(&outcome) {
                return Err(invalid_data());
            }
            let mut native = NativeIdentity::new(id.to_string())
                .qualified("plan_id", plan_id)
                .qualified("runtime_stats_interval_id", interval.to_string())
                .qualified("execution_type", outcome.to_string());
            if let Some(group) = replica_group_id {
                native = native.qualified("replica_group_id", group);
            }
            PageServerReadSummary::NativeFragment {
                native: Some(native),
                executions: executions.to_string(),
                statistics,
            }
        }
        2.. => {
            if optional_counter(row, "page_server_native_row")?.is_some()
                || optional_counter(row, "page_server_native_interval")?.is_some()
            {
                return Err(invalid_data());
            }
            PageServerReadSummary::Derived {
                native_fragments: native_fragments.to_string(),
                executions: executions.to_string(),
                execution_type: optional_counter(row, "page_server_native_outcome")?,
                statistics,
            }
        }
        _ => return Err(invalid_data()),
    };
    summary.is_valid().then_some(summary).ok_or_else(invalid_data)
}

#[cfg(test)]
mod tests;
