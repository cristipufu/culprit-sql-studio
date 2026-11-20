use super::super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    read::{Parameter, ReadResult},
    resources::test_support::{FakeRead, instance_pressure_row, replace},
    value::{Rec, Val},
};
use culprit_core::{Error, ErrorCode, MeasurementScope, SourceState};

struct Reader {
    inner: FakeRead,
    generation: Option<u64>,
    change_at: Option<u32>,
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation.map(|generation| generation + u64::from(self.change_at.is_some_and(|at| self.queries() >= at)))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings },
        budget: CollectionBudget::default(),
    }
}
fn authority(at: &str) -> Rec {
    replace(&crate::activity::database_context::tests::row(3), &[("observed_at", Val::Text(at.into()))])
}
fn control() -> Rec {
    Rec::of(&[
        ("culprit_settings_frame", Val::Int(1)),
        ("culprit_settings_database_id", Val::Int(7)),
        ("culprit_settings_database_name", Val::Text("shop".into())),
        ("culprit_settings_updateability", Val::Text("READ_WRITE".into())),
    ])
}
fn framed_with(row: Rec, control: Rec) -> Rec {
    let columns = control.columns().chain(row.columns()).map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    Rec::of(&columns)
}
fn framed(row: Rec) -> Rec {
    framed_with(row, control())
}

fn properties() -> Rec {
    replace(
        &crate::settings::tests::database(),
        &[
            ("database_id", Val::Int(7)),
            ("selected_database_id", Val::Int(7)),
            ("database_name", Val::Text("shop".into())),
            ("selected_database", Val::Text("shop".into())),
        ],
    )
}
fn server() -> Rec {
    Rec::of(&[
        ("name", Val::Text("max degree of parallelism".into())),
        ("value", Val::Text("8".into())),
        ("value_in_use", Val::Text("0".into())),
    ])
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    let mut rows = vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(3))])]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:00Z")]),
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:00:00Z")]),
    ];
    for row in [properties(), crate::settings::tests::scoped(), server(), crate::settings::tests::options()] {
        rows.push(Ok(vec![instance_pressure_row()]));
        rows.push(Ok(vec![framed(row)]));
    }
    rows.extend([
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:02:01Z")]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority("2020-01-01T06:02:02Z")]),
    ]);
    rows
}
fn settings_source(rows: Vec<ReadResult<Vec<Rec>>>, request: &CollectionRequest) -> StateSource<Reader> {
    StateSource::with_io(
        Reader {
            inner: FakeRead::new(rows).with_query_limit(settings_budget(&request.budget).max_queries),
            generation: Some(1),
            change_at: None,
        },
        request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap()
}
async fn drain(source: &mut StateSource<Reader>) -> Vec<TelemetryRecord> {
    let mut rows = vec![];
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::SettingObservations, cursor.as_deref()).await.unwrap()
        else {
            panic!("original Settings pages")
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            return rows;
        }
    }
}

#[tokio::test]
async fn boxed_settings_reuse_four_native_families_with_local_authority_and_exact_distinct_scopes() {
    let mut source = settings_source(replies(), &request());
    let description = source.describe().await.unwrap();
    let context = NativeReadContext::from_snapshot(&description).unwrap();
    assert!(context.has_qualified_local_owner());
    assert!(description.target.database().unwrap().incarnation.is_none());
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.sources.len(), 4);
    assert!(snapshot.sources.iter().all(|source| source.state == SourceState::Collected));
    let rows = drain(&mut source).await;
    let mut baseline = FakeRead::new(
        [properties(), crate::settings::tests::scoped(), server(), crate::settings::tests::options()]
            .into_iter()
            .map(|row| Ok(vec![row]))
            .collect(),
    );
    let mut expected = Vec::new();
    for kind in crate::settings::Kind::ALL {
        expected.extend(
            crate::settings::read(&mut baseline, kind, 512).await.unwrap().into_iter().map(TelemetryRecord::Setting),
        );
    }
    assert_eq!(rows, expected, "local qualification must not alter the canonical native decoder values");
    assert_eq!(snapshot.records, rows.len() as u32);
    let values = rows
        .iter()
        .map(|row| match row {
            TelemetryRecord::Setting(value) => value,
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    let server = values.iter().find(|value| value.namespace == "sys.configurations").unwrap();
    assert_eq!(server.scope, MeasurementScope::Server);
    assert_eq!(server.value.as_deref(), Some("8"));
    assert_eq!(server.value_in_use.as_deref(), Some("0"));
    let scoped = values.iter().find(|value| value.namespace == "sys.database_scoped_configurations").unwrap();
    assert_eq!(scoped.scope, MeasurementScope::Database);
    assert_eq!(scoped.value.as_deref(), Some("9007199254740993"));
    assert_eq!(scoped.value_for_secondary, None);
    assert!(
        values
            .iter()
            .all(|value| !value.name.contains("culprit_settings_frame") && !value.name.contains("database_id"))
    );
    let capture = CaptureIdentity { observation_id: "local-settings".into(), revision: None };
    for row in rows {
        let original = ExportedRecord::new(&description, &capture, row).unwrap();
        assert_eq!(
            serde_json::from_str::<ExportedRecord>(&serde_json::to_string(&original).unwrap()).unwrap(),
            original
        );
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 17);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.io.inner.closed, 1);
    assert_eq!(source.io.inner.calls.iter().filter(|call| call.sql.contains("AS culprit_settings_frame")).count(), 4);
    assert!(source.io.inner.calls.iter().all(|call| !call.sql.contains("dm_user_db_resource_governance")));
}

