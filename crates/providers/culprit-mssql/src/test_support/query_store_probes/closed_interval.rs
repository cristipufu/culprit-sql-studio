//! One owned-only native interval wait. No source reader configures Query Store or polls for retention.
use super::*;
use crate::{
    read::{integer, signed_integer, text},
    value::Rec,
};
use std::time::Duration;
const MAX_INTERVAL_WAIT_MS: u64 = 60_000;
const CONFIGURE: &str = "ALTER DATABASE /* culprit */ CURRENT SET QUERY_STORE (INTERVAL_LENGTH_MINUTES=1)";
const FLUSH: &str = "EXEC /* culprit */ sys.sp_query_store_flush_db";
const WINDOW: &str = concat!(
    "SELECT TOP(2) /* culprit */ ",
    crate::query_store::catalog_sql::query_identity_columns!("q.query_id"),
    ",i.runtime_stats_interval_id AS id,CONVERT(varchar(40),i.start_time,127) AS starts,CONVERT(varchar(40),i.end_time,127) AS ends,",
    "DATEDIFF_BIG(millisecond,SYSDATETIMEOFFSET(),i.end_time) AS remaining_ms ",
    "FROM sys.query_store_query q CROSS APPLY(SELECT TOP(1) i.* FROM sys.query_store_runtime_stats_interval i ",
    "WHERE EXISTS(SELECT 1 FROM sys.query_store_runtime_stats r JOIN sys.query_store_plan p ON p.plan_id=r.plan_id ",
    "WHERE p.query_id=q.query_id AND r.runtime_stats_interval_id=i.runtime_stats_interval_id) ORDER BY i.end_time DESC,i.runtime_stats_interval_id DESC)i ",
    "WHERE q.query_id=@P1 OPTION(MAXDOP 1)"
);
struct Window {
    interval: TelemetryInterval,
    wait: Duration,
}
impl Window {
    fn decode(row: &Rec, original: &NativeQueryIdentity) -> Result<Self> {
        if decode::query_identity(row)? != *original {
            return Err(CollectionStop::SourceChanged.error());
        }
        let id = integer(row, "id")?;
        let interval = TelemetryInterval {
            native_id: id.to_string(),
            from: canonical_time(&text(row, "starts")?)?,
            until: canonical_time(&text(row, "ends")?)?,
        };
        let bounds = ObservationBounds::new(&interval.from, &interval.until).ok_or_else(invalid_data)?;
        let remaining = signed_integer(row, "remaining_ms")?;
        if id <= 0 || !bounds.has_positive_duration() || remaining > MAX_INTERVAL_WAIT_MS as i64 {
            return Err(invalid_data());
        }
        // A natively closed interval needs no wait. The actual source still enforces exact closed bounds later.
        let wait = Duration::from_millis(u64::try_from(remaining.max(0)).map_err(|_| invalid_data())?);
        Ok(Self { interval, wait })
    }
}

pub(crate) async fn seed_closed_interval(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    cancellation: &CollectionCancellation,
) -> Result<(NativeQueryIdentity, TelemetryInterval)> {
    let mut reader = ReadSession::new(
        fixture.pool.clone(),
        CollectionBudget { max_queries: 3, statement_timeout_ms: 1500, total_timeout_ms: 90_000, ..Default::default() },
    );
    let started = Instant::now();
    let outcome = AssertUnwindSafe(async {
        until_cancelled(cancellation, async {
            reader.query(CONFIGURE, vec![], 1).await.map_err(ReadError::into_error)
        })
        .await?;
        let originals =
            seed(fixture, directory, cancellation, 1, ProbePolicy::OwnedApplication, "query-history-owned-probe")
                .await?;
        if originals.len() != 1 {
            return Err(invalid_data());
        }
        let original = originals.into_values().next().ok_or_else(invalid_data)?;
        let rows = until_cancelled(cancellation, async {
            reader
                .query(WINDOW, vec![Parameter::Integer(original.query_id.parse().map_err(|_| invalid_data())?)], 2)
                .await
                .map_err(ReadError::into_error)
        })
        .await?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        let window = Window::decode(row, &original)?;
        until_cancelled(cancellation, async {
            tokio::time::sleep(window.wait).await;
            Ok(())
        })
        .await?;
        until_cancelled(cancellation, async { reader.query(FLUSH, vec![], 1).await.map_err(ReadError::into_error) })
            .await?;
        Ok((original, window.interval))
    })
    .catch_unwind()
    .await;
    let shutdown = reader.close().await;
    let written = save_cost_receipt(
        directory,
        "query-history-closed-window",
        &json!({
            "queries":reader.queries(),"read_metrics":reader.read_metrics(),"client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "shutdown":shutdown_state(shutdown),"panicked":outcome.is_err(),"cancelled":cancellation.is_cancelled(),
            "error":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "error_detail":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).and_then(|error|error.detail),
            "maximum_wait_ms":MAX_INTERVAL_WAIT_MS,"maximum_client_waits":1,"outside_source_measurement":true,
            "setup":"owned-only one-minute Query Store configuration; existing one-row application seed; one exact native query-incarnation/interval lookup; one cancellable wait and flush; no polling",
            "production_admission":"unqualified"
        }),
    );
    let result = match outcome {
        Ok(result) => result,
        Err(panic) => {
            let _ = written;
            std::panic::resume_unwind(panic)
        }
    };
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    match result {
        Ok(value) => {
            written?;
            Ok(value)
        }
        Err(error) => {
            let _ = written;
            Err(error)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::test_support::replace;
    use crate::value::Val;
    #[test]
    fn native_interval_wait_is_finite_exact_and_never_rebinds_a_recycled_query_number() {
        let runtime_summary = crate::query_store::tests::summary_row();
        let mut fields = runtime_summary.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
        fields.extend([
            ("id", Val::Int(17)),
            ("starts", Val::Text("2026-11-18T12:00:00.1234567Z".into())),
            ("ends", Val::Text("2026-11-18T12:01:00.1234567Z".into())),
            ("remaining_ms", Val::Int(59_999)),
        ]);
        let row = Rec::of(&fields);
        let original = decode::query_identity(&row).unwrap();
        let window = Window::decode(&row, &original).unwrap();
        assert_eq!(window.interval.native_id, "17");
        assert_eq!(window.wait, Duration::from_millis(59_999));
        assert_eq!(window.interval.until, "2026-11-18T12:01:00.123456700Z");
        assert_eq!(
            Window::decode(&replace(&row, &[("remaining_ms", Val::Int(-1))]), &original).unwrap().wait,
            Duration::ZERO
        );
        for changes in [
            vec![("remaining_ms", Val::Int(60_001))],
            vec![("remaining_ms", Val::Null)],
            vec![("ends", Val::Text("2026-11-18T12:00:00Z".into()))],
            vec![("query_initial_compile_at", Val::Text("2026-11-18T11:00:00Z".into()))],
        ] {
            assert!(Window::decode(&replace(&row, &changes), &original).is_err());
        }
        assert!(!WINDOW.contains("DISTINCT"));
        assert!(WINDOW.contains("EXISTS(SELECT 1 FROM sys.query_store_runtime_stats"));
    }
}
