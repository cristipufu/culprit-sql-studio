use super::*;
use culprit_core::{ErrorCode, telemetry::CollectionStop};
use culprit_postgres::wire::{QueryRead, Row};
use std::collections::VecDeque;
struct Reader {
    replies: VecDeque<Result<Vec<Row>>>,
    calls: usize,
    sql: Vec<String>,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, _: usize) -> Result<Vec<Row>> {
        self.calls += 1;
        self.sql.push(sql.into());
        self.replies.pop_front().expect("no native work beyond the owned probe")
    }
}
fn context() -> Row {
    context_with_info(Some("f"))
}
fn context_with_info(info: Option<&str>) -> Row {
    Row::from_fields(
        [
            ("version", Some("PostgreSQL 15.2-YB-2025.2.0.0-b0".into())),
            ("compatibility", Some("150002".into())),
            ("database", Some("shop".into())),
            ("oid", Some("42".into())),
            ("now", Some("2020-01-01T06:00:00Z".into())),
            ("started", Some("2020-01-01T05:00:00Z".into())),
            ("global_views", Some("f".into())),
            ("statement_schema", Some("public".into())),
            ("statement_info", info.map(str::to_owned)),
            ("cluster_statements", Some("f".into())),
            ("metrics", Some("f".into())),
            ("rpc_metrics", Some("f".into())),
            ("role_oid", Some("10".into())),
            ("all_stats", Some("t".into())),
        ]
        .map(|(name, value)| (name.into(), value)),
    )
}
#[tokio::test]
async fn native_node_probe_preserves_typed_stops_instead_of_turning_them_into_optional_absence() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::TimedOut,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
        CollectionStop::SourceChanged,
    ] {
        let mut reader = Reader { replies: vec![Ok(vec![context()]), Err(stop.error())].into(), calls: 0, sql: vec![] };
        let error = read_from(&mut reader).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.calls, 2);
        assert!(reader.replies.is_empty());
    }
}
#[tokio::test]
async fn optional_permission_absence_remains_unknown_but_unclassified_native_errors_are_not_swallowed() {
    for code in [ErrorCode::PermissionDenied, ErrorCode::Unsupported] {
        let mut reader = Reader {
            replies: vec![Ok(vec![context()]), Err(Error::new(code, "fixture"))].into(),
            calls: 0,
            sql: vec![],
        };
        assert!(read_from(&mut reader).await.unwrap().node.is_none());
        assert_eq!(reader.calls, 2);
    }
    for code in [ErrorCode::Timeout, ErrorCode::ConnectionFailed, ErrorCode::QueryFailed, ErrorCode::InvalidInput] {
        let mut reader = Reader {
            replies: vec![Ok(vec![context()]), Err(Error::new(code, "fixture"))].into(),
            calls: 0,
            sql: vec![],
        };
        assert_eq!(read_from(&mut reader).await.err().unwrap().code, code);
        assert_eq!(reader.calls, 2);
    }
}

#[tokio::test]
async fn reset_capability_uses_native_extension_ownership_and_access_in_the_existing_probe() {
    for available in [false, true] {
        let mut reader = Reader {
            replies: [
                Ok(vec![context_with_info(Some(if available { "t" } else { "f" }))]),
                Ok(vec![Row::from_fields([("node".into(), Some("0123456789abcdef0123456789abcdef".into()))])]),
            ]
            .into(),
            calls: 0,
            sql: vec![],
        };
        let result = read_from(&mut reader).await.unwrap();
        assert_eq!(result.statement_info, available);
        assert_eq!(reader.calls, 2, "reset discovery adds no roundtrip");
        let sql = &reader.sql[0];
        for guard in [
            "cd.refobjid=e.oid AND cd.deptype='e'",
            "pd.refobjid=e.oid AND pd.deptype='e'",
            "rd.refclassid='pg_catalog.pg_proc'::regclass AND rd.refobjid=p.oid",
            "cr.rolsuper",
            "pr.rolsuper",
            "p.pronargs=0 AND NOT p.prosecdef",
            "l.lanname='c'",
            "has_schema_privilege(e.extnamespace,'USAGE')",
            "has_table_privilege(c.oid,'SELECT')",
            "has_function_privilege(p.oid,'EXECUTE')",
        ] {
            assert!(sql.contains(guard), "missing native reset capability guard {guard}");
        }
        assert!(!reader.sql.iter().any(|sql| sql.contains("FROM public.pg_stat_statements_info")));
    }
}

#[tokio::test]
async fn unknown_reset_capability_is_invalid_before_further_native_metadata_work() {
    for capability in [None, Some("unknown"), Some("")] {
        let mut reader = Reader { replies: [Ok(vec![context_with_info(capability)])].into(), calls: 0, sql: vec![] };
        assert!(read_from(&mut reader).await.is_err());
        assert_eq!(reader.calls, 1);
        assert!(reader.replies.is_empty());
    }
}
