use super::*;
use crate::{read::QueryRead, rows::Row};
use std::collections::{BTreeMap, VecDeque};

pub(crate) const BACKEND: &str = "2026-11-15T08:00:00.000001Z";
pub(crate) const REQUEST: &str = "2026-11-15T09:00:00.000002Z";
pub(crate) const AT: &str = "2026-11-15T10:00:00.000003Z";

pub(crate) fn row(fields: &[(&str, Option<&str>)]) -> Row {
    Row::from_fields(fields.iter().map(|(key, value)| ((*key).into(), value.map(str::to_owned))))
}
pub(crate) fn metadata_with(standby: &str, started: &str) -> Row {
    metadata_on_version(180_000, standby, started)
}
pub(crate) fn metadata_on_version(version: u32, standby: &str, started: &str) -> Row {
    let build = format!("PostgreSQL {}.0", version / 10_000);
    let version = version.to_string();
    row(&[
        ("build", Some(&build)),
        ("database_oid", Some("16384")),
        ("database", Some("shop")),
        ("version", Some(&version)),
        ("block_bytes", Some("8192")),
        ("standby", Some(standby)),
        ("started_at", Some(started)),
        ("observed_at", Some(AT)),
        ("full_visibility", Some("t")),
        ("replay_pause_requested_access", Some("f")),
        ("replay_pause_state_access", Some("f")),
        ("pgss_schema", None),
        ("pgss_columns", None),
        ("pgss_info", Some("f")),
    ])
}
pub(crate) fn member(address: &str) -> Row {
    row(&[
        ("address", Some(address)),
        ("port", Some("5432")),
        ("system_identifier", Some("42")),
        ("observed_at", Some(AT)),
    ])
}
pub(crate) fn actor(backend: &str, query: &str) -> Row {
    row(&[
        ("observed_at", Some(AT)),
        ("pid", Some("42")),
        ("backend_start", Some(backend)),
        ("query_start", Some(query)),
        ("database_oid", Some("16384")),
        ("database", Some("shop")),
        ("state", Some("active")),
        ("elapsed_ms", Some("2000")),
        ("current_sql", Some("CREATE INDEX ix ON shop_items(id)")),
    ])
}
pub(crate) fn progress() -> Row {
    native_progress(&[
        ("index_oid", Some("0")),
        ("command", Some("CREATE INDEX")),
        ("phase", Some("building index: scanning table")),
        ("blocks_total", Some("9007199254740993")),
        ("blocks_done", Some("0")),
        ("tuples_total", None),
        ("tuples_done", None),
        ("lockers_total", None),
        ("lockers_done", None),
        ("current_locker_pid", Some("0")),
    ])
}

pub(crate) fn native_progress(fields: &[(&str, Option<&str>)]) -> Row {
    let mut values = [
        ("pid", Some("42")),
        ("backend_start", Some(BACKEND)),
        ("query_start", Some(REQUEST)),
        ("observed_at", Some(AT)),
        ("database_oid", Some("16384")),
        ("relation_oid", Some("16420")),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    values.extend(fields.iter().copied());
    row(&values.into_iter().collect::<Vec<_>>())
}

pub(crate) fn vacuum_progress(version: u32, changes: &[(&str, Option<&str>)]) -> Row {
    let mut fields = vec![
        ("phase", Some("scanning heap")),
        ("heap_blks_total", Some("9007199254740993")),
        ("heap_blks_scanned", Some("2")),
        ("heap_blks_vacuumed", Some("0")),
        ("index_vacuum_count", Some("0")),
        ("max_dead_tuples", (version < 170_000).then_some("291")),
        ("num_dead_tuples", (version < 170_000).then_some("0")),
        ("max_dead_tuple_bytes", (version >= 170_000).then_some("9007199254740993")),
        ("dead_tuple_bytes", (version >= 170_000).then_some("0")),
        ("num_dead_item_ids", (version >= 170_000).then_some("1")),
        ("indexes_total", (version >= 170_000).then_some("2")),
        ("indexes_processed", (version >= 170_000).then_some("0")),
        ("cost_delay_ms", (version >= 180_000).then_some("0.125")),
    ];
    fields.extend_from_slice(changes);
    native_progress(&fields)
}

pub(crate) fn analyze_progress(changes: &[(&str, Option<&str>)]) -> Row {
    let mut fields = vec![
        ("phase", Some("acquiring inherited sample rows")),
        ("sample_blks_total", Some("9007199254740993")),
        ("sample_blks_scanned", Some("0")),
        ("ext_stats_total", Some("2")),
        ("ext_stats_computed", None),
        ("child_tables_total", Some("4")),
        ("child_tables_done", Some("1")),
        ("current_child_table_relid", Some("16421")),
        ("cost_delay_ms", Some("0")),
    ];
    fields.extend_from_slice(changes);
    native_progress(&fields)
}

pub(crate) fn cluster_progress(command: &str, changes: &[(&str, Option<&str>)]) -> Row {
    let mut fields = vec![
        ("phase", Some("rebuilding index")),
        ("command", Some(command)),
        ("index_oid", Some("0")),
        ("heap_tuples_scanned", Some("9007199254740993")),
        ("heap_tuples_written", Some("0")),
        ("heap_blks_total", None),
        ("heap_blks_scanned", None),
        ("index_rebuild_count", Some("1")),
    ];
    fields.extend_from_slice(changes);
    native_progress(&fields)
}

pub(crate) fn copy_progress(changes: &[(&str, Option<&str>)]) -> Row {
    let mut fields = vec![
        ("command", Some("COPY FROM")),
        ("type", Some("PIPE")),
        ("bytes_processed", Some("9007199254740993")),
        ("bytes_total", Some("0")),
        ("tuples_processed", Some("9007199254740993")),
        ("tuples_excluded", Some("0")),
        ("tuples_skipped", None),
    ];
    fields.extend_from_slice(changes);
    native_progress(&fields)
}

#[derive(Default)]
pub(crate) struct Reader {
    pub replies: VecDeque<Result<Vec<Row>>>,
    pub calls: Vec<String>,
    pub closed: bool,
    pub unconfirmed: bool,
    pub query_limit: Option<usize>,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, _: usize) -> Result<Vec<Row>> {
        assert!(!self.closed);
        if self.query_limit.is_some_and(|limit| self.calls.len() >= limit) {
            return Err(CollectionStop::QueryLimit.error());
        }
        self.calls.push(sql.into());
        self.replies.pop_front().expect("unexpected native maintenance read")
    }
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    fn queries(&self) -> u32 {
        self.calls.len() as u32
    }
    fn metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed = true;
        if self.unconfirmed { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent }
    }
}

