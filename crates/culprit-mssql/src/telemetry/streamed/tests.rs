use super::*;
use crate::telemetry::grants::Grants;
use crate::{
    read::{NativeRead, ReadBatch, ReadResult},
    resources::test_support::{Call, FakeRead, latest_row, replace},
    value::{Rec, Val},
};
use std::collections::VecDeque;

struct StreamingRead {
    native: FakeRead,
    batches: VecDeque<ReadResult<ReadBatch>>,
    starts: u32,
    pause_batches: bool,
}
#[async_trait::async_trait]
impl NativeRead for StreamingRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.native.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.native.queries()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.native.close().await
    }
}
#[async_trait::async_trait]
impl NativeStreamRead for StreamingRead {
    async fn begin_stream(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<()> {
        self.starts += 1;
        self.native.calls.push(Call { sql: sql.into(), parameters, limit });
        Ok(())
    }
    async fn next_batch(&mut self) -> ReadResult<ReadBatch> {
        if self.pause_batches {
            std::future::pending::<()>().await;
        }
        self.batches.pop_front().expect("unexpected native batch")
    }
}
fn identity() -> Rec {
    Rec::of(&[
        ("database_name", Val::Text("sample".into())),
        ("incarnation", Val::Text("logical-guid".into())),
        ("physical_incarnation", Val::Text("physical-guid".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
    ])
}
fn clock(at: &str) -> Rec {
    Rec::of(&[("observed_at", Val::Text(at.into())), ("metadata_visibility", Val::Int(1))])
}
fn pressure() -> Rec {
    replace(
        &latest_row("2020-01-01T06:01:30Z"),
        &[
            ("avg_instance_cpu_percent", Val::Float(10.0)),
            ("max_worker_percent", Val::Float(10.0)),
            ("avg_data_io_percent", Val::Float(10.0)),
            ("avg_log_write_percent", Val::Float(10.0)),
        ],
    )
}
fn request() -> CollectionRequest {
    CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::StatementGrants },
        budget: CollectionBudget { page_rows: 1, max_snapshot_rows: 100_000, ..Default::default() },
    }
}
fn fixture_source(
    available: bool,
    batches: Vec<ReadResult<ReadBatch>>,
    final_identity: Rec,
) -> StreamedSnapshot<Grants, StreamingRead> {
    fixture_with_options(available, batches, final_identity, false, SourceShutdown::Quiescent)
}
fn fixture_with_options(
    available: bool,
    batches: Vec<ReadResult<ReadBatch>>,
    final_identity: Rec,
    pause_batches: bool,
    shutdown: SourceShutdown,
) -> StreamedSnapshot<Grants, StreamingRead> {
    let columns = if available {
        vec![
            "total_grant_kb",
            "total_used_grant_kb",
            "last_grant_kb",
            "last_used_grant_kb",
            "min_grant_kb",
            "min_used_grant_kb",
            "max_grant_kb",
            "max_used_grant_kb",
        ]
    } else {
        vec![]
    };
    let mut replies = vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![clock("2020-01-01T06:02:00Z")]),
        Ok(vec![pressure()]),
        Ok(vec![crate::grants::tests::layout_row(true, &columns)]),
    ];
    if available {
        replies.push(Ok(vec![pressure()]));
    }
    replies.extend([Ok(vec![final_identity]), Ok(vec![clock("2020-01-01T06:02:01Z")])]);
    let mut native = FakeRead::new(replies);
    native.shutdown = shutdown;
    let io = StreamingRead { native, batches: batches.into(), starts: 0, pause_batches };
    StreamedSnapshot::with_io(io, &request(), "fixture.invalid".into(), 1433, Grants).unwrap()
}
fn catalog_source<D: SnapshotRecipe>(
    recipe: D,
    row: Rec,
    visible: bool,
    final_visible: bool,
    budget: CollectionBudget,
) -> StreamedSnapshot<D, StreamingRead> {
    let io = StreamingRead {
        native: FakeRead::new(vec![
            Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
            Ok(vec![identity()]),
            Ok(vec![replace(&clock("2020-01-01T06:02:00Z"), &[("metadata_visibility", Val::Int(i64::from(visible)))])]),
            Ok(vec![pressure()]),
            Ok(vec![pressure()]),
            Ok(vec![identity()]),
            Ok(vec![replace(
                &clock("2020-01-01T06:02:01Z"),
                &[("metadata_visibility", Val::Int(i64::from(final_visible)))],
            )]),
        ]),
        batches: vec![batch(row, true)].into(),
        starts: 0,
        pause_batches: false,
    };
    StreamedSnapshot::with_io(
        io,
        &CollectionRequest { selection: CollectionSelection::Snapshot { dataset: D::DATASET }, budget },
        "fixture.invalid".into(),
        1433,
        recipe,
    )
    .unwrap()
}
fn batch(row: Rec, complete: bool) -> ReadResult<ReadBatch> {
    Ok(ReadBatch { rows: vec![row], complete })
}

