use super::*;
use crate::native::replication::ReplayPauseAccess;
use std::collections::VecDeque;

struct Reader {
    replies: VecDeque<Result<Vec<Row>>>,
    calls: Vec<String>,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, _: usize) -> Result<Vec<Row>> {
        self.calls.push(sql.into());
        self.replies.pop_front().expect("one original metadata read")
    }
}

fn row() -> Row {
    Row::from_fields(
        [
            ("build", Some("PostgreSQL 18.6")),
            ("database_oid", Some("16384")),
            ("database", Some("application")),
            ("version", Some("180006")),
            ("block_bytes", Some("8192")),
            ("standby", Some("t")),
            ("started_at", Some("2026-11-13T05:00:00Z")),
            ("observed_at", Some("2026-11-13T06:00:00Z")),
            ("full_visibility", Some("t")),
            ("pgss_schema", None),
            ("pgss_columns", None),
            ("pgss_info", Some("f")),
            ("replay_pause_requested_access", Some("t")),
            ("replay_pause_state_access", Some("f")),
        ]
        .map(|(name, value)| (name.into(), value.map(str::to_owned))),
    )
}

#[tokio::test]
async fn existing_metadata_read_resolves_optional_function_oids_without_executing_them() {
    let mut reader = Reader { replies: [Ok(vec![row()])].into(), calls: vec![] };
    let metadata = read(&mut reader, "fixture.invalid", 5432).await.unwrap();
    assert_eq!(metadata.replay_pause_access, ReplayPauseAccess { requested: true, state: false });
    assert_eq!(reader.calls.len(), 1);
    assert!(reader.replies.is_empty());
    assert!(reader.calls[0].contains("to_regprocedure('pg_catalog.pg_is_wal_replay_paused()')"));
    assert!(reader.calls[0].contains("to_regprocedure('pg_catalog.pg_get_wal_replay_pause_state()')"));
    assert!(!reader.calls[0].contains("THEN pg_catalog.pg_"));
}

#[tokio::test]
async fn changed_pause_access_retires_only_the_standby_replication_read_not_an_unrelated_source() {
    let mut reader = Reader { replies: [Ok(vec![row()])].into(), calls: vec![] };
    let before = read(&mut reader, "fixture.invalid", 5432).await.unwrap();
    assert!(before.same_replication_source(&before));
    for access in
        [ReplayPauseAccess { requested: false, state: false }, ReplayPauseAccess { requested: true, state: true }]
    {
        let mut after = before.clone();
        after.replay_pause_access = access;
        assert!(before.same_source(&after), "unrelated datasets never call these optional functions");
        assert!(!before.same_replication_source(&after));
        let mut primary = before.clone();
        primary.standby = false;
        after.standby = false;
        assert!(primary.same_replication_source(&after), "primary collection has no pause-function dependency");
    }
    let mut changed_process = before.clone();
    changed_process.started_at = "2026-11-13T05:30:00Z".into();
    assert!(!before.same_replication_source(&changed_process));
}
