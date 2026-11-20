//! Real scan and collector acceptance share one guarded disposable XE configuration owner.
use super::super::*;
use crate::{
    MssqlProvider,
    deadlocks::system_health::tests::owned_fixture,
    instance,
    test_support::{receipt_directory, save_cost_receipt},
};
use culprit_core::{Environment, Provider};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;

#[derive(Clone, Copy)]
enum Case {
    Absolute,
    Relative,
}
impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Absolute => "configured-system-health-absolute",
            Self::Relative => "configured-system-health-relative",
        }
    }
}

struct Capture {
    observed: Result<(CollectionDescription, Vec<TelemetryRecord>, SourceCompletion)>,
    shutdown: SourceShutdown,
    metrics: Option<ReadWorkMetrics>,
    queries: u32,
    constructor_queries: u32,
    emitter_control: Option<&'static str>,
}

async fn capture(fixture: &owned_fixture::Fixture, case: Case) -> Result<Capture> {
    let admitted = &fixture.admitted;
    // These two native owner/time reads are fixture preparation, separate from the eight source dispatches.
    let mut preparation =
        ReadSession::new(admitted.pool.clone(), CollectionBudget { max_queries: 2, ..Default::default() });
    let prepared = instance::read(&mut preparation).await;
    let shutdown = preparation.close().await;
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let (owner, until) = prepared?;
    if !system_health::supported(&owner) {
        return Err(invalid_data());
    }
    let end = chrono::DateTime::parse_from_rfc3339(&until).map_err(|_| invalid_data())?;
    let requested = CollectionRequest {
        subject: CollectionSubject::Shared { resource: owner.identity },
        selection: CollectionSelection::DeadlockHistory {
            from: (end - chrono::Duration::minutes(1)).to_rfc3339(),
            until,
        },
        budget: shared_budget(&CollectionBudget::default()),
    };
    let provider = MssqlProvider {
        endpoint_host: "127.0.0.1".into(),
        endpoint_port: admitted.port,
        environment: Environment::Development,
        pool: admitted.pool.clone(),
    };
    let mut source = provider.telemetry_source(&requested)?;
    let constructor_queries = source.query_count();
    let mut emitter_control = None;
    let observed = AssertUnwindSafe(async {
        let description = source.describe().await?;
        let mut records = Vec::new();
        let mut cursor = None;
        loop {
            let CollectionRead::Page { records: page, next_cursor } =
                source.read_page(TelemetryStream::DeadlockEvents, cursor.as_deref()).await?
            else {
                return Err(invalid_data());
            };
            records.extend(page);
            cursor = next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        if matches!(case, Case::Absolute) {
            fixture.cycle_and_stop_emitter().await?;
            emitter_control = Some("stop-start-stop-acknowledged");
        }
        let completion = source.finish().await?;
        Ok((description, records, completion))
    })
    .catch_unwind()
    .await;
    // The same original reader settles before any result/metric assertion, receipt, restoration or later case.
    let shutdown = source.close().await;
    let observed = match observed {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    Ok(Capture {
        observed,
        shutdown,
        metrics: source.read_metrics(),
        queries: source.query_count(),
        constructor_queries,
        emitter_control,
    })
}

async fn run(case: Case) {
    let captured = owned_fixture::run(case.name(), matches!(case, Case::Absolute), move |fixture| {
        Box::pin(async move { capture(fixture, case).await })
    })
    .await
    .expect("owned source lifecycle and exact original XE restoration");
    let receipt = serde_json::json!({
        "case":case.name(),"source_queries":captured.queries,"factory_constructor_queries":captured.constructor_queries,"emitter_control":captured.emitter_control,
        "shutdown":match captured.shutdown {SourceShutdown::Quiescent=>"quiescent",SourceShutdown::Unconfirmed=>"unconfirmed"},
        "source_read_metrics":captured.metrics.as_ref(),
        "description":captured.observed.as_ref().ok().map(|observed|&observed.0),
        "records":captured.observed.as_ref().ok().map(|observed|&observed.1),
        "error":captured.observed.as_ref().err().map(|error|error.code),
        "fixture_native_owner_preparation_queries":2,"administrative_fixture_setup_in_source_metrics":false,
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false,"server_file_scan_work_bounded":false,
    });
    save_cost_receipt(&receipt_directory(), case.name(), &receipt)
        .expect("retain source outcome privately after settlement and restoration");
    assert_eq!(captured.shutdown, SourceShutdown::Quiescent);
    assert_eq!(captured.constructor_queries, 0, "the actual provider factory stays lazy");
    let metrics = captured.metrics.expect("original source work metrics");
    assert!(metrics.is_valid() && !metrics.has_unconfirmed_work());
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.untracked_tasks, 0);
    match case {
        Case::Absolute => {
            assert_eq!(captured.queries, SHARED_QUERIES);
            assert_eq!(captured.emitter_control, Some("stop-start-stop-acknowledged"));
            let (description, records, completion) = captured.observed.expect("native absolute configured file");
            assert_eq!(completion, SourceCompletion::Verified);
            assert!(matches!(description.target, CollectionTarget::Shared(_)));
            assert_eq!(description.continuity, SourceContinuity::Unverified);
            assert!(!description.observation.events().unwrap().history_complete);
            assert!(!records.is_empty(), "the owned deadlock must be retained in the configured file");
            let identity = CaptureIdentity { observation_id: case.name().into(), revision: None };
            for record in records {
                let exported = ExportedRecord::new(&description, &identity, record).unwrap();
                let TelemetryRecord::Deadlock(row) = exported.record else { panic!("native deadlock event") };
                assert!(row.is_valid());
                assert!(row.processes.iter().all(|process| process.login.is_none() && process.host.is_none()));
            }
        }
        Case::Relative => {
            assert_eq!(captured.queries, 3, "native owner plus configured metadata only; no file/target-data fallback");
            assert_eq!(captured.observed.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
        }
    }
}

#[tokio::test]
#[ignore = "requires nonce-owned disposable SQL Server and private native receipts; changes/restores only owned XE target"]
async fn an_owned_absolute_configured_target_preserves_native_deadlock_history_and_original_source_settlement() {
    run(Case::Absolute).await;
}

#[tokio::test]
#[ignore = "requires nonce-owned disposable SQL Server and private native receipts; changes/restores only owned XE target"]
async fn an_owned_relative_configured_target_is_unavailable_without_a_file_or_runtime_target_fallback() {
    run(Case::Relative).await;
}
