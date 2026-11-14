//! Cheap aggregated tempdb version-store space, attributable to this database, shared by app and export.
use crate::read::{NativeRead, ReadError, integer, invalid_data, optional_integer};
use culprit_core::{
    Coverage, MeasurementScope, Result,
    telemetry::{
        CleanupSnapshot, CollectionBudget, CollectionStop, CounterNature, CounterUnit, NativeCounterSample,
        OperationalFact, OperationalObservation, SourceShutdown,
    },
};

pub(crate) mod persistent;

const SQL: &str = r#"SELECT COUNT_BIG(*) AS native_rows, COUNT_BIG(reserved_space_kb) AS known_rows,
 SUM(reserved_space_kb) AS reserved_space_kb
 FROM sys.dm_tran_version_store_space_usage WHERE database_id=DB_ID() OPTION(MAXDOP 1)"#;

pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<NativeCounterSample> {
    let rows = reader.query(SQL, vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let total = integer(row, "native_rows")?;
    if integer(row, "known_rows")? != total {
        return Err(invalid_data());
    }
    let value = match (total, optional_integer(row, "reserved_space_kb")?) {
        (0, None) => 0,
        (1.., Some(value)) => value,
        _ => return Err(invalid_data()),
    };
    Ok(NativeCounterSample::gauge(
        "tempdb-version-store",
        value as u64,
        CounterUnit::Kibibytes,
        MeasurementScope::Database,
        "sys.dm_tran_version_store_space_usage",
        "reserved_space_kb",
    ))
}
pub(crate) fn observation(sample: NativeCounterSample) -> OperationalObservation {
    OperationalObservation {
        source: culprit_core::source_id::VERSION_STORE.into(),
        key: sample.key.clone(),
        fact: OperationalFact::Counter(sample),
    }
}
pub(crate) fn bytes(sample: &NativeCounterSample) -> Option<f64> {
    (sample.is_valid()
        && sample.unit == CounterUnit::Kibibytes
        && sample.nature == CounterNature::Gauge
        && sample.scope == MeasurementScope::Database)
        .then(|| sample.value.integer().map(|value| value as f64 * 1024.0))
        .flatten()
}
pub(crate) struct StoreContext {
    pub tempdb_bytes: Option<f64>,
    pub persistent: Option<CleanupSnapshot>,
    adr_enabled: Option<bool>,
}
impl StoreContext {
    pub fn tempdb_cleanup_context(&self) -> Option<f64> {
        // ADR moves row versions into the database. Tempdb allocation cannot measure that transaction's context.
        self.tempdb_bytes.filter(|_| self.adr_enabled == Some(false))
    }

    pub fn persistent_signal(&self) -> Option<culprit_core::Signal> {
        let OperationalFact::MssqlPersistentVersionStore(sample) =
            &self.persistent.as_ref()?.observations.first()?.fact
        else {
            return None;
        };
        persistent::signal(sample)
    }
}
pub(crate) async fn application(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &crate::probe::Probe,
    coverage: &mut Coverage,
) -> StoreContext {
    let mut reader = crate::read::ReadSession::new(pool, CollectionBudget::default());
    application_with(&mut reader, coverage, &probe.grant_hint()).await
}

async fn application_with<R: NativeRead>(reader: &mut R, coverage: &mut Coverage, grant_hint: &str) -> StoreContext {
    let tempdb = read(reader).await;
    let observed_from = chrono::Utc::now().to_rfc3339();
    let persistent = if let Err(error) = &tempdb
        && crate::read::stops_remaining_reads(error)
    {
        Err(error.clone())
    } else {
        persistent::read(reader).await
    };
    let observed_until = chrono::Utc::now().to_rfc3339();
    let terminal_error = persistent.as_ref().err().filter(|error| crate::read::stops_remaining_reads(error));
    let (tempdb, persistent) = if reader.close().await == SourceShutdown::Unconfirmed {
        (Err(CollectionStop::CancellationUnconfirmed.error()), Err(CollectionStop::CancellationUnconfirmed.error()))
    } else if let Some(error) = terminal_error {
        (Err(error.clone()), Err(error.clone()))
    } else {
        (tempdb, persistent)
    };
    let permission = |error: culprit_core::Error| {
        if error.code == culprit_core::ErrorCode::PermissionDenied { error.with_hint(grant_hint) } else { error }
    };
    let tempdb_bytes =
        coverage.read(&crate::sources::VERSION_STORE, tempdb.map_err(permission)).as_ref().and_then(bytes);
    let (persistent, adr_enabled) = match persistent {
        Ok(None) => {
            coverage.not_applicable(
                &crate::sources::PERSISTENT_VERSION_STORE,
                culprit_core::message!("provider.mssql.persistent_version_store.not_applicable"),
            );
            (None, Some(false))
        }
        result => {
            let sample = coverage.read(&crate::sources::PERSISTENT_VERSION_STORE, result.map_err(permission)).flatten();
            let adr_enabled = sample.as_ref().map(|_| true);
            let snapshot = sample.map(|sample| CleanupSnapshot {
                source: crate::sources::PERSISTENT_VERSION_STORE.name.into(),
                observed_from,
                observed_until,
                observations: vec![persistent::observation(sample)],
            });
            (snapshot, adr_enabled)
        }
    };
    StoreContext { tempdb_bytes, persistent, adr_enabled }
}

#[cfg(test)]
pub(crate) mod tests;
