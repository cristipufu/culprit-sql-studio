use super::*;
use culprit_core::{ErrorCode, telemetry::CollectionStop};
use culprit_postgres::wire::{QueryRead, Row};
use std::collections::VecDeque;
struct Reader {
    replies: VecDeque<Result<Vec<Row>>>,
    calls: usize,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, _: &str, _: usize) -> Result<Vec<Row>> {
        self.calls += 1;
        self.replies.pop_front().expect("no native work beyond the owned probe")
    }
}
fn context() -> Row {
    Row::from_fields(
        [
            ("version", Some("PostgreSQL 15.2-YB-2025.2.0.0-b0".into())),
            ("compatibility", Some("150002".into())),
            ("database", Some("shop".into())),
            ("oid", Some("42".into())),
            ("now", Some("2020-01-01T06:00:00Z".into())),
            ("started", Some("2020-01-01T05:00:00Z".into())),
            ("global_views", Some("f".into())),
            ("statement_schema", None),
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
        let mut reader = Reader { replies: vec![Ok(vec![context()]), Err(stop.error())].into(), calls: 0 };
        let error = read_from(&mut reader).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.calls, 2);
        assert!(reader.replies.is_empty());
    }
}
#[tokio::test]
async fn optional_permission_absence_remains_unknown_but_unclassified_native_errors_are_not_swallowed() {
    for code in [ErrorCode::PermissionDenied, ErrorCode::Unsupported] {
        let mut reader =
            Reader { replies: vec![Ok(vec![context()]), Err(Error::new(code, "fixture"))].into(), calls: 0 };
        assert!(read_from(&mut reader).await.unwrap().node.is_none());
        assert_eq!(reader.calls, 2);
    }
    for code in [ErrorCode::Timeout, ErrorCode::ConnectionFailed, ErrorCode::QueryFailed, ErrorCode::InvalidInput] {
        let mut reader =
            Reader { replies: vec![Ok(vec![context()]), Err(Error::new(code, "fixture"))].into(), calls: 0 };
        assert_eq!(read_from(&mut reader).await.err().unwrap().code, code);
        assert_eq!(reader.calls, 2);
    }
}