#[tokio::test]
async fn changed_frame_database_name_id_or_role_stops_siblings_even_when_final_authority_would_match() {
    for (field, value) in [
        ("culprit_settings_database_id", Val::Int(8)),
        ("culprit_settings_database_name", Val::Text("foreign".into())),
        ("culprit_settings_updateability", Val::Text("READ_ONLY".into())),
    ] {
        let mut rows = replies();
        rows[8] = Ok(vec![framed_with(crate::settings::tests::scoped(), replace(&control(), &[(field, value)]))]);
        let mut source = settings_source(rows, &request());
        assert_eq!(
            CollectionStop::from_error(&source.describe().await.unwrap_err()),
            CollectionStop::SourceChanged,
            "{field}"
        );
        assert_eq!(source.query_count(), 9);
        assert!(source.read_page(TelemetryStream::SettingObservations, None).await.is_err());
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn missing_unknown_or_duplicate_frame_withholds_only_that_completed_family_without_control_columns_becoming_settings()
 {
    for rows in [
        vec![crate::settings::tests::scoped()],
        vec![framed_with(
            crate::settings::tests::scoped(),
            replace(&control(), &[("culprit_settings_updateability", Val::Null)]),
        )],
        vec![framed_with(framed(crate::settings::tests::scoped()), control())],
        vec![],
    ] {
        let mut responses = replies();
        responses[8] = Ok(rows);
        let mut source = settings_source(responses, &request());
        let description = source.describe().await.unwrap();
        let coverage = &description.observation.snapshot().unwrap().sources;
        assert_eq!(coverage[1].state, SourceState::Unavailable);
        assert!(!coverage[1].complete_scope);
        assert_eq!(coverage[2].state, SourceState::Collected);
        assert!(drain(&mut source).await.iter().all(|row|!matches!(row,TelemetryRecord::Setting(value) if value.namespace=="sys.database_scoped_configurations")));
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn native_permission_failure_preserves_independent_configuration_and_measurement_meanings() {
    let mut rows = replies();
    rows[12] = Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "fixture denied")));
    let mut source = settings_source(rows, &request());
    let description = source.describe().await.unwrap();
    let coverage = &description.observation.snapshot().unwrap().sources;
    assert_eq!(coverage[3].state, SourceState::Unavailable);
    assert_eq!(coverage[3].error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(coverage[2].state, SourceState::Collected);
    assert!(
        drain(&mut source)
            .await
            .iter()
            .any(|row| matches!(row,TelemetryRecord::Setting(value) if value.namespace=="sys.configurations"))
    );
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn cancelled_or_unconfirmed_original_read_stops_without_dispatching_sibling_batches() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::TimedOut,
        CollectionStop::Pressure,
    ] {
        let mut rows = replies();
        rows[8] = Err(ReadError::Stopped(stop));
        let mut source = settings_source(rows, &request());
        source.io.inner.shutdown = if stop == CollectionStop::CancellationUnconfirmed {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        };
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stop);
        assert_eq!(source.query_count(), 9);
        let expected = source.io.inner.shutdown;
        assert_eq!(source.close().await, expected);
        assert_eq!(source.io.inner.closed, 1);
    }
}
#[tokio::test]
async fn missing_or_nonprimary_owner_and_unsupported_platform_never_read_settings() {
    for (field, value) in [
        ("database_guid", Val::Null),
        ("recovery_fork_guid", Val::Null),
        ("updateability", Val::Null),
        ("updateability", Val::Text("READ_ONLY".into())),
    ] {
        let mut rows = replies();
        rows[3] = Ok(vec![replace(&authority("2020-01-01T06:00:00Z"), &[(field, value)])]);
        let mut source = settings_source(rows, &request());
        assert!(source.describe().await.is_err(), "{field}");
        assert_eq!(source.query_count(), 4);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    for (edition, major) in [(3, 12), (8, 16)] {
        let mut rows = replies();
        rows[0] = Ok(vec![Rec::of(&[("edition", Val::Int(edition))])]);
        rows[1] =
            Ok(vec![replace(&metadata(), &[("edition", Val::Int(edition)), ("major", Val::Text(major.to_string()))])]);
        rows[3] = Ok(vec![replace(
            &authority("2020-01-01T06:00:00Z"),
            &[("edition", Val::Int(edition)), ("major", Val::Int(major))],
        )]);
        let mut source = settings_source(rows, &request());
        assert!(source.describe().await.is_err());
        assert_eq!(source.query_count(), 4);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut source = settings_source(replies(), &request());
    source.io.generation = None;
    assert!(source.describe().await.is_err());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn final_database_process_socket_role_and_clock_change_withdraw_completion() {
    for (field, value) in [
        ("database_guid", Val::Text("changed".into())),
        ("selected_database_id", Val::Int(8)),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("observed_at", Val::Text("2019-01-01T00:00:00Z".into())),
    ] {
        let mut rows = replies();
        rows[16] = Ok(vec![replace(&authority("2020-01-01T06:02:02Z"), &[(field, value)])]);
        let mut source = settings_source(rows, &request());
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{field}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut rows = replies();
    rows[15] = Ok(vec![epoch(101)]);
    let mut source = settings_source(rows, &request());
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut source = settings_source(replies(), &request());
    source.io.change_at = Some(15);
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn ordinary_operational_dataset_remains_azure_only() {
    let mut request = request();
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational };
    let mut source = settings_source(vec![Ok(vec![Rec::of(&[("edition", Val::Int(3))])])], &request);
    assert_eq!(source.describe().await.unwrap_err().code, ErrorCode::Unsupported);
    assert_eq!(source.query_count(), 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[test]
fn settings_limits_intersect_every_independent_caller_bound() {
    let upper = super::bounded_budget(&CollectionBudget::upper_limits());
    assert_eq!(upper.max_queries, 18);
    assert_eq!(upper.max_snapshot_rows, 512);
    assert_eq!(upper.page_rows, 64);
    assert_eq!(upper.statement_timeout_ms, 1000);
    assert_eq!(upper.total_timeout_ms, 5000);
    let requested = CollectionBudget {
        max_queries: 7,
        max_snapshot_rows: 4,
        page_rows: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        max_pages: 3,
        statement_timeout_ms: 20,
        total_timeout_ms: 40,
        ..Default::default()
    };
    assert_eq!(super::bounded_budget(&requested), requested);
}
#[tokio::test]
async fn smaller_query_cap_cannot_skip_the_original_final_authority_check() {
    let mut request = request();
    request.budget.max_queries = 16;
    let mut source = settings_source(replies(), &request);
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 16);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_valid_foreign_database_property_row_cannot_borrow_the_batch_authority() {
    let mut rows = replies();
    rows[6] = Ok(vec![framed(crate::settings::tests::database())]);
    let mut source = settings_source(rows, &request());
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.query_count(), 7);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_changed_held_connection_cannot_dispatch_the_catalog_batch_under_the_old_owner() {
    let mut source = settings_source(replies(), &request());
    source.io.change_at = Some(6);
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn an_actual_property_role_cannot_contradict_the_original_batch_role() {
    let mut rows = replies();
    rows[6] = Ok(vec![framed(replace(&properties(), &[("updateability", Val::Text("READ_ONLY".into()))]))]);
    let mut source = settings_source(rows, &request());
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.query_count(), 7);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_later_catalog_row_cannot_borrow_the_first_rows_native_authority() {
    let mut rows = replies();
    rows[8] = Ok(vec![
        framed(crate::settings::tests::scoped()),
        framed_with(
            crate::settings::tests::scoped(),
            replace(&control(), &[("culprit_settings_database_id", Val::Int(8))]),
        ),
    ]);
    let mut source = settings_source(rows, &request());
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.query_count(), 9);
    assert!(source.read_page(TelemetryStream::SettingObservations, None).await.is_err());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
