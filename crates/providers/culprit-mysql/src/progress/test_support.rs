use super::*;
use crate::{
    native::activity::test_support::{consumers, row},
    read::test_support::Read,
    rows::Rec,
};
use mysql_async::Value;

pub(crate) const UUID: &str = "f54a461c-0001-4000-8000-000000000001";
pub(crate) const UID: &str = "q83hHHfky00d9QAi5bwmy+JVlfE=";
pub(crate) const BEFORE: &str = "2020-01-01T06:00:00Z";
pub(crate) const POINT: &str = "2020-01-01T06:00:00.100000Z";
pub(crate) const AFTER: &str = "2020-01-01T06:00:00.800000Z";
const OBSERVED_SECOND: u64 = 1_577_858_400;
const STARTED_SECOND: u64 = OBSERVED_SECOND - 100;

pub(crate) fn connector() -> Arc<Connector> {
    Arc::new(
        Connector::new(&culprit_core::ConnectionSpec {
            host: "fixture.invalid".into(),
            database: Some("shop".into()),
            user: Some("unused".into()),
            ..Default::default()
        })
        .unwrap(),
    )
}
pub(crate) fn bound(
    at: &str,
    version: &str,
    member: Option<&str>,
    readonly: bool,
    uptime: u64,
    stages: bool,
) -> Vec<Result<Vec<Rec>>> {
    let flavor = Flavor::of_version(version);
    let mut rows = vec![Ok(vec![Rec::from_values(vec![
        ("database_name", "shop".into()),
        ("server_id", 1u64.into()),
        ("server_name", "fixture".into()),
        ("connection_id", 90u64.into()),
        ("version", version.into()),
        ("login", "reader@%".into()),
        ("performance_schema", 1u64.into()),
        ("read_only", u64::from(readonly).into()),
        ("observed_at", at.into()),
    ])])];
    if flavor == Flavor::Mysql || source_native::local_context::has_mariadb_uid(version) {
        let field = if flavor == Flavor::Mysql { "server_uuid" } else { "server_uid" };
        rows.push(Ok(vec![Rec::from_values(vec![(field, member.map(Value::from).unwrap_or(Value::NULL))])]));
    }
    rows.push(Ok(vec![Rec::from_values(vec![("observed_second", OBSERVED_SECOND.into()), ("uptime", uptime.into())])]));
    let mut enabled = consumers(true);
    enabled.push(Rec::from_values(vec![
        ("name", "events_stages_current".into()),
        ("enabled", if stages { "YES" } else { "NO" }.into()),
    ]));
    rows.push(Ok(enabled));
    rows
}

pub(crate) fn statement(changes: &[(&str, Value)]) -> Rec {
    let sql = "SELECT private_value FROM orders WHERE id=9264";
    row(7, "Query", true)
        .with_fields(&[
            ("sql_text", sql.into()),
            ("sql_text_bytes", (sql.len() as u64).into()),
            ("rows_examined", 9_007_199_254_740_993u64.into()),
            ("rows_sent", 0u64.into()),
            ("created_tmp_tables", 1u64.into()),
            ("created_tmp_disk_tables", 0u64.into()),
            ("sort_merge_passes", Value::NULL),
        ])
        .with_fields(changes)
}
pub(crate) fn stage(changes: &[(&str, Value)]) -> Rec {
    Rec::from_values(vec![
        ("thread_id", 107u64.into()),
        ("event_id", 4u64.into()),
        ("end_event_id", Value::NULL),
        ("nesting_event_id", 3u64.into()),
        ("nesting_event_type", "STATEMENT".into()),
        ("event_name", "stage/sql/copy to tmp table".into()),
        ("timer_start_ps", Value::NULL),
        ("timer_wait_ps", 0u64.into()),
        ("work_completed", 9_007_199_254_740_993u64.into()),
        ("work_estimated", 9_007_199_254_740_994u64.into()),
    ])
    .with_fields(changes)
}
pub(crate) fn reader(after: Vec<Result<Vec<Rec>>>, stage: Result<Vec<Rec>>, last: Rec) -> Read {
    let mut rows = bound(BEFORE, "8.4.0", Some(UUID), false, 100, true);
    rows.push(Ok(vec![statement(&[])]));
    rows.push(stage);
    rows.push(Ok(vec![last]));
    rows.extend(after);
    Read::with(rows).with_native_phases().with_query_budget(MAX_SOURCE_QUERIES, 7)
}
pub(crate) fn session_point(changes: &[(&str, Value)]) -> Rec {
    let sql = "ALTER TABLE orders ADD INDEX ix_customer(customer_id)";
    Rec::from_values(vec![
        ("id", 7u64.into()),
        ("db", "shop".into()),
        ("command", "Query".into()),
        ("state", "copy to tmp table".into()),
        ("sql_text", sql.into()),
        ("sql_text_bytes", (sql.len() as u64).into()),
        ("stage", Value::NULL),
        ("max_stage", Value::NULL),
        ("progress", Value::NULL),
        ("observed_at", POINT.into()),
    ])
    .with_fields(changes)
}

pub(crate) fn without_performance_schema(at: &str, version: &str) -> Vec<Result<Vec<Rec>>> {
    let mut rows = bound(at, version, None, false, 100, false);
    let context = rows.remove(0).unwrap().remove(0).with_fields(&[("performance_schema", 0u64.into())]);
    vec![Ok(vec![context])]
}

pub(crate) fn without_statement_consumers(at: &str) -> Vec<Result<Vec<Rec>>> {
    let mut rows = bound(at, "8.4.0", Some(UUID), false, 100, false);
    let mut disabled = consumers(false);
    disabled.push(Rec::from_values(vec![("name", "events_stages_current".into()), ("enabled", "NO".into())]));
    *rows.last_mut().unwrap() = Ok(disabled);
    rows
}

pub(crate) fn original_focus() -> ActivityRequestFocus {
    original_focus_in(SourcePartition::InstanceLocal {
        member: Some(source_native::instance::resource(Flavor::Mysql, UUID).unwrap()),
        epoch: Some(source_native::local_context::process_epoch(STARTED_SECOND)),
    })
}
pub(crate) fn original_focus_in(partition: SourcePartition) -> ActivityRequestFocus {
    let original = source_native::activity::decode(&row(7, "Query", true), "shop", true).unwrap().unwrap();
    let epoch = source_native::local_context::process_epoch(STARTED_SECOND);
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Mysql, "shop", "fixture.invalid", 3306, None).into(),
        partition,
        source: source_native::activity::SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some(epoch.clone()),
            database_writable: Some(true),
            observed_from: BEFORE.into(),
            observed_until: AFTER.into(),
            at: POINT.into(),
            scope: culprit_core::MeasurementScope::Database,
            population: ActivityPopulation::DefaultDatabaseConnections,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::collected(source_native::activity::SOURCE.into(), 1),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "original-current-request".into(),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"current-request-parent"),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let exported = ExportedRecord::new(&source, &capture, TelemetryRecord::from(original.clone())).unwrap();
    let parent =
        DiagnosticEvidenceRef::from_export(&source, &digest(b"current request original manifest"), &exported, 0)
            .unwrap();
    let focus = ActivityRequestFocus {
        parent,
        target: source.target.clone(),
        partition: source.partition.clone(),
        database_writable: Some(true),
        source: source.source.clone(),
        epoch,
        original_observed_at: POINT.into(),
        statement_identity: original.statement_identity.clone().unwrap(),
        original,
    };
    assert!(focus.is_current_request_work_focus());
    assert!(focus.matches_parent(&source, &focus.original, &focus.parent));
    focus
}