#[tokio::test]
async fn native_rows_stream_once_and_only_drained_final_counts_can_complete_coverage() {
    let row = crate::grants::tests::row();
    let second = replace(
        &row,
        &[("plan_generation_num", Val::Int(2)), ("text_prefix", Val::Text("SELECT /* culprit */ 1".into()))],
    );
    let mut source = fixture_source(true, vec![batch(row, false), batch(second, true)], identity());
    assert_eq!(source.query_count(), 0, "factory construction performs no native reads");
    let initial = source.describe().await.unwrap();
    assert!(matches!(initial.observation, SourceObservation::PagedSnapshot(_)));
    assert!(!initial.observation.covers_request());
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("first page")
    };
    assert_eq!(records.len(), 1);
    assert_eq!(next_cursor.as_deref(), Some("1"));
    assert!(source.finish().await.is_err(), "provisional rows cannot establish final coverage");
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, Some("1")).await.unwrap()
    else {
        panic!("final page")
    };
    assert!(next_cursor.is_none());
    let TelemetryRecord::Operational(value) = &records[0] else { panic!("operational record") };
    let OperationalFact::MssqlStatementGrants(grants) = &value.fact else { panic!("grant record") };
    assert_eq!(grants.context.origin, StatementOrigin::Culprit, "raw overhead evidence remains present");
    assert_eq!(
        source.inner.reader().io.starts,
        1,
        "every page belongs to one native statement rather than a repeated DMV scan"
    );
    let stream_call =
        source.inner.reader().io.native.calls.iter().find(|call| call.sql.contains("qs.plan_handle")).unwrap();
    assert_eq!(stream_call.limit, 100_001);
    let completion = source.finish().await.unwrap();
    let completed = initial.completed(completion).unwrap().unwrap();
    let snapshot = completed.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 2);
    assert_eq!(snapshot.sources[0].records, 2);
    assert!(completed.observation.covers_request());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.inner.reader().io.native.closed, 1);
}

#[tokio::test]
async fn absent_native_columns_produce_not_applicable_coverage_without_running_a_stream() {
    let mut source = fixture_source(false, vec![], identity());
    let initial = source.describe().await.unwrap();
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("empty page")
    };
    assert!(records.is_empty());
    assert!(next_cursor.is_none());
    assert_eq!(source.inner.reader().io.starts, 0);
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    let source = &completed.observation.snapshot().unwrap().sources[0];
    assert_eq!(source.state, culprit_core::SourceState::NotApplicable);
    assert_eq!(source.records, 0);
}

#[tokio::test]
async fn late_native_errors_and_invalid_cursors_prevent_completion_and_new_reads() {
    let mut source = fixture_with_options(
        true,
        vec![batch(crate::grants::tests::row(), false), Err(ReadError::Source(invalid_data()))],
        identity(),
        false,
        SourceShutdown::Unconfirmed,
    );
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert!(source.read_page(TelemetryStream::OperationalSamples, Some("1")).await.is_err());
    let calls = source.query_count();
    assert!(source.finish().await.is_err());
    assert!(source.read_page(TelemetryStream::OperationalSamples, Some("1")).await.is_err());
    assert_eq!(source.query_count(), calls);
    assert_eq!(source.inner.reader().io.starts, 1);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    let mut invalid = fixture_source(true, vec![], identity());
    invalid.describe().await.unwrap();
    assert!(invalid.read_page(TelemetryStream::OperationalSamples, Some("1")).await.is_err());
    assert_eq!(invalid.inner.reader().io.starts, 0);
}

#[tokio::test]
async fn a_different_physical_database_cannot_certify_even_a_drained_stream() {
    let changed = replace(&identity(), &[("physical_incarnation", Val::Text("another-physical-guid".into()))]);
    let mut source = fixture_source(true, vec![batch(crate::grants::tests::row(), true)], changed);
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert!(matches!(source.finish().await.unwrap(), SourceCompletion::Changed));
}

