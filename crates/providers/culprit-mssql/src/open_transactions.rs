//! One bounded oldest-holder statistic, projected by app and export without streaming every transient transaction.
use crate::{
    read::{
        NativeRead, Parameter, ReadError, ReadSession, boolean, invalid_data, optional_boolean, optional_counter,
        optional_text, text,
    },
    sql_time::native_time,
    value::Rec,
};
use culprit_core::{Coverage, OpenTransaction, Result, TransactionActivity, telemetry::*};

// Logged work is established by a positive native log-record count, not by a non-NULL numeric LSN sentinel.
const OLDEST: &str = r#"WITH candidates AS (SELECT
 CASE WHEN dt.database_transaction_log_record_count>0 OR snap.is_snapshot=1 THEN 1 ELSE 0 END AS confirmed,
 ROW_NUMBER() OVER(PARTITION BY CASE WHEN dt.database_transaction_log_record_count>0 OR snap.is_snapshot=1 THEN 1 ELSE 0 END
 ORDER BY at.transaction_begin_time,at.transaction_id,s.session_id) AS age_order, CAST(s.session_id AS varchar(10)) AS session_id,
 CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS session_generation,
 CAST(at.transaction_id AS varchar(20)) AS transaction_id,
 CONVERT(varchar(40),CAST(at.transaction_begin_time AS datetime2),126) AS transaction_generation,
 CONVERT(varchar(40),CAST(GETDATE() AS datetime2),126) AS native_observed_at,
 s.program_name,s.status,dt.database_transaction_log_record_count AS log_record_count,
 dt.database_transaction_log_bytes_used AS log_bytes_used,
 CONVERT(varchar(25),dt.database_transaction_begin_lsn) AS begin_lsn,
 CASE WHEN snap.transaction_id IS NULL THEN 0 ELSE 1 END AS row_version_participant,
 snap.is_snapshot AS snapshot_isolation,snap.transaction_sequence_num AS transaction_sequence_number,
 snap.elapsed_time_seconds AS versioning_age_seconds
 FROM sys.dm_tran_database_transactions dt
 JOIN sys.dm_tran_session_transactions st ON st.transaction_id=dt.transaction_id
 JOIN sys.dm_tran_active_transactions at ON at.transaction_id=dt.transaction_id
 JOIN sys.dm_exec_sessions s ON s.session_id=st.session_id
 LEFT JOIN sys.dm_tran_active_snapshot_database_transactions snap ON snap.transaction_id=dt.transaction_id
 WHERE dt.database_id=DB_ID() AND s.is_user_process=1 AND s.session_id<>@@SPID
 AND (@P1=0 OR ISNULL(s.program_name,N'')<>N'culprit')
 AND (dt.database_transaction_log_record_count>0 OR snap.transaction_id IS NOT NULL)
 ) SELECT * FROM candidates WHERE age_order=1 ORDER BY confirmed DESC OPTION(MAXDOP 1)"#;

