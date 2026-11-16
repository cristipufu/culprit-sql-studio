use super::*;
use culprit_postgres::wire::QueryRead;
use std::collections::VecDeque;

pub(crate) const NODE: &str = "0123456789abcdef0123456789abcdef";
pub(crate) const BACKEND: &str = "2026-11-15T08:00:00.000001Z";
pub(crate) const REQUEST: &str = "2026-11-15T09:00:00.000002Z";
pub(crate) const AT: &str = "2026-11-15T10:00:00.000003Z";
pub(crate) fn row(fields: &[(&str, Option<&str>)]) -> Row {
    Row::from_fields(fields.iter().map(|(name, value)| ((*name).into(), value.map(str::to_owned))))
}
pub(crate) fn metadata(started: &str) -> Row {
    row(&[
        ("version", Some("PostgreSQL 15.2-YB-2025.2.0.0")),
        ("compatibility", Some("150002")),
        ("database", Some("shop")),
        ("oid", Some("16384")),
        ("now", Some(AT)),
        ("started", Some(started)),
        ("global_views", Some("on")),
        ("cluster_statements", Some("t")),
        ("statement_schema", None),
        ("metrics", Some("off")),
        ("rpc_metrics", Some("off")),
        ("role_oid", Some("10")),
        ("all_stats", Some("t")),
    ])
}
pub(crate) fn node(node: &str) -> Row {
    row(&[("node", Some(node))])
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
        ("index_oid", Some("16421")),
        ("command", Some("CREATE INDEX CONCURRENTLY")),
        ("phase", Some("backfilling")),
        ("tuples_done", Some("9007199254740993")),
        ("tuples_total", Some("42")),
    ])
}
#[derive(Default)]
pub(crate) struct Reader {
    pub replies: VecDeque<Result<Vec<Row>>>,
    pub calls: Vec<String>,
    pub closed: bool,
    pub unconfirmed: bool,
    pub notices: u64,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, _: usize) -> Result<Vec<Row>> {
        assert!(!self.closed);
        self.calls.push(sql.into());
        self.replies.pop_front().expect("unexpected native maintenance read")
    }
}
impl culprit_postgres::wire::NoticeRead for Reader {
    fn notices(&self) -> u64 {
        self.notices
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
            Ok(vec![metadata("2026-11-14T00:00:00Z")]),
            Ok(vec![node(NODE)]),
            Ok(vec![row(&[("backend", Some("99"))])]),
            Ok(vec![actor(BACKEND, REQUEST)]),
            Ok(vec![progress()]),
            Ok(vec![actor(BACKEND, REQUEST)]),
            Ok(vec![metadata("2026-11-14T00:00:00Z")]),
            Ok(vec![node(NODE)]),
            Ok(vec![row(&[("backend", Some("99"))])]),
        ]
        .into(),
        ..Default::default()
    }
}
pub(crate) fn original_source() -> CollectionDescription {
    CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Yugabyte, "shop", "fixture.invalid", 9, None).into(),
        partition: SourcePartition::Target,
        source: format!("ysql-activity-{NODE}"),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some("original-ysql-epoch".into()),
            database_writable: None,
            observed_from: "2026-11-15T09:59:59Z".into(),
            observed_until: "2026-11-15T09:59:59Z".into(),
            at: "2026-11-15T09:59:59Z".into(),
            scope: culprit_core::MeasurementScope::Node,
            population: ActivityPopulation::NodeClientBackendsAndConnectionWorkers,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::collected(format!("ysql-activity-{NODE}"), 1),
        }),
    }
}
pub(crate) fn original_focus() -> ActivityRequestFocus {
    let mut original =
        culprit_postgres::wire::activity::decode_row(actor(BACKEND, REQUEST), "16384").unwrap().facts.unwrap();
    original.key.session.id = format!("{NODE}/42");
    original.blocking_status = ActivityBlockingStatus::Unknown;
    original.statement_identity.as_mut().unwrap().qualifiers.insert("node".into(), NODE.into());
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