#[tokio::test]
async fn index_recipe_uses_the_shared_stream_owner_and_final_coverage() {
    use crate::telemetry::index_operations::IndexOperations;
    let request = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::IndexOperations },
        budget: Default::default(),
    };
    let io = StreamingRead {
        native: FakeRead::new(vec![
            Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
            Ok(vec![identity()]),
            Ok(vec![clock("2020-01-01T06:02:00Z")]),
            Ok(vec![pressure()]),
            Ok(vec![crate::index_operations::tests::layout_row(true, &[])]),
            Ok(vec![pressure()]),
            Ok(vec![identity()]),
            Ok(vec![clock("2020-01-01T06:02:01Z")]),
        ]),
        batches: vec![batch(crate::index_operations::tests::row(), true)].into(),
        starts: 0,
        pause_batches: false,
    };
    let mut source = StreamedSnapshot::with_io(io, &request, "fixture.invalid".into(), 1433, IndexOperations).unwrap();
    let initial = source.describe().await.unwrap();
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("page")
    };
    assert!(next_cursor.is_none());
    assert_eq!(source.inner.reader().io.starts, 1);
    let TelemetryRecord::Operational(observation) = &records[0] else { panic!("operational") };
    let OperationalFact::MssqlIndexOperations(index) = &observation.fact else { panic!("index") };
    assert!(index.is_valid());
    assert_eq!(observation.key, index.identity.key().unwrap());
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert_eq!(completed.observation.snapshot().unwrap().records, 1);
    let queries = source.query_count();
    assert!(source.finish().await.is_err(), "finalization cannot reread native state");
    assert_eq!(source.query_count(), queries);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn table_capture_shares_native_decoding_and_requires_full_metadata_visibility() {
    use crate::telemetry::tables::Tables;
    for visible in [true, false] {
        let mut source = catalog_source(Tables, crate::tables::tests::row(), visible, visible, Default::default());
        if !visible {
            assert_eq!(source.describe().await.err().unwrap().code, culprit_core::ErrorCode::PermissionDenied);
            assert_eq!(source.inner.reader().io.starts, 0);
            assert!(source.finish().await.is_err());
            assert_eq!(source.close().await, SourceShutdown::Quiescent);
            continue;
        }
        let initial = source.describe().await.unwrap();
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
        else {
            panic!("page")
        };
        assert!(next_cursor.is_none());
        let TelemetryRecord::Operational(observation) = &records[0] else { panic!("operational") };
        let OperationalFact::MssqlTable(table) = &observation.fact else { panic!("table") };
        assert_eq!(table.estimated_rows.as_deref(), Some("9007199254740993"));
        assert_eq!(observation.key, table.identity.key().unwrap());
        let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
        assert_eq!(completed.observation.snapshot().unwrap().records, 1);
        assert_eq!(completed.observation.snapshot().unwrap().sources[0].source, crate::sources::TABLES.name);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn foreign_key_versions_require_complete_metadata_and_seal_the_definition_stream() {
    use crate::telemetry::foreign_keys::ForeignKeys;
    for visible in [true, false] {
        let mut source =
            catalog_source(ForeignKeys, crate::foreign_keys::tests::row(), visible, visible, Default::default());
        if !visible {
            assert_eq!(source.describe().await.err().unwrap().code, culprit_core::ErrorCode::PermissionDenied);
            assert_eq!(source.inner.reader().io.starts, 0);
            assert!(source.finish().await.is_err());
        } else {
            let initial = source.describe().await.unwrap();
            let CollectionRead::Page { records, next_cursor } =
                source.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
            else {
                panic!("definition page")
            };
            assert!(next_cursor.is_none());
            let TelemetryRecord::ObjectDefinition(definition) = &records[0] else { panic!("definition") };
            assert!(definition.is_valid());
            let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
            let snapshot = completed.observation.snapshot().unwrap();
            assert_eq!(snapshot.records, 1);
            assert_eq!(snapshot.sources[0].source, crate::sources::FOREIGN_KEYS.name);
            assert_eq!(source.inner.reader().io.starts, 1);
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn catalog_sql_bodies_obey_the_capture_budget_and_the_export_boundary_redacts_injected_text() {
    use crate::telemetry::columns::Columns;
    let budget = CollectionBudget { max_sql_bytes: 1024, ..Default::default() };
    let row =
        replace(&crate::columns::tests::row(), &[("default_bytes", Val::Int(2048)), ("default_value", Val::Null)]);
    let mut source = catalog_source(Columns, row, true, true, budget);
    let initial = source.describe().await.unwrap();
    let CollectionRead::Page { records, .. } =
        source.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
    else {
        panic!("definition page")
    };
    let TelemetryRecord::ObjectDefinition(record) = &records[0] else { panic!("definition") };
    let ObjectDefinitionFact::MssqlColumn(column) = &record.fact else { panic!("column") };
    assert!(matches!(
        column.default.as_ref().unwrap().payload,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
    ));
    assert!(
        source.inner.reader().io.native.calls.iter().any(|call| call.sql.contains("DATALENGTH(dc.definition)<=1024"))
    );
    let mut fact = record.fact.clone();
    let ObjectDefinitionFact::MssqlColumn(column) = &mut fact else { unreachable!() };
    let raw = "(N'export-boundary-secret')";
    column.default = Some(CatalogExpression {
        native_sha256: Some(digest(raw.as_bytes())),
        payload: DefinitionPayload::Available {
            text: raw.into(),
            sha256: digest(raw.as_bytes()),
            redaction_version: REDACTION_VERSION,
        },
    });
    let record = ObjectDefinitionRecord::new(crate::sources::COLUMNS.name.into(), fact).unwrap();
    let exported = ExportedRecord::new(
        &initial,
        &CaptureIdentity { observation_id: "fixture".into(), revision: None },
        TelemetryRecord::ObjectDefinition(record),
    )
    .unwrap();
    assert!(!serde_json::to_string(&exported).unwrap().contains("export-boundary-secret"));
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert_eq!(completed.observation.snapshot().unwrap().records, 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn statistics_capture_keeps_unreadable_properties_explicit_after_native_stream_completion() {
    use crate::telemetry::statistics::Statistics;
    let row = replace(
        &crate::statistics::tests::row(),
        &[
            ("properties_available", Val::Int(0)),
            ("last_updated_without_offset", Val::Null),
            ("display_updated_at", Val::Null),
            ("rows", Val::Null),
            ("rows_sampled", Val::Null),
            ("steps", Val::Null),
            ("unfiltered_rows", Val::Null),
            ("modification_counter", Val::Null),
        ],
    );
    let mut source = catalog_source(Statistics, row, true, true, Default::default());
    let initial = source.describe().await.unwrap();
    let CollectionRead::Page { records, .. } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("statistics page")
    };
    let TelemetryRecord::Operational(record) = &records[0] else { panic!("observation") };
    let OperationalFact::MssqlStatistics(statistics) = &record.fact else { panic!("statistics") };
    assert!(!statistics.properties_available);
    assert!(statistics.rows.is_none() && statistics.modifications.is_none());
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    let snapshot = completed.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 1);
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
    assert!(!snapshot.sources[0].complete_scope);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn index_capture_uses_native_column_identities_and_preserves_counter_availability() {
    use crate::telemetry::indexes::Indexes;
    let mut source = catalog_source(Indexes, crate::indexes::tests::row(), true, true, Default::default());
    let initial = source.describe().await.unwrap();
    let CollectionRead::Page { records, .. } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("index page")
    };
    let TelemetryRecord::Operational(record) = &records[0] else { panic!("observation") };
    let OperationalFact::MssqlIndex(index) = &record.fact else { panic!("index") };
    assert_eq!(record.key, index.key().unwrap());
    assert_eq!(index.columns[0].name, "owner\u{1f}name");
    let MssqlIndexUsage::Observed(usage) = &index.usage else { panic!("usage") };
    assert_eq!(usage.user_seeks.as_deref(), Some("9007199254740993"));
    let exported = ExportedRecord::new(
        &initial,
        &CaptureIdentity { observation_id: "fixture".into(), revision: None },
        records[0].clone(),
    )
    .unwrap();
    assert!(!serde_json::to_string(&exported).unwrap().contains("private-owner"));
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert_eq!(completed.observation.snapshot().unwrap().sources[0].source, crate::sources::INDEXES.name);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn losing_metadata_visibility_prevents_a_drained_catalog_from_certifying_complete_scope() {
    use crate::telemetry::indexes::Indexes;
    let mut source = catalog_source(Indexes, crate::indexes::tests::row(), true, false, Default::default());
    let initial = source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    let snapshot = completed.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 1, "retain the facts that were read");
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
    assert_eq!(snapshot.sources[0].error_code, Some(culprit_core::ErrorCode::PermissionDenied));
    assert!(!snapshot.sources[0].complete_scope);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn check_definitions_keep_native_trust_and_body_availability_in_the_definition_stream() {
    use crate::telemetry::check_constraints::CheckConstraints;
    let mut source =
        catalog_source(CheckConstraints, crate::check_constraints::tests::row(), true, true, Default::default());
    let initial = source.describe().await.unwrap();
    let CollectionRead::Page { records, .. } =
        source.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
    else {
        panic!("definition page")
    };
    let TelemetryRecord::ObjectDefinition(definition) = &records[0] else { panic!("definition") };
    let ObjectDefinitionFact::MssqlCheckConstraint(constraint) = &definition.fact else { panic!("check constraint") };
    assert!(constraint.disabled && !constraint.trusted);
    assert!(definition.is_valid());
    assert_eq!(definition.fact.definition_payloads().count(), 1);
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert_eq!(completed.observation.snapshot().unwrap().sources[0].source, crate::sources::CHECK_CONSTRAINTS.name);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn retiring_a_page_future_requires_native_close_before_any_more_work() {
    let mut source = fixture_with_options(true, vec![], identity(), true, SourceShutdown::Unconfirmed);
    source.describe().await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(1),
            source.read_page(TelemetryStream::OperationalSamples, None)
        )
        .await
        .is_err()
    );
    assert_eq!(source.inner.reader().io.starts, 1);
    let reads = source.query_count();
    assert!(source.read_page(TelemetryStream::OperationalSamples, None).await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), reads);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(source.inner.reader().io.native.closed, 1);
}

#[tokio::test]
async fn compilation_capture_streams_retained_lifetime_totals_through_the_shared_decoder() {
    use crate::telemetry::compilation::QueryCompilation;
    let mut source =
        catalog_source(QueryCompilation, crate::query_store::tests::compilation_row(), true, true, Default::default());
    let initial = source.describe().await.unwrap();
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("compilation page")
    };
    assert!(next_cursor.is_none());
    let TelemetryRecord::Operational(record) = &records[0] else { panic!("operational record") };
    let OperationalFact::MssqlQueryCompilation(compilation) = &record.fact else { panic!("compilation") };
    assert_eq!(record.key, compilation.key().unwrap());
    assert_eq!(compilation.compilations, "9007199254740993");
    let exported = ExportedRecord::new(
        &initial,
        &CaptureIdentity { observation_id: "fixture".into(), revision: None },
        records[0].clone(),
    )
    .unwrap();
    assert!(serde_json::to_string(&exported).unwrap().contains("9007199254740993"));
    assert_eq!(source.inner.reader().io.starts, 1);
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert_eq!(completed.observation.snapshot().unwrap().sources[0].source, crate::sources::QUERY_COMPILATION.name);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn cached_runtime_uses_the_shared_identity_and_exports_only_qualified_native_counters() {
    use crate::telemetry::cached_runtime::CachedRuntime;
    for measured in [true, false] {
        let row = if measured {
            crate::cached_runtime::tests::row()
        } else {
            replace(&crate::cached_runtime::tests::row(), &[("plan_handle", Val::Bytes(vec![0; 64]))])
        };
        let mut source = catalog_source(CachedRuntime, row, true, true, Default::default());
        let initial = source.describe().await.unwrap();
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
        else {
            panic!("runtime page")
        };
        assert!(next_cursor.is_none());
        let TelemetryRecord::Operational(record) = &records[0] else { panic!("runtime observation") };
        let OperationalFact::MssqlCachedRuntime(runtime) = &record.fact else { panic!("cache runtime") };
        assert_eq!(runtime.measured(), measured);
        assert_eq!(record.key, runtime.context.identity.key().unwrap());
        ExportedRecord::new(
            &initial,
            &CaptureIdentity { observation_id: "fixture".into(), revision: None },
            records[0].clone(),
        )
        .unwrap();
        let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
        let coverage = &completed.observation.snapshot().unwrap().sources[0];
        assert_eq!(coverage.complete_scope, measured);
        assert_eq!(coverage.records, 1);
        assert_eq!(source.inner.reader().io.starts, 1);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn unknown_trigger_events_remain_definitions_with_partial_scope_after_the_stream_is_drained() {
    use crate::telemetry::triggers::Triggers;
    for known in [true, false] {
        let row = if known {
            crate::triggers::tests::row()
        } else {
            replace(&crate::triggers::tests::row(), &[("insert_event", Val::Null)])
        };
        let mut source = catalog_source(Triggers, row, true, true, Default::default());
        let initial = source.describe().await.unwrap();
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
        else {
            panic!("trigger definitions")
        };
        assert!(next_cursor.is_none());
        assert_eq!(records.len(), 1);
        let exported = ExportedRecord::new(
            &initial,
            &CaptureIdentity { observation_id: "fixture".into(), revision: None },
            records[0].clone(),
        )
        .unwrap();
        assert!(exported.record_id.is_some());
        let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
        let coverage = &completed.observation.snapshot().unwrap().sources[0];
        assert_eq!(coverage.complete_scope, known);
        assert_eq!(coverage.records, 1);
        assert_eq!(
            coverage.state,
            if known { culprit_core::SourceState::Collected } else { culprit_core::SourceState::Partial }
        );
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn object_metadata_uses_versioned_definitions_and_losing_visibility_keeps_scope_partial() {
    use crate::telemetry::objects::Objects;
    for final_visible in [true, false] {
        let mut source =
            catalog_source(Objects, crate::recorded::tests::row(), true, final_visible, Default::default());
        let initial = source.describe().await.unwrap();
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
        else {
            panic!("objects page")
        };
        assert!(next_cursor.is_none());
        let TelemetryRecord::ObjectDefinition(record) = &records[0] else { panic!("object definition") };
        let ObjectDefinitionFact::MssqlObject(object) = &record.fact else { panic!("native object") };
        assert_eq!(object.identity.created_without_offset, "2020-01-01T08:00:00.0030000");
        let mut changed = object.clone();
        changed.modified_without_offset = "2020-01-01T09:01:00.0070000".into();
        let second =
            ObjectDefinitionRecord::new(record.source.clone(), ObjectDefinitionFact::MssqlObject(changed)).unwrap();
        assert_eq!(record.key, second.key);
        assert_ne!(record.version, second.version);
        ExportedRecord::new(
            &initial,
            &CaptureIdentity { observation_id: "fixture".into(), revision: None },
            records[0].clone(),
        )
        .unwrap();
        let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
        assert_eq!(completed.observation.snapshot().unwrap().sources[0].complete_scope, final_visible);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn partition_exports_keep_native_dates_after_redaction_and_qualify_missing_value_fingerprints() {
    use crate::telemetry::partitions::Partitions;
    for fingerprint in [true, false] {
        let row = if fingerprint {
            crate::partitions::tests::row()
        } else {
            replace(&crate::partitions::tests::row(), &[("native_value_sha256", Val::Null)])
        };
        let mut source = catalog_source(Partitions, row, true, true, Default::default());
        let initial = source.describe().await.unwrap();
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
        else {
            panic!("partition definitions")
        };
        assert!(next_cursor.is_none());
        let exported = ExportedRecord::new(
            &initial,
            &CaptureIdentity { observation_id: "fixture".into(), revision: None },
            records[0].clone(),
        )
        .unwrap();
        let encoded = serde_json::to_string(&exported).unwrap();
        assert!(encoded.contains("2026-11-10T00:00:00.1234567"));
        let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
        assert_eq!(completed.observation.snapshot().unwrap().sources[0].complete_scope, fingerprint);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn cached_definitions_export_two_bounded_payloads_without_duplicating_the_native_read() {
    use crate::telemetry::cached_definitions::CachedDefinitions;
    let mut source =
        catalog_source(CachedDefinitions, crate::cached_definitions::tests::row(), true, true, Default::default());
    let initial = source.describe().await.unwrap();
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
    else {
        panic!("cached definitions")
    };
    assert!(next_cursor.is_none());
    assert_eq!(records.len(), 1);
    assert_eq!(source.inner.reader().io.starts, 1);
    let exported = ExportedRecord::new(
        &initial,
        &CaptureIdentity { observation_id: "fixture".into(), revision: None },
        records[0].clone(),
    )
    .unwrap();
    assert!(!serde_json::to_string(&exported).unwrap().contains("private-value"));
    let TelemetryRecord::ObjectDefinition(record) = &exported.record else { panic!("definition") };
    assert_eq!(record.fact.definition_payloads().count(), 2);
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert!(completed.observation.snapshot().unwrap().sources[0].complete_scope);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