pub(crate) fn scripted_reader() -> Reader {
    progress_reader(180_000, 0, Some(progress()))
}

pub(crate) fn progress_reader(version: u32, empty_views: usize, point: Option<Row>) -> Reader {
    let mut replies = VecDeque::from([
        Ok(vec![metadata_on_version(version, "f", "2026-11-14T00:00:00Z")]),
        Ok(vec![member("127.0.0.1")]),
        Ok(vec![actor(BACKEND, REQUEST)]),
    ]);
    replies.extend((0..empty_views).map(|_| Ok(vec![])));
    if let Some(point) = point {
        replies.push_back(Ok(vec![point]));
    }
    replies.extend([
        Ok(vec![actor(BACKEND, REQUEST)]),
        Ok(vec![metadata_on_version(version, "f", "2026-11-14T00:00:00Z")]),
        Ok(vec![member("127.0.0.1")]),
    ]);
    Reader { replies, ..Default::default() }
}

pub(crate) fn original_source() -> CollectionDescription {
    CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Postgres, "shop", "fixture.invalid", 9, None).into(),
        partition: SourcePartition::Target,
        source: "pg_stat_activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some("original-source-epoch".into()),
            database_writable: Some(true),
            observed_from: "2026-11-15T09:59:59Z".into(),
            observed_until: "2026-11-15T09:59:59Z".into(),
            at: "2026-11-15T09:59:59Z".into(),
            scope: culprit_core::MeasurementScope::Database,
            population: ActivityPopulation::ClientBackends,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::collected("pg_stat_activity".into(), 1),
        }),
    }
}
pub(crate) fn original_focus() -> ActivityRequestFocus {
    let original = native::activity::decode_row(actor(BACKEND, REQUEST), "16384").unwrap().facts.unwrap();
    focus_from(&original_source(), original)
}

pub(crate) fn focus_from(source: &CollectionDescription, original: ActivityObservation) -> ActivityRequestFocus {
    let snapshot = source.observation.activity().unwrap();
    let capture = CaptureIdentity {
        observation_id: "original-maintenance".into(),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"maintenance parent outbox"),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let exported = ExportedRecord::new(source, &capture, original.clone().into()).unwrap();
    let parent =
        DiagnosticEvidenceRef::from_export(source, &digest(b"maintenance parent manifest"), &exported, 0).unwrap();
    let focus = ActivityRequestFocus {
        parent,
        target: source.target.clone(),
        partition: source.partition.clone(),
        database_writable: snapshot.database_writable,
        source: source.source.clone(),
        epoch: snapshot.epoch.clone().unwrap(),
        original_observed_at: snapshot.at.clone(),
        statement_identity: original.statement_identity.clone().unwrap(),
        original,
    };
    assert!(focus.is_valid());
    focus
}
