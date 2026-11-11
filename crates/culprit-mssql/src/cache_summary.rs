//! A database-scoped cache aggregate. Cache lookups, memory footprint and query executions have distinct meanings.
use crate::read::{NativeRead, ReadError, ReadSession, integer, invalid_data, optional_integer};
use culprit_core::{Coverage, MeasurementScope, PlanCacheUse, Result, telemetry::*};

const SUMMARY: &str = r#"SELECT COUNT_BIG(*) AS total_plans,COUNT_BIG(cp.size_in_bytes) AS known_sizes,
 SUM(CAST(cp.size_in_bytes AS bigint)) AS total_bytes,
 SUM(CASE WHEN cp.usecounts=1 AND cp.objtype='Adhoc' THEN CAST(1 AS bigint) ELSE 0 END) AS adhoc_lookup_one_entries,
 SUM(CASE WHEN cp.usecounts=1 AND cp.objtype='Adhoc' THEN CAST(cp.size_in_bytes AS bigint) ELSE 0 END) AS adhoc_lookup_one_bytes,
 SUM(CASE WHEN cp.usecounts=1 AND cp.objtype='Prepared' THEN CAST(1 AS bigint) ELSE 0 END) AS prepared_lookup_one_entries,
 SUM(CASE WHEN cp.usecounts=1 AND cp.objtype='Prepared' THEN CAST(cp.size_in_bytes AS bigint) ELSE 0 END) AS prepared_lookup_one_bytes
 FROM sys.dm_exec_cached_plans cp CROSS APPLY sys.dm_exec_plan_attributes(cp.plan_handle) pa
 WHERE pa.attribute='dbid' AND CAST(pa.value AS int)=DB_ID() OPTION(MAXDOP 1)"#;

pub(crate) struct Summary {
    total_plans: u64,
    total_bytes: u64,
    adhoc_lookup_one_entries: u64,
    adhoc_lookup_one_bytes: u64,
    prepared_lookup_one_entries: u64,
    prepared_lookup_one_bytes: u64,
}
impl Summary {
    pub fn application(&self) -> PlanCacheUse {
        PlanCacheUse {
            total_plans: self.total_plans as f64,
            total_bytes: self.total_bytes as f64,
            // Parameterized reuse does not increment usecounts. Prepared entries must never drive this advice.
            single_use_plans: self.adhoc_lookup_one_entries as f64,
            single_use_bytes: self.adhoc_lookup_one_bytes as f64,
        }
    }
    pub fn observations(&self) -> Vec<OperationalObservation> {
        [
            ("plan-cache-total-entries", "total_plans", self.total_plans, CounterUnit::Count),
            ("plan-cache-total-bytes", "total_bytes", self.total_bytes, CounterUnit::Bytes),
            (
                "plan-cache-adhoc-lookup-one-entries",
                "adhoc_lookup_one_entries",
                self.adhoc_lookup_one_entries,
                CounterUnit::Count,
            ),
            (
                "plan-cache-adhoc-lookup-one-bytes",
                "adhoc_lookup_one_bytes",
                self.adhoc_lookup_one_bytes,
                CounterUnit::Bytes,
            ),
            (
                "plan-cache-prepared-lookup-one-entries",
                "prepared_lookup_one_entries",
                self.prepared_lookup_one_entries,
                CounterUnit::Count,
            ),
            (
                "plan-cache-prepared-lookup-one-bytes",
                "prepared_lookup_one_bytes",
                self.prepared_lookup_one_bytes,
                CounterUnit::Bytes,
            ),
        ]
        .into_iter()
        .map(|(key, name, value, unit)| OperationalObservation {
            source: crate::sources::PLAN_CACHE_USE.name.into(),
            key: key.into(),
            fact: OperationalFact::Counter(NativeCounterSample::gauge(
                key,
                value,
                unit,
                MeasurementScope::Database,
                "sys.dm_exec_cached_plans",
                name,
            )),
        })
        .collect()
    }
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<Summary> {
    let rows = reader.query(SUMMARY, vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let total = integer(row, "total_plans")?;
    if integer(row, "known_sizes")? != total {
        return Err(invalid_data());
    }
    let aggregate = |name| -> Result<u64> {
        match (total, optional_integer(row, name)?) {
            (0, None) => Ok(0),
            (1.., Some(value)) => Ok(value as u64),
            _ => Err(invalid_data()),
        }
    };
    let summary = Summary {
        total_plans: total as u64,
        total_bytes: aggregate("total_bytes")?,
        adhoc_lookup_one_entries: aggregate("adhoc_lookup_one_entries")?,
        adhoc_lookup_one_bytes: aggregate("adhoc_lookup_one_bytes")?,
        prepared_lookup_one_entries: aggregate("prepared_lookup_one_entries")?,
        prepared_lookup_one_bytes: aggregate("prepared_lookup_one_bytes")?,
    };
    if summary.adhoc_lookup_one_entries.saturating_add(summary.prepared_lookup_one_entries) > summary.total_plans
        || summary.adhoc_lookup_one_bytes.saturating_add(summary.prepared_lookup_one_bytes) > summary.total_bytes
    {
        return Err(invalid_data());
    }
    Ok(summary)
}
pub(crate) async fn collect(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &crate::probe::Probe,
    coverage: &mut Coverage,
) -> Option<PlanCacheUse> {
    let mut reader = ReadSession::new(pool, CollectionBudget::default());
    let result = read(&mut reader).await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    let result = result.map_err(|error| {
        if error.code == culprit_core::ErrorCode::PermissionDenied {
            error.with_hint(probe.grant_hint())
        } else {
            error
        }
    });
    coverage.read(&crate::sources::PLAN_CACHE_USE, result).as_ref().map(Summary::application)
}

#[cfg(test)]
pub(crate) mod tests;
