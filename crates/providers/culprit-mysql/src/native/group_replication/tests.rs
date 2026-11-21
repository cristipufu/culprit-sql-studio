use super::*;
use crate::{native::error_log::context::tests as instance_rows, read::test_support::Read};
pub(crate) const GROUP: &str = "aaaaaaaa-0000-4000-8000-000000000001";
pub(crate) fn layout() -> Vec<Rec> {
    let mut fields = vec![
        (MEMBERS, "CHANNEL_NAME"),
        (MEMBERS, "MEMBER_ID"),
        (MEMBERS, "MEMBER_STATE"),
        (MEMBERS, "MEMBER_ROLE"),
        (MEMBERS, "MEMBER_VERSION"),
        (STATISTICS, "CHANNEL_NAME"),
        (STATISTICS, "MEMBER_ID"),
        (STATISTICS, "VIEW_ID"),
    ];
    fields.extend(Counter::ALL.map(|field| (STATISTICS, field.native())));
    fields
        .into_iter()
        .map(|(table, column)| {
            Rec::from_values(vec![("table_name", Value::from(table)), ("column_name", Value::from(column))])
        })
        .collect()
}
pub(crate) fn context_rows(at: &str) -> Vec<Result<Vec<Rec>>> {
    vec![
        Ok(vec![instance_rows::platform_at("8.4.7", true, at)]),
        Ok(vec![Rec::from_values(vec![
            ("server_uuid", Value::from(instance_rows::UUID)),
            ("plugin_status", Value::from("ACTIVE")),
            ("group_id", Value::from(GROUP)),
            ("observed_at", Value::from(at)),
        ])]),
        Ok(vec![instance_rows::process(10000, 100)]),
        Ok(layout()),
    ]
}
pub(crate) fn member() -> Rec {
    Rec::from_values(vec![
        ("observer_id", Value::from(instance_rows::UUID)),
        ("group_id", Value::from(GROUP)),
        ("member_id", Value::from(instance_rows::UUID)),
        ("channel", Value::from("group_replication_applier")),
        ("observed_at", Value::from("2026-11-13T10:00:00.123457Z")),
        ("member_state", Value::from("ONLINE")),
        ("member_role", Value::from("PRIMARY")),
        ("member_version", Value::from("8.4.7")),
    ])
}
pub(crate) fn statistics() -> Rec {
    Rec::from_values(vec![
        ("observer_id", Value::from(instance_rows::UUID)),
        ("group_id", Value::from(GROUP)),
        ("member_id", Value::from(instance_rows::UUID)),
        ("channel", Value::from("group_replication_applier")),
        ("observed_at", Value::from("2026-11-13T10:00:00.123457Z")),
        ("view_id", Value::from("view-original")),
        ("certification_queue", Value::UInt(0)),
        ("checked", Value::UInt(u64::MAX)),
        ("conflicts", Value::UInt(9007199254740993)),
        ("rows_validating", Value::NULL),
        ("applier_queue", Value::UInt(3)),
        ("remote_applied", Value::UInt(5)),
        ("local_proposed", Value::UInt(7)),
        ("local_rollback", Value::UInt(1)),
    ])
}
pub(crate) fn frame() -> Vec<Result<Vec<Rec>>> {
    let mut replies = context_rows("2026-11-13T10:00:00.123456Z");
    replies.extend([Ok(vec![member()]), Ok(vec![statistics()])]);
    replies.extend(context_rows("2026-11-13T10:00:00.123458Z"));
    replies
}
#[tokio::test]
async fn native_member_identity_role_and_exact_gauges_totals_keep_null_zero_and_wide_u64() {
    let mut reader = Read::with(context_rows("2026-11-13T10:00:00.123456Z"));
    let (context, _) = super::context(&mut reader).await.unwrap();
    let point = Kind::Members.decode(&member(), &context).unwrap();
    let MysqlGroupReplicationPoint::Member(member) = point else { panic!() };
    assert_eq!(member.role.as_deref(), Some("PRIMARY"));
    assert_eq!(member.state, "ONLINE");
    let MysqlGroupReplicationPoint::Statistics(stats) = Kind::Statistics.decode(&statistics(), &context).unwrap()
    else {
        panic!()
    };
    assert_eq!(stats.checked.as_deref(), Some("18446744073709551615"));
    assert_eq!(stats.conflicts.as_deref(), Some("9007199254740993"));
    assert_eq!(stats.certification_queue.as_deref(), Some("0"));
    assert_eq!(stats.rows_validating, None);
}
#[tokio::test]
async fn product_version_and_instrumentation_are_observed_before_mysql_plugin_or_layout_sql() {
    for (version, instrumented) in [("11.8.0-MariaDB", true), ("5.7.16", true), ("8.4.7", false)] {
        let mut reader = Read::with(vec![Ok(vec![instance_rows::platform(version, instrumented)])]);
        assert_eq!(super::context(&mut reader).await.err().unwrap().code, ErrorCode::Unsupported);
        assert_eq!(reader.queries.len(), 1);
    }
}
#[tokio::test]
async fn optional_native_layout_columns_are_not_compiled_and_never_filled_from_role_or_totals() {
    let mut rows = context_rows("2026-11-13T10:00:00.123456Z");
    let retained = layout()
        .into_iter()
        .filter(|row| {
            !matches!(
                text(row, "column_name").unwrap().as_str(),
                "MEMBER_ROLE" | "MEMBER_VERSION" | "COUNT_TRANSACTIONS_REMOTE_APPLIED"
            )
        })
        .collect();
    rows[3] = Ok(retained);
    let (context, _) = super::context(&mut Read::with(rows)).await.unwrap();
    let sql = context.layout.query(Kind::Members);
    assert!(sql.contains("NULL AS member_role"));
    assert!(!sql.contains("MEMBER_ROLE AS"));
    assert!(context.layout.query(Kind::Statistics).contains("NULL AS remote_applied"));
    for sql in [sql, context.layout.query(Kind::Statistics)] {
        assert!(sql.ends_with("LIMIT ?"));
        assert!(!sql.contains("JOIN"));
        assert!(!sql.contains("TRANSACTIONS_COMMITTED_ALL_MEMBERS"));
    }
}
#[tokio::test]
async fn foreign_observer_group_malformed_native_id_duplicate_points_or_fractional_counts_refuse() {
    let (context, _) = super::context(&mut Read::with(context_rows("2026-11-13T10:00:00.123456Z"))).await.unwrap();
    for (field, value) in [
        ("observer_id", Value::from(GROUP)),
        ("group_id", Value::from(instance_rows::UUID)),
        ("member_id", Value::from("not-uuid")),
        ("checked", Value::from("1.5")),
    ] {
        let row = statistics().with_fields(&[(field, value)]);
        assert!(Kind::Statistics.decode(&row, &context).is_err(), "{field}");
    }
    let mut reader = Read::with(vec![Ok(vec![member(), member()])]);
    assert!(super::read(&mut reader, &context, 128).await.is_err());
    assert_eq!(reader.queries.len(), 1);
}
#[tokio::test]
async fn disabled_or_unobserved_layout_is_unavailable_without_querying_native_tables() {
    let mut rows = context_rows("2026-11-13T10:00:00.123456Z");
    rows[3] = Ok(vec![]);
    let (context, _) = super::context(&mut Read::with(rows)).await.unwrap();
    let mut reader = Read::default();
    let observed = super::read(&mut reader, &context, 128).await.unwrap();
    assert!(observed.rows.is_empty());
    assert!(reader.queries.is_empty());
    assert!(observed.coverage.iter().all(|source| source.state == culprit_core::SourceState::Unavailable));
}
#[tokio::test]
async fn statistics_select_denial_keeps_native_members_but_transport_timeout_and_unconfirmed_work_are_terminal() {
    let (context, _) = super::context(&mut Read::with(context_rows("2026-11-13T10:00:00.123456Z"))).await.unwrap();
    let mut reader = Read::with(vec![Ok(vec![member()]), Err(Error::new(ErrorCode::PermissionDenied, "denied"))]);
    let observed = super::read(&mut reader, &context, 128).await.unwrap();
    assert_eq!(observed.rows.len(), 1);
    assert_eq!(observed.coverage[0].state, culprit_core::SourceState::Collected);
    assert_eq!(observed.coverage[1].state, culprit_core::SourceState::Unavailable);
    for error in [CollectionStop::TimedOut.error(), CollectionStop::CancellationUnconfirmed.error()] {
        let mut reader = Read::with(vec![Ok(vec![member()]), Err(error)]);
        assert!(super::read(&mut reader, &context, 128).await.is_err());
        assert_eq!(reader.queries.len(), 2);
    }
}

#[tokio::test]
async fn bounded_native_table_lookahead_marks_each_retained_prefix_partial_without_a_complete_group_claim() {
    let (context, _) = super::context(&mut Read::with(context_rows("2026-11-13T10:00:00.123456Z"))).await.unwrap();
    let other = "cccccccc-0000-4000-8000-000000000003";
    let mut reader = Read::with(vec![
        Ok(vec![member(), member().with_fields(&[("member_id", Value::from(other))])]),
        Ok(vec![statistics(), statistics().with_fields(&[("member_id", Value::from(other))])]),
    ]);
    let observed = super::read(&mut reader, &context, 1).await.unwrap();
    assert_eq!(observed.rows.len(), 2);
    assert!(observed.coverage.iter().all(|source| source.state == culprit_core::SourceState::Partial
        && !source.complete_scope
        && source.records == 1));
    assert!(reader.queries.iter().all(|(_, parameters, limit)| *limit == 2 && parameters == &vec![Value::UInt(2)]));
}
