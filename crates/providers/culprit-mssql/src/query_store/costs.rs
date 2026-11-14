//! Native Query Store cost projections. Compilation totals retain their own lifetime and never borrow a runtime window.
use crate::{
    read::{integer, invalid_data, signed_integer},
    value::Rec,
};
use culprit_core::{Result, telemetry::canonical_time};

pub(crate) const COMPILATION_COLUMNS: &str = concat!(
    super::catalog_sql::query_identity_columns!("q.query_id"),
    r#",
 q.count_compiles,q.avg_compile_duration,
 q.avg_compile_memory_kb,q.last_compile_memory_kb,q.max_compile_memory_kb,
 CONVERT(varchar(40),q.last_compile_start_time,127) AS last_compile,
 DATEDIFF_BIG(second,q.initial_compile_start_time,SYSDATETIMEOFFSET()) AS compiling_s"#
);

pub(crate) fn compilation_sql() -> String {
    format!(
        r#"DECLARE /* culprit */ @ids xml=CONVERT(xml,@P1);
SELECT /* culprit */ {COMPILATION_COLUMNS}
FROM sys.query_store_query q
WHERE q.query_id IN(SELECT n.x.value('@id[1]','bigint') FROM @ids.nodes('/ids/id') n(x))
OPTION(MAXDOP 1)"#
    )
}

/// Bounded export of retained entries, including Culprit's own compilation overhead.
pub(crate) fn compilation_snapshot_sql() -> String {
    format!(
        r#"SELECT /* culprit */ TOP (@P1) {COMPILATION_COLUMNS}
FROM sys.query_store_query q
OPTION(MAXDOP 1)"#
    )
}

pub(crate) struct Compilation {
    pub identity: culprit_core::telemetry::NativeQueryIdentity,
    pub count: u64,
    pub avg_duration_us: f64,
    pub memory: Option<culprit_core::CompilationMemory>,
    pub first_at: String,
    pub last_at: String,
    pub seconds_since_first: i64,
}
impl Compilation {
    pub fn decode(row: &Rec) -> Result<Self> {
        let identity = super::decode::query_identity(row)?;
        let memory = culprit_core::CompilationMemory {
            average_kb: crate::read::optional_nonnegative_number(row, "avg_compile_memory_kb")?,
            last_kb: crate::read::optional_nonnegative_number(row, "last_compile_memory_kb")?,
            maximum_kb: crate::read::optional_nonnegative_number(row, "max_compile_memory_kb")?,
        };
        Ok(Self {
            first_at: identity.qualifiers.get("initial_compile_at").cloned().ok_or_else(invalid_data)?,
            identity,
            count: integer(row, "count_compiles")?.try_into().map_err(|_| invalid_data())?,
            avg_duration_us: crate::read::required_nonnegative_number(row, "avg_compile_duration")?,
            memory: memory.is_measured().then_some(memory),
            last_at: canonical_time(&crate::read::text(row, "last_compile")?)?,
            seconds_since_first: signed_integer(row, "compiling_s")?,
        })
    }
    pub fn observation(&self) -> culprit_core::telemetry::MssqlQueryCompilation {
        culprit_core::telemetry::MssqlQueryCompilation {
            query_id: self.identity.query_id.clone(),
            first_compile_at: self.first_at.clone(),
            last_compile_at: self.last_at.clone(),
            compilations: self.count.to_string(),
            average_duration_us: self.avg_duration_us,
            memory: self.memory,
            seconds_since_first_compile: self.seconds_since_first,
            reset: culprit_core::ResetMarker::Unknown,
        }
    }
    pub fn within_window(&self, minutes: u32) -> bool {
        self.count > 0 && self.seconds_since_first >= 0 && self.seconds_since_first <= i64::from(minutes) * 60
    }
    pub fn history(&self, window_minutes: u32) -> culprit_core::signals::CompileHistory {
        culprit_core::signals::CompileHistory {
            count: self.count as f64,
            total_ms: self.total_ms(),
            first_at: Some(self.first_at.clone()),
            last_at: Some(self.last_at.clone()),
            within_window: self.within_window(window_minutes),
            memory: self.memory,
        }
    }
    pub fn total_ms(&self) -> f64 {
        super::runtime::milliseconds(self.avg_duration_us * self.count as f64)
    }
}

pub(crate) fn database_compilation_sql() -> String {
    format!(
        r#"SELECT /* culprit */ SUM(CAST(q.count_compiles AS float)) AS compiles,
 SUM(q.avg_compile_duration*q.count_compiles)/1000.0 AS compile_ms,
 CONVERT(varchar(40),MIN(q.initial_compile_start_time),127) AS first_compile,
 CONVERT(varchar(40),MAX(q.last_compile_start_time),127) AS last_compile
FROM sys.query_store_query q
LEFT JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id
WHERE q.is_internal_query=0 AND q.is_clouddb_internal_query=0 AND {workload} OPTION(MAXDOP 1)"#,
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text")
    )
}
