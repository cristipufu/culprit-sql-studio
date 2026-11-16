use super::*;
use std::collections::VecDeque;

pub(crate) const BACKEND: &str = "2026-11-15T08:00:00.000001Z";
pub(crate) const REQUEST: &str = "2026-11-15T09:00:00.000002Z";
pub(crate) const AT: &str = "2026-11-15T10:00:00.000003Z";

pub(crate) fn row(fields: &[(&str, Option<&str>)]) -> Row {
    Row::from_fields(fields.iter().map(|(key, value)| ((*key).into(), value.map(str::to_owned))))
}
pub(crate) fn metadata() -> Row {
    metadata_with("f", "2026-11-14T00:00:00Z")
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
    row(&[
        ("pid", Some("42")),
        ("backend_start", Some(BACKEND)),
        ("query_start", Some(REQUEST)),
        ("observed_at", Some(AT)),
        ("database_oid", Some("16384")),
        ("relation_oid", Some("16420")),
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

#[derive(Default)]
pub(crate) struct Reader {
    pub replies: VecDeque<Result<Vec<Row>>>,
    pub calls: Vec<String>,
    pub closed: bool,
    pub unconfirmed: bool,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, _: usize) -> Result<Vec<Row>> {
        assert!(!self.closed);
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
    Reader {
        replies: [
            Ok(vec![metadata()]),
            Ok(vec![member("127.0.0.1")]),
            Ok(vec![actor(BACKEND, REQUEST)]),
            Ok(vec![progress()]),
            Ok(vec![actor(BACKEND, REQUEST)]),
            Ok(vec![metadata()]),
            Ok(vec![member("127.0.0.1")]),
        ]
        .into(),
        ..Default::default()
    }
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