#[derive(Clone, Copy)]
pub(crate) enum Selection {
    Application,
    AllOtherUserSessions,
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R, selection: Selection) -> Result<MssqlCleanupHorizon> {
    let parameters = vec![Parameter::Integer(i64::from(matches!(selection, Selection::Application)))];
    let rows = reader.query(OLDEST, parameters, 3).await.map_err(ReadError::into_error)?;
    let mut horizon = MssqlCleanupHorizon { oldest_confirmed: None, oldest_unattributed_versioning: None };
    if rows.len() > 2 {
        return Err(invalid_data());
    }
    for row in rows {
        let holder = transaction(&row)?;
        let selected = if boolean(&row, "confirmed")? {
            &mut horizon.oldest_confirmed
        } else {
            &mut horizon.oldest_unattributed_versioning
        };
        if selected.replace(Box::new(holder)).is_some() {
            return Err(invalid_data());
        }
    }
    if !horizon.is_valid() {
        return Err(invalid_data());
    }
    Ok(horizon)
}
fn transaction(row: &Rec) -> Result<MssqlCleanupTransaction> {
    let begin = native_time(&text(row, "transaction_generation")?)?;
    let now = native_time(&text(row, "native_observed_at")?)?;
    let parse =
        |value: &str| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").map_err(|_| invalid_data());
    let elapsed = parse(&now)? - parse(&begin)?;
    Ok(MssqlCleanupTransaction {
        session: NativeActivityIdentity {
            id: text(row, "session_id")?,
            generation: native_time(&text(row, "session_generation")?)?,
        },
        transaction: NativeActivityIdentity { id: text(row, "transaction_id")?, generation: begin },
        observed_time_without_offset: now,
        age_seconds: (elapsed >= chrono::Duration::zero()).then(|| elapsed.num_seconds().to_string()),
        program: optional_text(row, "program_name")?,
        native_status: optional_text(row, "status")?,
        log_record_count: optional_counter(row, "log_record_count")?,
        log_bytes_used: optional_counter(row, "log_bytes_used")?,
        begin_lsn: optional_text(row, "begin_lsn")?,
        row_version_participant: boolean(row, "row_version_participant")?,
        snapshot_isolation: optional_boolean(row, "snapshot_isolation")?,
        transaction_sequence_number: optional_counter(row, "transaction_sequence_number")?,
        versioning_age_seconds: optional_counter(row, "versioning_age_seconds")?,
    })
}
pub(crate) fn observation(horizon: MssqlCleanupHorizon) -> OperationalObservation {
    OperationalObservation {
        source: crate::sources::OPEN_TRANSACTIONS.name.into(),
        key: "oldest-cleanup-transaction".into(),
        fact: OperationalFact::MssqlCleanupHorizon(horizon),
    }
}
fn application(horizon: &MssqlCleanupHorizon, version_store_bytes: Option<f64>) -> Option<OpenTransaction> {
    let row = horizon.oldest_confirmed.as_ref()?;
    Some(OpenTransaction {
        catalog_only: false,
        session: row.session.id.clone(),
        seconds: row.age_seconds.as_ref()?.parse::<u64>().ok()? as f64,
        program: row.program.clone().filter(|value| !value.trim().is_empty()),
        activity: match (row.native_status.as_deref(), row.snapshot_isolation == Some(true)) {
            (Some("sleeping"), _) => TransactionActivity::IdleInTransaction,
            (_, true) => TransactionActivity::Snapshot,
            _ => TransactionActivity::Running,
        },
        state: None,
        // This is explicitly this database's version-store context, not bytes attributable to the transaction.
        held_bytes: version_store_bytes.filter(|_| row.snapshot_isolation == Some(true)),
        holder: culprit_core::CleanupHolder::Session,
        xid_age: None,
        wrote: row.log_record_count.as_ref().and_then(|value| value.parse::<u64>().ok()).map(|count| count > 0),
        database: None,
    })
}
pub(crate) async fn collect(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &crate::probe::Probe,
    coverage: &mut Coverage,
    version_store_bytes: Option<f64>,
) -> Option<OpenTransaction> {
    let mut reader = ReadSession::new(pool, CollectionBudget::default());
    let result = read(&mut reader, Selection::Application).await;
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
    record_application_result(coverage, result, version_store_bytes)
}
fn record_application_result(
    coverage: &mut Coverage,
    result: Result<MssqlCleanupHorizon>,
    version_store_bytes: Option<f64>,
) -> Option<OpenTransaction> {
    let horizon = coverage.read(&crate::sources::OPEN_TRANSACTIONS, result)?;
    coverage.completed(&crate::sources::OPEN_TRANSACTIONS, usize::from(horizon.oldest_confirmed.is_some()));
    let value = application(&horizon, version_store_bytes);
    if horizon.oldest_unattributed_versioning.is_some() {
        coverage.partial(
            &crate::sources::OPEN_TRANSACTIONS,
            culprit_core::message!("provider.mssql.transaction.versioning_scope_unknown"),
        );
    }
    if horizon.oldest_confirmed.is_some() && value.is_none() {
        coverage.partial(
            &crate::sources::OPEN_TRANSACTIONS,
            culprit_core::message!("provider.mssql.transaction.age_unavailable"),
        );
    }
    value
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
pub(crate) mod tests;
