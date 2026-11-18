use super::*;
use crate::telemetry::hyperscale_log_components::{
    HyperscaleLogComponents,
    tests::{layout_row, native_row},
};
fn source(final_layout: Rec) -> StreamedSnapshot<HyperscaleLogComponents, StreamingRead> {
    catalog_source_with_native(
        HyperscaleLogComponents,
        native_row(),
        true,
        true,
        crate::telemetry::hyperscale_log_components::bounded_budget(&CollectionBudget::upper_limits()),
        CatalogReplies { layout: vec![Ok(vec![layout_row()])], final_proof: vec![Ok(vec![final_layout])] },
    )
}
async fn drain(source: &mut StreamedSnapshot<HyperscaleLogComponents, StreamingRead>) -> Vec<TelemetryRecord> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    rows
}
#[tokio::test]
async fn one_native_stream_preserves_exact_components_and_full_eight_query_roster_before_quiet_close() {
    let mut source = source(layout_row());
    let initial = source.describe().await.unwrap();
    assert_eq!(source.query_count(), 4);
    let rows = drain(&mut source).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(source.query_count(), 6);
    let completion = source.finish().await.unwrap();
    let completed = initial.completed(completion).unwrap().unwrap();
    assert_eq!(source.query_count(), crate::telemetry::hyperscale_log_components::STANDALONE_QUERY_COUNT);
    let TelemetryRecord::Operational(row) = &rows[0] else { panic!() };
    let OperationalFact::HyperscaleLogComponent(point) = &row.fact else { panic!() };
    assert_eq!(point.current_max_log_rate_bps, "9007199254740993");
    assert_eq!(point.catchup_rate_bps, "0");
    assert_eq!(point.catchup_distance_bytes, i64::MAX.to_string());
    assert!(point.replica_id.is_none());
    assert!(point.role_desc.is_none());
    assert!(point.last_reported_time_without_offset.is_none());
    let capture = CaptureIdentity { observation_id: "held-native-hs-log-components".into(), revision: None };
    assert!(ExportedRecord::new(&completed, &capture, rows[0].clone()).is_ok());
    let calls = &source.inner.reader().io.native.calls;
    assert_eq!(calls.iter().filter(|call| call.sql.contains("supported_columns")).count(), 2);
    assert_eq!(calls.iter().filter(|call| call.sql.contains("FROM sys.dm_hs_database_log_rate(")).count(), 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), crate::telemetry::hyperscale_log_components::STANDALONE_QUERY_COUNT);
}
#[tokio::test]
async fn final_schema_permission_and_profile_changes_withhold_success_without_native_followup() {
    for (field, value) in [
        ("database_id", Val::Int(6)),
        ("supported_columns", Val::Int(9)),
        ("performance_visibility", Val::Int(0)),
        ("database_edition", Val::Text("GeneralPurpose".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
    ] {
        let mut source = source(replace(&layout_row(), &[(field, value)]));
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(source.finish().await.is_err(), "{field}");
        assert_eq!(source.query_count(), 7, "final authority must not run after failed layout: {field}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

fn log_rate_request() -> CollectionRequest {
    let original = crate::activity::memory::focus::tests::original_focus();
    let mut actor = original.original;
    actor.wait = Some(ActivityWait {
        class: Some(ActivityWaitClass::LogRateGovernance),
        native_type: Some("LOG_RATE_GOVERNOR".into()),
        identified_resources: vec![],
        elapsed_ms: Some("1".into()),
        owners: vec![],
        resources: None,
    });
    let source = CollectionDescription {
        target: original.target,
        partition: original.partition,
        source: original.source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            database_writable: original.database_writable,
            epoch: Some(original.epoch),
            at: original.original_observed_at.clone(),
            observed_from: original.original_observed_at.clone(),
            observed_until: original.original_observed_at.clone(),
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            scope: culprit_core::MeasurementScope::Database,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::observed(original.source, 1, true),
        }),
    };
    let row = ExportedRecord::new(
        &source,
        &CaptureIdentity {
            observation_id: "controlled-native-log-rate-wait".into(),
            revision: Some(CaptureRevision {
                outbox_id: "controlled-log-rate-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        },
        TelemetryRecord::Activity(Box::new(actor.clone())),
    )
    .unwrap();
    let basis = DiagnosticEvidenceRef::from_export(&source, &digest(b"controlled original log-rate manifest"), &row, 0)
        .unwrap();
    let episode = DiagnosticEpisode::start(
        DiagnosticScope::from_source(&source).unwrap(),
        DiagnosticSignal::LogRateGovernance { key: actor.key, statement_identity: actor.statement_identity },
        original.original_observed_at,
        basis,
    )
    .unwrap();
    let focus = episode.source_read_focus().unwrap().unwrap();
    let execution = NativeSourceReadScope::from_context(&NativeReadContext::from_activity(&source).unwrap()).unwrap();
    assert!(focus.matches_basis(&source, &row, &execution));
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::SourceRead { focus: Box::new(focus), execution: Box::new(execution) },
        budget: crate::telemetry::hyperscale_log_components::bounded_budget(&CollectionBudget::upper_limits()),
    }
}
#[tokio::test]
async fn source_read_uses_the_same_native_stream_with_separate_logical_and_execution_scopes_and_ten_queries() {
    let request = log_rate_request();
    let before = replace(
        &crate::activity::memory::tests::context(),
        &[("observed_at", Val::Text("2020-01-01T08:01:00Z".into()))],
    );
    let after = replace(
        &crate::activity::memory::tests::context(),
        &[("observed_at", Val::Text("2020-01-01T08:01:01Z".into()))],
    );
    let mut native = FakeRead::new(vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![before]),
        Ok(vec![clock("2020-01-01T08:01:00Z")]),
        Ok(vec![pressure()]),
        Ok(vec![layout_row()]),
        Ok(vec![pressure()]),
        Ok(vec![layout_row()]),
        Ok(vec![clock("2020-01-01T08:01:01Z")]),
        Ok(vec![after]),
    ])
    .with_query_limit(10);
    native.shutdown = SourceShutdown::Quiescent;
    let io = StreamingRead { native, batches: vec![batch(native_row(), true)].into(), starts: 0, pause_batches: false };
    let mut source =
        StreamedSnapshot::with_io(io, &request, "fixture.invalid".into(), 1433, HyperscaleLogComponents).unwrap();
    let initial = source.describe().await.unwrap();
    assert_eq!(source.query_count(), 5);
    assert_eq!(initial.partition, SourcePartition::Target);
    assert!(initial.observation.execution_context().unwrap().has_qualified_local_owner());
    let rows = drain(&mut source).await;
    assert_eq!(source.query_count(), 7);
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert_eq!(source.query_count(), 10);
    let observations = rows
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect();
    let read = HyperscaleLogComponentRead::from_source(&completed, observations, true).unwrap();
    assert_eq!(read.description.as_ref(), &completed);
    assert_eq!(read.context().unwrap().partition, SourcePartition::Target);
    assert!(read.context().unwrap().database_writable.is_none());
    assert!(read.description.observation.execution_context().unwrap().database_writable == Some(true));
    let calls = &source.inner.reader().io.native.calls;
    assert_eq!(calls.len(), 10);
    assert_eq!(calls.iter().filter(|call| call.sql.contains("supported_columns")).count(), 2);
    assert_eq!(calls.iter().filter(|call| call.sql.contains("FROM sys.dm_hs_database_log_rate(")).count(), 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[test]
fn only_native_log_rate_wait_types_are_classified_for_the_new_source_question() {
    for wait in [
        "LOG_RATE_GOVERNOR",
        "RBIO_RG_STORAGE",
        "RBIO_RG_REPLICA",
        "RBIO_RG_DESTAGE",
        "RBIO_RG_LOCALDESTAGE",
        "RBIO_RG_GEOREPLICA",
        "RBIO_RG_STORAGE_CHECKPOINT",
        "RBIO_RG_MIGRATION_TARGET",
    ] {
        assert_eq!(crate::waits::activity_wait_class(wait), Some(ActivityWaitClass::LogRateGovernance), "{wait}");
    }
    for wait in ["WRITELOG", "PAGEIOLATCH_SH", "RBIO_RG_UNKNOWN", "LOG_RATE_GOVERNOR_FOREIGN"] {
        assert_eq!(crate::waits::activity_wait_class(wait), None, "{wait}");
    }
}
